//! The fullscreen rule, shared by the manager's own filter and the dock.
//!
//! The manager must never move an application that owns the whole monitor: a game, a video player
//! or a presentation. The rule is the one `neb-shell::dock::fullscreen` uses for hiding the dock —
//! the *foreground* window, not ours, not a shell surface, covering its whole monitor — and it is
//! copied here rather than pulled from the shell because the manager must not link a crate that
//! renders Direct2D.
//!
//! As in the dock, a maximised window does not count: it covers the work area but leaves the
//! taskbar strip alone, and tiling it is exactly what the manager is for.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
};

use crate::rect::Rect;

/// How far a window may miss a monitor edge and still count as fullscreen.
pub const EDGE_TOLERANCE: i32 = 2;

/// Window classes that belong to the desktop or to the taskbar.
///
/// They are visible, titled and would otherwise be tiled, so the manager filters them by class the
/// same way the dock does.
pub const SHELL_CLASSES: [&str; 5] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
];

/// Whether `class` names the desktop, the taskbar or another shell surface.
pub fn is_shell_class(class: &str) -> bool {
    SHELL_CLASSES
        .iter()
        .any(|shell| shell.eq_ignore_ascii_case(class))
}

/// What the desktop says about a window, independent of its handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundFacts {
    /// The window itself; zero when there is no foreground window.
    pub window: isize,
    /// The process that owns it.
    pub pid: u32,
    /// This process, which never counts as somebody else's fullscreen app.
    pub own_pid: u32,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// Window class, so the desktop and the taskbar can be excluded.
    pub class: String,
    /// The window's screen rectangle.
    pub bounds: Rect,
    /// The rectangle of the monitor the window is on.
    pub monitor: Rect,
}

/// Whether `facts` describe a fullscreen application.
pub fn is_fullscreen(facts: &ForegroundFacts) -> bool {
    if facts.window == 0 || facts.pid == 0 || facts.pid == facts.own_pid {
        return false;
    }

    if !facts.visible || is_shell_class(&facts.class) {
        return false;
    }

    covers(facts.bounds, facts.monitor, EDGE_TOLERANCE)
}

/// Whether `window` covers `monitor` within `tolerance` pixels.
pub fn covers(window: Rect, monitor: Rect, tolerance: i32) -> bool {
    let tolerance = tolerance.max(0);

    window.left <= monitor.left + tolerance
        && window.top <= monitor.top + tolerance
        && window.right >= monitor.right - tolerance
        && window.bottom >= monitor.bottom - tolerance
}

/// Whether the foreground window is a fullscreen application right now.
///
/// The manager consults this before it touches anything, so a game that starts while the manager
/// runs is left alone even if an event arrives for it.
pub fn foreground_is_fullscreen() -> bool {
    // SAFETY: `GetForegroundWindow` only reads the desktop state.
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return false;
    }

    let mut pid = 0u32;
    // SAFETY: the pid out-parameter points at a live `u32`.
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
    }

    let mut bounds = RECT::default();
    // SAFETY: `bounds` is a live, writable rectangle.
    if unsafe { GetWindowRect(window, &mut bounds) }.is_err() {
        return false;
    }

    let mut monitor_info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..MONITORINFO::default()
    };
    // SAFETY: the monitor handle comes from the window just read; `monitor_info` is initialised
    // with its own size and is writable.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) };
    if monitor.is_invalid() || !unsafe { GetMonitorInfoW(monitor, &mut monitor_info) }.as_bool() {
        return false;
    }

    let mut class = [0u16; 256];
    // SAFETY: `class` is a live, correctly sized array and the call writes at most its length.
    let written = unsafe { GetClassNameW(window, &mut class) };
    let class = String::from_utf16_lossy(&class[..usize::try_from(written.max(0)).unwrap_or(0)]);

    is_fullscreen(&ForegroundFacts {
        window: window.0 as isize,
        pid,
        own_pid: std::process::id(),
        // SAFETY: the handle is live; the call returns a plain boolean.
        visible: unsafe { IsWindowVisible(window).as_bool() },
        class,
        bounds: Rect::from_win32(bounds),
        monitor: Rect::from_win32(monitor_info.rcMonitor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Rect = Rect::new(0, 0, 1920, 1080);

    fn facts(bounds: Rect) -> ForegroundFacts {
        ForegroundFacts {
            window: 0x1000,
            pid: 42,
            own_pid: 1,
            visible: true,
            class: "Chrome_WidgetWin_1".to_owned(),
            bounds,
            monitor: MONITOR,
        }
    }

    #[test]
    fn a_window_covering_the_monitor_is_fullscreen() {
        assert!(is_fullscreen(&facts(MONITOR)));
        assert!(is_fullscreen(&facts(Rect::new(-8, -8, 1928, 1088))));
    }

    #[test]
    fn a_maximised_window_that_leaves_the_taskbar_alone_is_not() {
        assert!(!is_fullscreen(&facts(Rect::new(0, 0, 1920, 1032))));
        assert!(!is_fullscreen(&facts(Rect::new(0, 0, 1916, 1080))));
    }

    #[test]
    fn our_own_windows_invisible_windows_and_shell_chrome_are_not() {
        let mut own = facts(MONITOR);
        own.pid = own.own_pid;
        assert!(!is_fullscreen(&own));

        let mut hidden = facts(MONITOR);
        hidden.visible = false;
        assert!(!is_fullscreen(&hidden));

        for class in SHELL_CLASSES {
            let mut shell = facts(MONITOR);
            shell.class = class.to_owned();
            assert!(!is_fullscreen(&shell), "{class}");
        }

        let mut none = facts(MONITOR);
        none.window = 0;
        assert!(!is_fullscreen(&none));
    }

    #[test]
    fn the_tolerance_is_symmetric_and_can_be_disabled() {
        let almost = Rect::new(2, 2, 1918, 1078);

        assert!(covers(almost, MONITOR, 2));
        assert!(!covers(almost, MONITOR, 1));
        assert!(covers(MONITOR, MONITOR, 0));
    }

    #[test]
    fn shell_classes_are_recognised_case_insensitively() {
        assert!(is_shell_class("progman"));
        assert!(is_shell_class("SHELL_TRAYWND"));
        assert!(!is_shell_class("Notepad"));
    }

    #[test]
    fn the_desktop_can_be_asked_without_panicking() {
        // The answer depends on what is in front right now; the assertion is that it answers.
        let _ = foreground_is_fullscreen();
    }
}
