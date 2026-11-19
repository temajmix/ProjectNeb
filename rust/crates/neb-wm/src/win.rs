//! The Win32 side of the manager: window facts, the public event hooks and [`WindowOps`].
//!
//! Nothing in this module injects code into another process or subclasses another window. Windows
//! it does not own are read through documented queries (`GetWindowRect`, `GetWindowLongPtrW`,
//! `DwmGetWindowAttribute`) and changed through documented requests (`SetWindowPos`,
//! `ShowWindow`, `DwmSetWindowAttribute`). The only hooks are `SetWinEventHook` with
//! `WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS`, which is the public accessibility mechanism
//! every window manager on Windows uses.

use std::ffi::c_void;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute, DwmSetWindowAttribute};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, HMONITOR, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromPoint,
    MonitorFromWindow,
};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::Accessibility::{HWINEVENTHOOK, SetWinEventHook, UnhookWinEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND, GA_ROOT, GW_OWNER,
    GWL_EXSTYLE, GWL_STYLE, GetAncestor, GetClassNameW, GetSystemMetrics, GetWindow,
    GetWindowLongPtrW, GetWindowRect, GetWindowTextLengthW, GetWindowTextW,
    GetWindowThreadProcessId, IsIconic, IsWindow, IsWindowVisible, SM_CXSCREEN, SM_CYSCREEN,
    SW_HIDE, SW_SHOWNOACTIVATE, SWP_ASYNCWINDOWPOS, SWP_NOACTIVATE, SWP_NOZORDER,
    SetForegroundWindow, SetWindowPos, ShowWindow, WINEVENT_OUTOFCONTEXT, WINEVENT_SKIPOWNPROCESS,
    WS_CAPTION, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_SIZEBOX, WS_THICKFRAME,
};
use windows::core::{BOOL, PCWSTR, PWSTR};

use crate::decorations::{self, DecorationPlan, DecorationReadback};
use crate::engine::WindowOps;
use crate::fullscreen::{self, ForegroundFacts};
use crate::model::WindowId;
use crate::rect::Rect;
use crate::rules::WindowFacts;

/// Longest image path `QueryFullProcessImageNameW` is asked for.
const PATH_UNITS: usize = 1024;

/// Longest window title the manager keeps.
const TITLE_UNITS: usize = 512;

/// Longest class name a window may have.
const CLASS_UNITS: usize = 256;

/// The public event range the manager subscribes to: every top-level show, hide, destroy and move.
pub const OBJECT_EVENT_RANGE: (u32, u32) = (EVENT_OBJECT_SHOW, EVENT_OBJECT_LOCATIONCHANGE);

/// The hooks a running manager owns; dropping the value unhooks them.
#[derive(Debug)]
pub struct Hooks {
    object: HWINEVENTHOOK,
    foreground: HWINEVENTHOOK,
}

impl Hooks {
    /// Installs the show/hide/destroy/location hook and the foreground hook.
    ///
    /// # Safety
    ///
    /// `callback` must be a valid `WINEVENTPROC` that stays alive for as long as the hooks do —
    /// which for a plain function pointer in a binary is always true.
    pub unsafe fn install(
        callback: windows::Win32::UI::Accessibility::WINEVENTPROC,
    ) -> Result<Self, String> {
        let flags = WINEVENT_OUTOFCONTEXT | WINEVENT_SKIPOWNPROCESS;

        // SAFETY: the range is the documented object range, no DLL is injected
        // (`hmodwineventproc` is null), and the hook is out-of-context so the callback runs on this
        // thread during message dispatch, never in another process.
        let object = unsafe {
            SetWinEventHook(
                OBJECT_EVENT_RANGE.0,
                OBJECT_EVENT_RANGE.1,
                None,
                callback,
                0,
                0,
                flags,
            )
        };

        if object.is_invalid() {
            return Err("cannot install the object event hook".to_owned());
        }

        // SAFETY: as above, for the single-value foreground event.
        let foreground = unsafe {
            SetWinEventHook(
                EVENT_SYSTEM_FOREGROUND,
                EVENT_SYSTEM_FOREGROUND,
                None,
                callback,
                0,
                0,
                flags,
            )
        };

        if foreground.is_invalid() {
            // SAFETY: `object` belongs to this value and is released exactly once.
            unsafe {
                let _ = UnhookWinEvent(object);
            }
            return Err("cannot install the foreground event hook".to_owned());
        }

        Ok(Self { object, foreground })
    }
}

impl Drop for Hooks {
    fn drop(&mut self) {
        // SAFETY: both hooks are owned by this value and are released exactly once, here.
        unsafe {
            let _ = UnhookWinEvent(self.object);
            let _ = UnhookWinEvent(self.foreground);
        }
    }
}

/// A window the manager could manage, with the facts rules and fullscreen filtering need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// The handle.
    pub id: WindowId,
    /// Executable, class and title.
    pub facts: WindowFacts,
    /// Rectangle at the moment the candidate was described.
    pub bounds: Rect,
}

/// Describes `id` as a management candidate, with the reason when it is not one.
///
/// This is the filter the task lists, in the order that avoids the most work: top-level and visible
/// first, then the shell surfaces, then style, then the queries that cost a DWM or process call.
/// The reason is a short static string, so a live desktop can be diagnosed without a debugger.
pub fn inspect(id: WindowId) -> Result<Candidate, &'static str> {
    let hwnd = hwnd(id);
    if id.is_null() {
        return Err("no handle");
    }

    // SAFETY: every call below only reads state of a handle that is checked to be a live window.
    unsafe {
        if !IsWindow(Some(hwnd)).as_bool() {
            return Err("not a window");
        }

        // A top-level window is its own root and has no owner; a dialog or a tool palette is not
        // something a tiler should move behind its parent.
        if GetAncestor(hwnd, GA_ROOT) != hwnd {
            return Err("not top-level");
        }
        if GetWindow(hwnd, GW_OWNER).is_ok() {
            return Err("owned by another window");
        }

        if !IsWindowVisible(hwnd).as_bool() || IsIconic(hwnd).as_bool() {
            return Err("hidden or minimised");
        }

        if GetWindowTextLengthW(hwnd) <= 0 {
            return Err("no title");
        }

        let class = class_of(hwnd);
        if class.is_empty() || fullscreen::is_shell_class(&class) {
            return Err("shell surface");
        }

        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
        if pid == 0 || pid == std::process::id() {
            return Err("our own process");
        }

        let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        if ex_style & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) != 0 {
            return Err("tool or non-activating window");
        }

        // A window without a resize frame is usually a fixed-size dialog; moving it would only
        // fight it. WinUI windows (Notepad 11, Settings) are resizable while exposing neither
        // `WS_THICKFRAME` nor `WS_SIZEBOX`, because the frame is drawn by the application, so a
        // captioned window that is not a `#32770` dialog is accepted as well. The real proof is
        // still the first `SetWindowPos`: a window that refuses is unmanaged by the engine.
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let dialog = class.eq_ignore_ascii_case("#32770");
        let resizable = style & (WS_THICKFRAME.0 | WS_SIZEBOX.0) != 0
            || (!dialog && style & WS_CAPTION.0 == WS_CAPTION.0);
        if !resizable {
            return Err("not resizable");
        }

        if is_cloaked(hwnd) {
            return Err("cloaked");
        }

        let mut bounds = RECT::default();
        if GetWindowRect(hwnd, &raw mut bounds).is_err() {
            return Err("no rectangle");
        }
        let bounds = Rect::from_win32(bounds);
        if bounds.is_empty() {
            return Err("empty rectangle");
        }

        let monitor = monitor_rect(hwnd);
        let title = title_of(hwnd);
        if title.trim().is_empty() {
            return Err("blank title");
        }

        let exe = image_path(pid).unwrap_or_else(|| format!("pid:{pid}"));
        let facts = ForegroundFacts {
            window: id.0,
            pid,
            own_pid: std::process::id(),
            visible: true,
            class: class.clone(),
            bounds,
            monitor,
        };

        if fullscreen::is_fullscreen(&facts) {
            return Err("fullscreen");
        }

        Ok(Candidate {
            id,
            facts: WindowFacts::new(exe, class, title),
            bounds,
        })
    }
}

/// Describes `id` as a management candidate, or `None` when it is not one.
pub fn candidate(id: WindowId) -> Option<Candidate> {
    inspect(id).ok()
}

/// Every current top-level window the manager could take, in enumeration order.
pub fn enumerate_candidates() -> Vec<Candidate> {
    let mut collected: Vec<Candidate> = Vec::new();
    let pointer = &raw mut collected as isize;

    // SAFETY: `collect` is a valid `WNDENUMPROC` and `pointer` is the address of a `Vec` that
    // outlives the call; `EnumWindows` only ever hands it back to that callback, on this thread.
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(collect),
            windows::Win32::Foundation::LPARAM(pointer),
        )
    };

    collected
}

/// One titled top-level window, for `wm.candidates` diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TitledWindow {
    /// The handle.
    pub id: WindowId,
    /// Window class.
    pub class: String,
    /// Window title.
    pub title: String,
}

/// Every titled top-level window, including ones the manager rejects.
///
/// Enumeration alone is not enough to explain why a window is not tiled; pairing it with
/// [`inspect`] is what makes `wm.candidates` answer that question.
pub fn titled_windows() -> Vec<TitledWindow> {
    let mut collected: Vec<TitledWindow> = Vec::new();
    let pointer = &raw mut collected as isize;

    // SAFETY: as in `enumerate_candidates`: the callback runs synchronously with a live vector.
    let _ = unsafe {
        windows::Win32::UI::WindowsAndMessaging::EnumWindows(
            Some(collect_titled),
            windows::Win32::Foundation::LPARAM(pointer),
        )
    };

    collected
}

/// Collects one titled top-level window.
///
/// # Safety
///
/// `data` must be the address of a live `Vec<TitledWindow>` for the whole enumeration.
unsafe extern "system" fn collect_titled(
    window: HWND,
    data: windows::Win32::Foundation::LPARAM,
) -> BOOL {
    let collected = data.0 as *mut Vec<TitledWindow>;

    if !collected.is_null() {
        let title = title_of(window);
        if !title.is_empty() {
            // SAFETY: the vector outlives the synchronous enumeration.
            unsafe {
                (*collected).push(TitledWindow {
                    id: WindowId(window.0 as isize),
                    class: class_of(window),
                    title,
                });
            }
        }
    }

    true.into()
}

/// Collects one candidate into the vector the enumeration was started with.
///
/// # Safety
///
/// `data` must be the address of a live `Vec<Candidate>` for the whole enumeration.
unsafe extern "system" fn collect(window: HWND, data: windows::Win32::Foundation::LPARAM) -> BOOL {
    let collected = data.0 as *mut Vec<Candidate>;

    if !collected.is_null() {
        if let Some(candidate) = candidate(WindowId(window.0 as isize)) {
            // SAFETY: the pointer came from `enumerate_candidates`, which keeps the vector alive
            // for the whole enumeration; the callback runs synchronously on the calling thread.
            unsafe {
                (*collected).push(candidate);
            }
        }
    }

    true.into()
}

/// The primary monitor's work area, which is the area every layout tiles.
pub fn work_area() -> Rect {
    // SAFETY: the point is a plain value and the flag is the documented default.
    let monitor = unsafe { MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY) };
    monitor_work_area(monitor).unwrap_or_else(|| {
        // SAFETY: both metrics are simple reads of the primary display size.
        let (width, height) = unsafe {
            (
                GetSystemMetrics(SM_CXSCREEN).max(1),
                GetSystemMetrics(SM_CYSCREEN).max(1),
            )
        };
        Rect::new(0, 0, width, height)
    })
}

/// The work area of one monitor.
fn monitor_work_area(monitor: HMONITOR) -> Option<Rect> {
    if monitor.is_invalid() {
        return None;
    }

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };

    // SAFETY: `info` is live and describes its own size; the call writes into it.
    unsafe { GetMonitorInfoW(monitor, &mut info) }
        .as_bool()
        .then(|| Rect::from_win32(info.rcWork))
}

/// The full monitor rectangle of `hwnd`, or the primary work area when Windows refuses.
fn monitor_rect(hwnd: HWND) -> Rect {
    // SAFETY: the handle is live; the flag is the documented default.
    let monitor = unsafe { MonitorFromWindow(hwnd, MONITOR_DEFAULTTOPRIMARY) };

    if monitor.is_invalid() {
        return work_area();
    }

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };

    // SAFETY: `info` is live and describes its own size.
    if unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        Rect::from_win32(info.rcMonitor)
    } else {
        work_area()
    }
}

/// The window's title, or an empty string when Windows refuses to answer.
fn title_of(hwnd: HWND) -> String {
    let mut buffer = [0u16; TITLE_UNITS];
    // SAFETY: the buffer is live and its length is passed as the capacity.
    let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    let written = usize::try_from(written.max(0))
        .unwrap_or(0)
        .min(buffer.len());

    String::from_utf16_lossy(&buffer[..written])
}

/// The window's class name.
fn class_of(hwnd: HWND) -> String {
    let mut buffer = [0u16; CLASS_UNITS];
    // SAFETY: the buffer is live and its length is passed as the capacity.
    let written = unsafe { GetClassNameW(hwnd, &mut buffer) };
    let written = usize::try_from(written.max(0))
        .unwrap_or(0)
        .min(buffer.len());

    String::from_utf16_lossy(&buffer[..written])
}

/// Whether DWM has cloaked the window: a suspended UWP app, for example, is visible to User32 but
/// not on screen, and must never be tiled.
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;
    // SAFETY: `cloaked` is a live out-parameter of the documented size; a refusal means the
    // attribute is unknown, which is treated as "not cloaked" so nothing disappears silently.
    let status = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };

    status.is_ok() && cloaked != 0
}

/// The full image path of `pid`, or `None` when the process cannot be queried.
fn image_path(pid: u32) -> Option<String> {
    // SAFETY: the call takes a plain pid; the handle is owned here and closed by `CloseHandle` via
    // the explicit close below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;
    let mut buffer = [0u16; PATH_UNITS];
    let mut size = buffer.len() as u32;

    // SAFETY: the buffer is live for `size` units and the handle is open for the duration.
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };

    // SAFETY: the handle came from `OpenProcess` and is closed exactly once, here.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(process);
    }

    result.ok().map(|()| {
        let length = usize::try_from(size).unwrap_or(0).min(buffer.len());
        String::from_utf16_lossy(&buffer[..length])
    })
}

/// The real window operations used by the running manager.
#[derive(Debug, Clone)]
pub struct Win32Ops {
    /// Accent colour of the theme, used for the border decoration.
    pub accent: neb_core::theme::Rgb,
}

impl Win32Ops {
    /// Operations that paint the accent border with `accent`.
    pub fn new(accent: neb_core::theme::Rgb) -> Self {
        Self { accent }
    }
}

impl WindowOps for Win32Ops {
    fn rect(&self, id: WindowId) -> Result<Rect, String> {
        let hwnd = hwnd(id);
        let mut rect = RECT::default();

        // SAFETY: `rect` is a live, writable rectangle.
        unsafe { GetWindowRect(hwnd, &raw mut rect) }
            .map(|()| Rect::from_win32(rect))
            .map_err(|error| format!("GetWindowRect failed for {id}: {error}"))
    }

    fn set_rect(&self, id: WindowId, rect: Rect) -> Result<(), String> {
        let hwnd = hwnd(id);
        if !is_window(hwnd) {
            return Err(format!("{id} is no longer a window"));
        }

        // SWP_NOZORDER is essential: without it SetWindowPos would raise the window, and a layout
        // pass must never change which application is in front. ASYNCWINDOWPOS keeps a slow
        // application from blocking the manager's message loop.
        let flags = SWP_NOACTIVATE | SWP_ASYNCWINDOWPOS | SWP_NOZORDER;
        let width = rect.width().max(1);
        let height = rect.height().max(1);

        // SAFETY: the handle is a live window; the coordinates are plain integers.
        unsafe { SetWindowPos(hwnd, None, rect.left, rect.top, width, height, flags) }
            .map_err(|error| format!("SetWindowPos failed for {id}: {error}"))
    }

    fn show(&self, id: WindowId, visible: bool) -> Result<(), String> {
        let hwnd = hwnd(id);
        if !is_window(hwnd) {
            return Err(format!("{id} is no longer a window"));
        }

        // SAFETY: the handle is a live window and the flag is one of the two documented values.
        unsafe {
            let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
        }

        Ok(())
    }

    fn set_decorations(&self, id: WindowId, plan: &DecorationPlan) -> Result<(), String> {
        let hwnd = hwnd(id);
        let mut errors = Vec::new();

        for (attribute, value) in plan.attributes() {
            if let Err(error) = set_attribute(hwnd, attribute, value) {
                errors.push(error);
            }
        }

        // A single unsupported attribute (a pre-22H2 build, for example) must not stop the other
        // two from being applied; only a total failure is an error.
        if !plan.is_empty() && errors.len() == plan.len() {
            Err(errors.join("; "))
        } else {
            Ok(())
        }
    }

    fn read_decorations(&self, id: WindowId) -> DecorationReadback {
        let hwnd = hwnd(id);

        DecorationReadback {
            corner: read_attribute(hwnd, decorations::ATTR_CORNER).map(|value| value as i32),
            dark: read_attribute(hwnd, decorations::ATTR_DARK).map(|value| value as i32),
            border: read_attribute(hwnd, decorations::ATTR_BORDER),
        }
    }

    fn focus(&self, id: WindowId) -> Result<(), String> {
        let hwnd = hwnd(id);
        if !is_window(hwnd) {
            return Err(format!("{id} is no longer a window"));
        }

        // SAFETY: the handle is a live window; the call only asks the shell to activate it.
        if unsafe { SetForegroundWindow(hwnd) }.as_bool() {
            Ok(())
        } else {
            Err(format!("Windows refused to foreground {id}"))
        }
    }

    fn is_fullscreen(&self, id: WindowId) -> bool {
        let hwnd = hwnd(id);
        if !is_window(hwnd) {
            return false;
        }

        let mut bounds = RECT::default();
        // SAFETY: `bounds` is a live out-parameter.
        if unsafe { GetWindowRect(hwnd, &raw mut bounds) }.is_err() {
            return false;
        }

        let mut pid = 0u32;
        // SAFETY: the pid out-parameter is live.
        unsafe {
            GetWindowThreadProcessId(hwnd, Some(&mut pid));
        }

        fullscreen::is_fullscreen(&ForegroundFacts {
            window: id.0,
            pid,
            own_pid: std::process::id(),
            // SAFETY: the handle is live.
            visible: unsafe { IsWindowVisible(hwnd).as_bool() },
            class: class_of(hwnd),
            bounds: Rect::from_win32(bounds),
            monitor: monitor_rect(hwnd),
        })
    }

    fn begin_animation(&self, ids: &[WindowId]) {
        for id in ids {
            let _ = set_attribute(hwnd(*id), decorations::ATTR_TRANSITIONS, 1);
        }
    }

    fn end_animation(&self, ids: &[WindowId]) {
        for id in ids {
            let _ = set_attribute(hwnd(*id), decorations::ATTR_TRANSITIONS, 0);
        }
    }
}

/// Writes one DWM attribute; `value` is `u32` because every attribute the manager writes is a
/// `u32`-sized integer, including the signed corner preference.
pub fn set_attribute(hwnd: HWND, attribute: u32, value: u32) -> Result<(), String> {
    // SAFETY: `value` is a live `u32` of exactly the size passed; DWM copies it out synchronously.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attribute as i32),
            (&raw const value).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    }
    .map_err(|error| format!("DwmSetWindowAttribute({attribute}) failed: {error}"))
}

/// Reads one `u32` DWM attribute.
pub fn read_attribute(hwnd: HWND, attribute: u32) -> Option<u32> {
    let mut value = 0u32;

    // SAFETY: `value` is a live `u32` of exactly the size passed.
    let status = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attribute as i32),
            (&raw mut value).cast::<c_void>(),
            std::mem::size_of::<u32>() as u32,
        )
    };

    status.ok().map(|()| value)
}

/// Whether a window is minimised.
///
/// A minimised window is still managed, but it is excluded from the tiling (its rectangle is the
/// off-screen minimised one) and a hide event for it must not be treated as the application hiding
/// itself.
pub fn is_iconic(id: WindowId) -> bool {
    let hwnd = hwnd(id);
    if !is_window(hwnd) {
        return false;
    }

    // SAFETY: the handle is a live window; the call only reads its state.
    unsafe { IsIconic(hwnd).as_bool() }
}

/// The process identifier that owns `id`, or `None` when Windows refuses.
pub fn window_pid(id: WindowId) -> Option<u32> {
    let hwnd = hwnd(id);
    if !is_window(hwnd) {
        return None;
    }

    let mut pid = 0u32;
    // SAFETY: the pid out-parameter is live.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut pid));
    }

    (pid != 0).then_some(pid)
}

/// The handle behind a [`WindowId`].
fn hwnd(id: WindowId) -> HWND {
    HWND(id.0 as *mut c_void)
}

/// Whether the handle is a live window.
fn is_window(hwnd: HWND) -> bool {
    // SAFETY: the call only reads the handle.
    unsafe { IsWindow(Some(hwnd)).as_bool() }
}

/// Makes the system timer one millisecond accurate for the lifetime of the process.
///
/// Without this, `thread::sleep(8ms)` is rounded to the default 15.6 ms tick and a 176 ms animation
/// becomes a stutter of a dozen frames. The resolution is released by Windows when the process
/// exits, so there is no matching `timeEndPeriod` on the shutdown path.
pub fn begin_high_resolution_timer() {
    // SAFETY: the call only changes this process's timer resolution request.
    unsafe {
        windows::Win32::Media::timeBeginPeriod(1);
    }
}

/// A wide, NUL-terminated class name for the hidden message window.
pub fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

/// A `PCWSTR` over a buffer produced by [`wide`].
pub fn pcwstr(buffer: &[u16]) -> PCWSTR {
    PCWSTR(buffer.as_ptr())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_desktop_has_a_usable_work_area() {
        let area = work_area();

        assert!(area.width() > 0, "{area:?}");
        assert!(area.height() > 0, "{area:?}");
    }

    #[test]
    fn the_manager_never_calls_its_own_windows_candidates() {
        // The process id check is what makes SKIPOWNPROCESS a second line of defence; a null
        // handle has no facts and must answer `None` rather than build a candidate.
        assert!(candidate(WindowId(0)).is_none());
    }

    #[test]
    fn the_object_hook_covers_show_through_location_change() {
        assert_eq!(OBJECT_EVENT_RANGE.0, EVENT_OBJECT_SHOW);
        assert_eq!(OBJECT_EVENT_RANGE.1, EVENT_OBJECT_LOCATIONCHANGE);
        assert!(OBJECT_EVENT_RANGE.0 < OBJECT_EVENT_RANGE.1);
    }
}
