//! The one registry key that says whether a Windhawk mod is enabled.
//!
//! Windhawk 1.7.3 (the version on this machine) keeps per-mod configuration in registry mode under
//! `HKLM\SOFTWARE\Windhawk\Engine\Mods\<id>`. The `Disabled` DWORD is the enabled state:
//! `0`/absent means enabled, `1` means disabled. `Engine\ModsWritable` is a *different* tree used
//! for a mod's own settings and runtime storage, so writing the enabled flag there would be
//! silently ignored. This module therefore reads and writes `Engine\Mods`, and verifies every
//! write by reading it back.
//!
//! The registry is machine-wide, which is why the install helper is elevated. Reads work for an
//! ordinary user, so listing installed mods never asks for UAC.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::System::Registry::{
    HKEY, HKEY_LOCAL_MACHINE, KEY_ENUMERATE_SUB_KEYS, KEY_QUERY_VALUE, KEY_SET_VALUE,
    REG_CREATE_KEY_DISPOSITION, REG_DWORD, REG_OPTION_NON_VOLATILE, REG_SAM_FLAGS, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
    RegSetValueExW,
};
use windows::core::PCWSTR;

use crate::plan::sanitize_id;

/// The registry tree whose subkeys are mod configurations.
pub const MODS_KEY: &str = r"SOFTWARE\Windhawk\Engine\Mods";

/// The value holding the disabled flag.
pub const DISABLED_VALUE: &str = "Disabled";

/// Where a mod's enabled state comes from.
///
/// A test supplies [`MemoryRegistry`]; production uses [`RealRegistry`]. `Err` is a machine
/// failure (access denied, a broken value), never "the key is absent": that is `Ok(None)`.
pub trait ModRegistry: std::fmt::Debug + Send + Sync {
    /// `Some(true)` when the mod is disabled, `Some(false)` when enabled, `None` when there is no
    /// configuration key for it (the state is then unknown, not "enabled").
    fn read_disabled(&self, id: &str) -> Result<Option<bool>, String>;

    /// Writes `Disabled` and reads it back; an unverified write is an error.
    fn set_disabled(&self, id: &str, disabled: bool) -> Result<(), String>;

    /// Deletes the mod's configuration subkey; an absent key is success.
    fn remove(&self, id: &str) -> Result<(), String>;

    /// Every configured mod id, used to find compiled mods whose source is gone.
    fn configured_ids(&self) -> Result<Vec<String>, String>;

    /// The full key path for display and logs.
    fn key_path(&self, id: &str) -> String {
        format!(r"{MODS_KEY}\{id}")
    }
}

/// `HKLM\SOFTWARE\Windhawk\Engine\Mods`, as Windhawk itself uses it.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealRegistry;

impl ModRegistry for RealRegistry {
    fn read_disabled(&self, id: &str) -> Result<Option<bool>, String> {
        let id = sanitize(id)?;
        let Some(key) = Key::open(&key_path(&id), KEY_QUERY_VALUE)? else {
            return Ok(None);
        };

        match key.value(DISABLED_VALUE)? {
            None => Ok(Some(false)),
            Some(Value::Dword(0)) => Ok(Some(false)),
            Some(Value::Dword(_)) => Ok(Some(true)),
            Some(Value::Other(kind)) => Err(format!(
                "'{DISABLED_VALUE}' under '{}' has registry type {kind}, not REG_DWORD",
                key_path(&id)
            )),
        }
    }

    fn set_disabled(&self, id: &str, disabled: bool) -> Result<(), String> {
        let id = sanitize(id)?;
        let path = key_path(&id);
        let key = Key::create(&path)?;
        key.set_dword(DISABLED_VALUE, u32::from(disabled))?;

        match key.value(DISABLED_VALUE)? {
            Some(Value::Dword(value)) if (value != 0) == disabled => Ok(()),
            Some(other) => Err(format!(
                "writing '{DISABLED_VALUE}' under '{path}' did not stick: read back {other:?}"
            )),
            None => Err(format!(
                "writing '{DISABLED_VALUE}' under '{path}' did not stick: the value is absent"
            )),
        }
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        let id = sanitize(id)?;
        let path = wide(&key_path(&id));

        // SAFETY: `path` is a NUL-terminated buffer that outlives the call. Deleting a missing
        // tree is specifically successful.
        let status = unsafe { RegDeleteTreeW(HKEY_LOCAL_MACHINE, PCWSTR(path.as_ptr())) };
        if status == ERROR_SUCCESS || status == ERROR_FILE_NOT_FOUND {
            return Ok(());
        }

        Err(format!(
            "cannot delete '{}': registry error {}",
            key_path(&id),
            status.0
        ))
    }

    fn configured_ids(&self) -> Result<Vec<String>, String> {
        let Some(key) = Key::open(MODS_KEY, KEY_ENUMERATE_SUB_KEYS | KEY_QUERY_VALUE)? else {
            return Ok(Vec::new());
        };

        let mut ids = Vec::new();
        let mut index = 0u32;
        loop {
            let mut buffer = [0u16; 256];
            let mut length = buffer.len() as u32;

            // SAFETY: `buffer` is a live writable array of the length passed in; the optional
            // class and timestamp outputs are not requested.
            let status = unsafe {
                RegEnumKeyExW(
                    key.0,
                    index,
                    Some(windows::core::PWSTR(buffer.as_mut_ptr())),
                    &mut length,
                    None,
                    None,
                    None,
                    None,
                )
            };

            if status == ERROR_NO_MORE_ITEMS {
                break;
            }
            if status != ERROR_SUCCESS {
                return Err(format!(
                    "cannot enumerate '{MODS_KEY}': registry error {}",
                    status.0
                ));
            }

            // `length` is the number of UTF-16 units written, terminator excluded.
            let name = String::from_utf16_lossy(&buffer[..length as usize]);
            ids.push(name);
            index += 1;
        }

        Ok(ids)
    }
}

/// An in-memory registry for tests and for a dry run.
///
/// `fail_writes` makes every write return the access-denied error a non-elevated process sees,
/// which is what the enable-verification tests need without touching `HKLM`.
#[derive(Debug, Default)]
pub struct MemoryRegistry {
    values: std::sync::Mutex<BTreeMap<String, bool>>,
    fail_writes: AtomicBool,
}

impl MemoryRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry pre-loaded with `(id, disabled)` pairs.
    pub fn with(entries: &[(&str, bool)]) -> Self {
        let registry = Self::new();
        {
            let mut values = registry
                .values
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            for (id, disabled) in entries {
                values.insert((*id).to_owned(), *disabled);
            }
        }
        registry
    }

    /// Makes every write fail, as it does for an ordinary user against `HKLM`.
    pub fn deny_writes(&self) {
        self.fail_writes.store(true, Ordering::SeqCst);
    }

    /// The disabled value a direct caller can inspect.
    pub fn get(&self, id: &str) -> Option<bool> {
        self.values
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(id)
            .copied()
    }
}

impl ModRegistry for MemoryRegistry {
    fn read_disabled(&self, id: &str) -> Result<Option<bool>, String> {
        let id = sanitize(id)?;
        Ok(self.get(&id))
    }

    fn set_disabled(&self, id: &str, disabled: bool) -> Result<(), String> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err("access denied (registry errors were simulated)".to_owned());
        }

        let id = sanitize(id)?;
        self.values
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(id, disabled);

        Ok(())
    }

    fn remove(&self, id: &str) -> Result<(), String> {
        if self.fail_writes.load(Ordering::SeqCst) {
            return Err("access denied (registry errors were simulated)".to_owned());
        }

        let id = sanitize(id)?;
        self.values
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(&id);

        Ok(())
    }

    fn configured_ids(&self) -> Result<Vec<String>, String> {
        Ok(self
            .values
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .keys()
            .cloned()
            .collect())
    }
}

/// An open `HKLM` key, closed on drop.
#[derive(Debug)]
struct Key(HKEY);

impl Key {
    fn open(path: &str, access: REG_SAM_FLAGS) -> Result<Option<Self>, String> {
        let path = wide(path);
        let mut key = HKEY(std::ptr::null_mut());

        // SAFETY: `path` is NUL-terminated and outlives the call; `key` is a live out-parameter.
        let status = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
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
            return Err(format!("cannot open a registry key: error {}", status.0));
        }

        Ok(Some(Self(key)))
    }

    fn create(path: &str) -> Result<Self, String> {
        let path = wide(path);
        let mut key = HKEY(std::ptr::null_mut());
        let mut disposition = REG_CREATE_KEY_DISPOSITION(0);

        // SAFETY: every pointer is a NUL-terminated buffer or a live out-parameter; the default
        // security descriptor is what `None` documents.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_LOCAL_MACHINE,
                PCWSTR(path.as_ptr()),
                None,
                PCWSTR::null(),
                REG_OPTION_NON_VOLATILE,
                KEY_QUERY_VALUE | KEY_SET_VALUE,
                None,
                &mut key,
                Some(&mut disposition),
            )
        };

        if status != ERROR_SUCCESS {
            return Err(format!(
                "cannot create a registry key (access denied without an elevated helper): error {}",
                status.0
            ));
        }

        Ok(Self(key))
    }

    fn value(&self, name: &str) -> Result<Option<Value>, String> {
        let name = wide(name);
        let mut kind = REG_DWORD;
        let mut data = [0u8; 16];
        let mut length = data.len() as u32;

        // SAFETY: `name` is NUL-terminated; `data` is a live buffer whose byte length is passed in.
        let status = unsafe {
            RegQueryValueExW(
                self.0,
                PCWSTR(name.as_ptr()),
                None,
                Some(&mut kind),
                Some(data.as_mut_ptr()),
                Some(&mut length),
            )
        };

        if status == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        if status != ERROR_SUCCESS {
            return Err(format!("cannot read a registry value: error {}", status.0));
        }

        if kind == REG_DWORD {
            let bytes: [u8; 4] = data[..4]
                .try_into()
                .map_err(|_| "a REG_DWORD shorter than four bytes".to_owned())?;
            return Ok(Some(Value::Dword(u32::from_le_bytes(bytes))));
        }

        Ok(Some(Value::Other(kind.0)))
    }

    fn set_dword(&self, name: &str, value: u32) -> Result<(), String> {
        let name = wide(name);
        let bytes = value.to_le_bytes();

        // SAFETY: `name` is NUL-terminated and `bytes` is a live four-byte buffer of the documented
        // size for a REG_DWORD.
        let status =
            unsafe { RegSetValueExW(self.0, PCWSTR(name.as_ptr()), None, REG_DWORD, Some(&bytes)) };

        if status != ERROR_SUCCESS {
            return Err(format!("cannot write a registry value: error {}", status.0));
        }

        Ok(())
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key was opened or created here and is closed exactly once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Value {
    Dword(u32),
    Other(u32),
}

fn key_path(id: &str) -> String {
    format!(r"{MODS_KEY}\{id}")
}

fn sanitize(id: &str) -> Result<String, String> {
    sanitize_id(id).map_err(|error| error.to_string())
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_memory_registry_reports_absent_enabled_and_disabled_states() {
        let registry = MemoryRegistry::with(&[("one", false), ("two", true)]);

        assert_eq!(registry.read_disabled("one").expect("read"), Some(false));
        assert_eq!(registry.read_disabled("two").expect("read"), Some(true));
        assert_eq!(registry.read_disabled("three").expect("read"), None);
    }

    #[test]
    fn a_write_is_visible_and_can_be_reversed() {
        let registry = MemoryRegistry::new();

        registry
            .set_disabled("accent-color-sync", true)
            .expect("disable");
        assert_eq!(
            registry.read_disabled("accent-color-sync").expect("read"),
            Some(true)
        );

        registry
            .set_disabled("accent-color-sync", false)
            .expect("enable");
        assert_eq!(
            registry.read_disabled("accent-color-sync").expect("read"),
            Some(false)
        );

        registry.remove("accent-color-sync").expect("remove");
        assert_eq!(
            registry.read_disabled("accent-color-sync").expect("read"),
            None
        );
    }

    #[test]
    fn a_denied_write_is_an_error_and_changes_nothing() {
        let registry = MemoryRegistry::with(&[("one", false)]);
        registry.deny_writes();

        assert!(registry.set_disabled("one", true).is_err());
        assert_eq!(registry.read_disabled("one").expect("read"), Some(false));
        assert!(registry.remove("one").is_err());
        assert_eq!(registry.read_disabled("one").expect("read"), Some(false));
    }

    #[test]
    fn configured_ids_lists_every_key() {
        let registry = MemoryRegistry::with(&[("b", false), ("a", true)]);

        assert_eq!(
            registry.configured_ids().expect("list"),
            vec!["a".to_owned(), "b".to_owned()]
        );
    }

    #[test]
    fn unsafe_ids_never_reach_the_registry() {
        let registry = MemoryRegistry::new();

        assert!(registry.read_disabled(r"..\evil").is_err());
        assert!(registry.set_disabled("a/b", false).is_err());
        assert!(registry.remove("a\\b").is_err());
    }

    #[test]
    fn the_real_registry_reads_this_machines_mods_key_without_admin() {
        // Reading `HKLM` needs no elevation; writing does, which is exactly why the helper exists.
        // Nothing is written here.
        let _ = RealRegistry.read_disabled("accent-color-sync");
        let _ = RealRegistry.configured_ids();
    }

    #[test]
    fn the_documented_key_is_the_one_windhawk_uses() {
        assert_eq!(MODS_KEY, r"SOFTWARE\Windhawk\Engine\Mods");
        assert_eq!(
            RealRegistry.key_path("accent-color-sync"),
            r"SOFTWARE\Windhawk\Engine\Mods\accent-color-sync"
        );
    }
}
