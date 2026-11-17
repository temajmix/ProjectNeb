//! Whether a fullscreen application owns the screen right now.
//!
//! A topmost dock over a game is worse than a hidden one, so the dock asks the desktop this question
//! a few times a second and gets out of the way while the answer is yes. The rule is the one the
//! wallpaper's pause uses: the *foreground* window, not ours, not a shell surface, covering its whole
//! monitor — a maximised window covers only the work area and therefore does not count, which is
//! exactly the case the dock must stay visible in.
//!
//! The decision itself is pure over [`ForegroundFacts`], so the maximised-versus-fullscreen boundary
//! is tested without a desktop.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
};

use crate::geometry::Rect;

/// How far a window may miss a monitor edge and still count as fullscreen.
///
/// A window one taskbar away is a maximised window, not a fullscreen one; a window that overshoots by
/// a couple of pixels because of a fractional DPI is still fullscreen.
pub const EDGE_TOLERANCE: i32 = 2;

/// What the desktop says about the window in front.
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

    if !facts.visible || crate::dock::running::is_shell_class(&facts.class) {
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

/// Whether the foreground window is a fullscreen application.
pub fn foreground_is_fullscreen() -> bool {
    // SAFETY: `GetForegroundWindow` only reads the desktop state.
    let window = unsafe { GetForegroundWindow() };
    if window.0.is_null() {
        return false;
    }

    let mut pid = 0u32;
    // SAFETY: the pid out-parameter points at a live `u32` and may be null, which it is not here.
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
    // SAFETY: the monitor handle comes from the window just read; `monitor_info` is initialised with
    // its own size and is writable.
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
        bounds: Rect::from(bounds),
        monitor: Rect::from(monitor_info.rcMonitor),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };

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
        // The work area of a 1080-high monitor with a 48 px taskbar.
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

        let mut desktop = facts(MONITOR);
        desktop.class = "Progman".to_owned();
        assert!(!is_fullscreen(&desktop));

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
    fn the_desktop_can_be_asked_without_panicking() {
        // The answer depends on what is in front right now; the assertion is that it answers.
        let _ = foreground_is_fullscreen();
    }
}
