//! The Win32 edge of the replacement mode: registry values, the settings broadcast and the desktop.
//!
//! Everything that can be decided without the operating system lives in the parent module; this file
//! only performs the reads and writes. It is behind [`RegistryAccess`] so the whole mode - plans,
//! state file, idempotence - is exercised by tests without touching `HKCU` or the user's desktop.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, HWND, LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_BINARY, REG_DWORD,
    REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, REG_VALUE_TYPE, RegCloseKey, RegCreateKeyExW,
    RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
};
use windows::Win32::UI::Shell::{SHCNE_ASSOCCHANGED, SHCNF_IDLIST, SHChangeNotify};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, FindWindowW, HWND_BROADCAST, SMTO_ABORTIFHUNG, SW_HIDE, SW_SHOW,
    SendMessageTimeoutW, ShowWindow, WM_SETTINGCHANGE,
};
use windows::core::{PCWSTR, w};

use super::{RawValue, Toggle};

/// The `WM_SETTINGCHANGE` area Explorer understands as "the taskbar changed".
pub const SETTINGS_AREA: &str = "TraySettings";

/// How long the settings broadcast waits for one window before skipping it.
const BROADCAST_TIMEOUT_MS: u32 = 1_000;

/// The registry reads and writes the replacement mode needs.
///
/// Implemented by [`RealRegistry`] in production and by [`MemoryRegistry`] in the tests. `notify`
/// carries the side effects a bare registry write does not have: the `WM_SETTINGCHANGE` broadcast
/// and, for the desktop icons, the `SHELLDLL_DefView` switch and the shell refresh.
pub trait RegistryAccess: fmt::Debug {
    /// The current value, or [`RawValue::Absent`] when the key or value does not exist.
    fn read(&self, key: &str, value: &str) -> Result<RawValue, String>;

    /// Writes `raw`, creating the key when Explorer has not yet.
    fn write(&self, key: &str, value: &str, raw: &RawValue) -> Result<(), String>;

    /// Deletes the value; a value that is already gone is not an error.
    fn delete(&self, key: &str, value: &str) -> Result<(), String>;

    /// Tells the rest of the session that the value changed.
    fn notify(&self, toggle: Toggle, hidden: bool);
}

/// The real `HKCU` and the real desktop.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealRegistry;

impl RegistryAccess for RealRegistry {
    fn read(&self, key: &str, value: &str) -> Result<RawValue, String> {
        // A missing key means Explorer has never written the value, which is exactly "absent".
        let Some(key) = Key::open(key, KEY_QUERY_VALUE)? else {
            return Ok(RawValue::Absent);
        };

        let name = wide(value);
        let mut size = 0u32;
        // SAFETY: `key` is an open key, `name` a NUL-terminated buffer, and a null data pointer asks
        // for the size only, which is what the two-call pattern requires.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                None,
                None,
                Some(&mut size),
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(RawValue::Absent);
        }
        if status != ERROR_SUCCESS {
            return Err(format!("size query failed with {}", status.0));
        }

        if size == 0 {
            return Ok(RawValue::Binary { value: Vec::new() });
        }

        let mut kind = REG_VALUE_TYPE::default();
        let mut bytes = vec![0u8; size as usize];
        // SAFETY: `bytes` is allocated with exactly `size` bytes and the same size is passed in, so
        // the call cannot write past it; `kind` is a valid out-parameter.
        let status = unsafe {
            RegQueryValueExW(
                key.0,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(bytes.as_mut_ptr()),
                Some(&mut size),
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("read failed with {}", status.0));
        }

        bytes.truncate(size as usize);

        match kind {
            REG_DWORD if bytes.len() >= 4 => {
                let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
                Ok(RawValue::Dword { value })
            }
            REG_BINARY => Ok(RawValue::Binary { value: bytes }),
            other => Err(format!("unexpected value type {}", other.0)),
        }
    }

    fn write(&self, key: &str, value: &str, raw: &RawValue) -> Result<(), String> {
        let (kind, bytes) = match raw {
            RawValue::Dword { value } => (REG_DWORD, value.to_le_bytes().to_vec()),
            RawValue::Binary { value } => (REG_BINARY, value.clone()),
            RawValue::Absent => return Err("cannot write an absent value".to_owned()),
        };

        let key = Key::create(key)?;
        let name = wide(value);
        // SAFETY: `key` is an open key, `name` a NUL-terminated buffer and `bytes` a live slice whose
        // length is passed alongside it.
        let status =
            unsafe { RegSetValueExW(key.0, PCWSTR(name.as_ptr()), None, kind, Some(&bytes)) };
        if status != ERROR_SUCCESS {
            return Err(format!("write failed with {}", status.0));
        }

        Ok(())
    }

    fn delete(&self, key: &str, value: &str) -> Result<(), String> {
        let Some(key) = Key::open(key, KEY_SET_VALUE)? else {
            return Ok(());
        };

        let name = wide(value);
        // SAFETY: `key` is an open key and `name` is a NUL-terminated buffer.
        let status = unsafe { RegDeleteValueW(key.0, PCWSTR(name.as_ptr())) };
        if status == ERROR_FILE_NOT_FOUND || status == ERROR_SUCCESS {
            return Ok(());
        }

        Err(format!("delete failed with {}", status.0))
    }

    fn notify(&self, toggle: Toggle, hidden: bool) {
        broadcast_change();
        if toggle == Toggle::DesktopIcons {
            show_desktop_icons(!hidden);
            refresh_desktop();
        }
    }
}

/// An in-memory registry with the same semantics, for tests and for a dry run.
///
/// Reads of a value that was never written return [`RawValue::Absent`], writes are counted (so a
/// test can prove a second enable writes nothing) and broadcasts are recorded instead of sent.
#[derive(Debug, Default)]
pub struct MemoryRegistry {
    values: Mutex<BTreeMap<(String, String), RawValue>>,
    notifications: Mutex<Vec<(Toggle, bool)>>,
    writes: AtomicUsize,
    deletes: AtomicUsize,
}

impl MemoryRegistry {
    /// The current value of `toggle`, as the next `read` would return it.
    pub fn get(&self, toggle: Toggle) -> RawValue {
        let descriptor = toggle.descriptor();
        self.get_raw(descriptor.key, descriptor.value)
    }

    /// The current value of an arbitrary key and value.
    pub fn get_raw(&self, key: &str, value: &str) -> RawValue {
        self.lock_values()
            .get(&(key.to_owned(), value.to_owned()))
            .cloned()
            .unwrap_or(RawValue::Absent)
    }

    /// Seeds a value the mode is about to change.
    pub fn set(&self, toggle: Toggle, raw: RawValue) {
        let descriptor = toggle.descriptor();
        self.lock_values().insert(
            (descriptor.key.to_owned(), descriptor.value.to_owned()),
            raw,
        );
    }

    /// Seeds an arbitrary value.
    pub fn set_raw(&self, key: &str, value: &str, raw: RawValue) {
        self.lock_values()
            .insert((key.to_owned(), value.to_owned()), raw);
    }

    /// Every notification the mode produced, in order.
    pub fn notifications(&self) -> Vec<(Toggle, bool)> {
        self.lock_notifications().clone()
    }

    /// How many writes reached the registry.
    pub fn writes(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    /// How many deletes reached the registry.
    pub fn deletes(&self) -> usize {
        self.deletes.load(Ordering::SeqCst)
    }

    /// The value map lock, poisoned or not: a panic in a test must not hide the data.
    fn lock_values(&self) -> std::sync::MutexGuard<'_, BTreeMap<(String, String), RawValue>> {
        self.values
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The notification lock, poisoned or not.
    fn lock_notifications(&self) -> std::sync::MutexGuard<'_, Vec<(Toggle, bool)>> {
        self.notifications
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

impl RegistryAccess for MemoryRegistry {
    fn read(&self, key: &str, value: &str) -> Result<RawValue, String> {
        Ok(self.get_raw(key, value))
    }

    fn write(&self, key: &str, value: &str, raw: &RawValue) -> Result<(), String> {
        if matches!(raw, RawValue::Absent) {
            return Err("cannot write an absent value".to_owned());
        }

        self.writes.fetch_add(1, Ordering::SeqCst);
        self.set_raw(key, value, raw.clone());

        Ok(())
    }

    fn delete(&self, key: &str, value: &str) -> Result<(), String> {
        self.deletes.fetch_add(1, Ordering::SeqCst);
        self.lock_values()
            .remove(&(key.to_owned(), value.to_owned()));

        Ok(())
    }

    fn notify(&self, toggle: Toggle, hidden: bool) {
        self.lock_notifications().push((toggle, hidden));
    }
}

/// An open registry key, closed on drop.
#[derive(Debug)]
struct Key(HKEY);

impl Key {
    fn open(path: &str, access: REG_SAM_FLAGS) -> Result<Option<Self>, String> {
        let path = wide(path);
        let mut key = HKEY(std::ptr::null_mut());

        // SAFETY: `path` is a NUL-terminated buffer and `key` a valid out-parameter.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                access,
                &mut key,
            )
        };
        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(format!("open failed with {}", status.0));
        }

        Ok(Some(Self(key)))
    }

    fn create(path: &str) -> Result<Self, String> {
        let path = wide(path);
        let mut key = HKEY(std::ptr::null_mut());

        // SAFETY: `path` is a NUL-terminated buffer and `key` a valid out-parameter; the default
        // security descriptor is what `RegCreateKeyExW` documents for `None`.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR(path.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                None,
                &mut key,
                None,
            )
        };
        if status != ERROR_SUCCESS {
            return Err(format!("create failed with {}", status.0));
        }

        Ok(Self(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened or created by this module and is closed exactly once, here.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

/// Tells every window that the taskbar settings changed.
///
/// A broadcast nobody hears is normal on a machine without Explorer, so the result is ignored.
fn broadcast_change() {
    let area = wide(SETTINGS_AREA);

    // SAFETY: the window is `HWND_BROADCAST`, the parameter is a NUL-terminated UTF-16 buffer that
    // outlives the call, and the timeout keeps a hung window from blocking the supervisor.
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(area.as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            BROADCAST_TIMEOUT_MS,
            None,
        );
    }
}

/// Shows or hides the desktop icon list.
///
/// On Windows 11 24H2 the icon list is a `SysListView32` inside `SHELLDLL_DefView`, and the view
/// itself also paints the wallpaper: hiding the view leaves the desktop black. The list is therefore
/// hidden on its own, and the view is only hidden when it has no list child to hide.
fn show_desktop_icons(show: bool) {
    let Some(view) = desktop_view() else {
        return;
    };

    if show {
        // SAFETY: `view` and `list` (when present) are windows this session owns and the command is
        // the documented show one.
        unsafe {
            let _ = ShowWindow(view, SW_SHOW);
            if let Some(list) = desktop_list(view) {
                let _ = ShowWindow(list, SW_SHOW);
            }
        }
        return;
    }

    let target = desktop_list(view).unwrap_or(view);

    // SAFETY: `target` is a window this session owns and the command is the documented hide one.
    unsafe {
        let _ = ShowWindow(target, SW_HIDE);
    }
}

/// The icon list inside `view`, when this build has one.
fn desktop_list(view: HWND) -> Option<HWND> {
    // SAFETY: `view` is a live window and the class name is a static NUL-terminated string.
    unsafe { FindWindowExW(Some(view), None, w!("SysListView32"), PCWSTR::null()) }.ok()
}

/// The window that draws the desktop icons.
///
/// It lives under `Progman` on most builds and under a `WorkerW` on others, so both are tried.
fn desktop_view() -> Option<HWND> {
    // SAFETY: both class names are static NUL-terminated strings.
    let progman = unsafe { FindWindowW(w!("Progman"), PCWSTR::null()) }.ok()?;
    // SAFETY: the parent was returned by the call above and both names are static.
    if let Ok(view) =
        unsafe { FindWindowExW(Some(progman), None, w!("SHELLDLL_DefView"), PCWSTR::null()) }
    {
        return Some(view);
    }

    let mut worker: Option<HWND> = None;
    loop {
        // SAFETY: every input is either a window returned by the previous iteration or a static
        // class name.
        let next = unsafe { FindWindowExW(None, worker, w!("WorkerW"), PCWSTR::null()) }.ok()?;
        // SAFETY: `next` is a live top-level window and the child class name is static.
        if let Ok(view) =
            unsafe { FindWindowExW(Some(next), None, w!("SHELLDLL_DefView"), PCWSTR::null()) }
        {
            return Some(view);
        }

        worker = Some(next);
    }
}

/// Tells the shell that the desktop contents changed, which is what makes the icon switch appear.
fn refresh_desktop() {
    // SAFETY: `SHCNE_ASSOCCHANGED` with an id list and no items is the documented "everything on the
    // desktop may have changed" notification.
    unsafe {
        SHChangeNotify(SHCNE_ASSOCCHANGED, SHCNF_IDLIST, None, None);
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_registry_behaves_like_the_real_one() {
        let registry = MemoryRegistry::default();
        assert_eq!(registry.get(Toggle::Widgets), RawValue::Absent);

        registry
            .write("k", "v", &RawValue::Dword { value: 7 })
            .expect("write");
        assert_eq!(registry.get_raw("k", "v"), RawValue::Dword { value: 7 });
        assert_eq!(registry.writes(), 1);

        registry.delete("k", "v").expect("delete");
        assert_eq!(registry.get_raw("k", "v"), RawValue::Absent);
        assert_eq!(registry.deletes(), 1);

        registry
            .write("k", "v", &RawValue::Absent)
            .expect_err("an absent value is not writable");
    }

    #[test]
    fn memory_notifications_are_recorded_in_order() {
        let registry = MemoryRegistry::default();

        registry.notify(Toggle::DesktopIcons, true);
        registry.notify(Toggle::DesktopIcons, false);

        assert_eq!(
            registry.notifications(),
            vec![(Toggle::DesktopIcons, true), (Toggle::DesktopIcons, false)]
        );
    }
}
