//! When the wallpaper stops drawing.
//!
//! Three inputs, one decision: a fullscreen application owns the screen, the machine is on battery,
//! or the session is locked. The decision itself ([`should_pause`]) is a pure function so the matrix
//! is a unit test; the rest of the module samples the machine and is deliberately forgiving - a
//! failed probe means "no reason to pause", never a permanently frozen wallpaper.

use neb_core::config::WallpaperSettings;
use neb_core::logging::LogHandle;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Power::GetSystemPowerStatus;
use windows::Win32::System::RemoteDesktop::{
    NOTIFY_FOR_THIS_SESSION, WTSRegisterSessionNotification,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowRect, GetWindowThreadProcessId, IsWindowVisible,
};

use crate::desktop::window::{self, RectI};

/// How far a window's edges may sit from its monitor's before it stops counting as fullscreen.
pub const EDGE_TOLERANCE: i32 = 2;

/// `ACLineStatus` when the machine reports that the value is not known.
pub const ACLINE_UNKNOWN: u8 = 255;

/// Environment variable that turns on the acceptance hooks below. Without it the wallpaper reads no
/// files and behaves exactly like the shipped build.
pub const HOOKS_VARIABLE: &str = "NEB_WALLPAPER_HOOKS";

/// File whose contents force one pause input while the hooks are on: `fullscreen`, `battery`, or
/// anything else for "no override".
pub const HOOKS_FILE: &str = "wallpaper-pause.txt";

/// A pause input the acceptance harness can force.
///
/// The real inputs are sensors - the foreground window, the power line - and neither can be driven
/// from a test harness on a machine nobody is touching. Forcing one through a file keeps the rest of
/// the path real: the same rule, the same transition, the same log line, the same stop of the frame
/// clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForcedInput {
    /// Pretend a fullscreen application owns the foreground.
    Fullscreen,
    /// Pretend the machine runs on battery.
    Battery,
}

impl ForcedInput {
    /// The word the hook file holds.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Fullscreen => "fullscreen",
            Self::Battery => "battery",
        }
    }
}

/// Windows that cover the screen but are the desktop, not an application in front of it.
const SHELL_CLASSES: [&str; 6] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
    "MultitaskingViewFrame",
];

/// The three inputs the pause decision is made from, sampled together so a log line describes one
/// moment rather than three.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PauseState {
    /// A fullscreen application owns the foreground.
    pub fullscreen: bool,
    /// The machine is running on battery.
    pub on_battery: bool,
    /// The session is locked.
    pub locked: bool,
}

impl PauseState {
    /// The state as the log and the IPC answer show it.
    pub fn describe(&self) -> String {
        format!(
            "fullscreen={} battery={} locked={}",
            self.fullscreen, self.on_battery, self.locked
        )
    }
}

/// Samples the machine, applying the acceptance hook when it is switched on.
///
/// `locked` is the caller's: it comes from session notifications, not from a probe.
pub fn sample(log: Option<&LogHandle>, locked: bool) -> PauseState {
    let forced = forced_input();

    PauseState {
        fullscreen: forced == Some(ForcedInput::Fullscreen) || foreground_fullscreen(),
        on_battery: forced == Some(ForcedInput::Battery) || on_battery(log),
        locked,
    }
}

/// Whether the hooks are on, and where the hook file is.
///
/// `None` in a normal run: the file is never opened and the exported build has no way to be paused
/// except by the machine's real state.
pub fn hooks() -> Option<std::path::PathBuf> {
    let enabled = std::env::var(HOOKS_VARIABLE).is_ok_and(|value| value.trim() == "1");
    if !enabled {
        return None;
    }

    neb_core::paths::try_root()
        .ok()
        .map(|root| root.join(HOOKS_FILE))
}

/// The input the hook file forces right now, or `None`.
fn forced_input() -> Option<ForcedInput> {
    let path = hooks()?;
    let text = std::fs::read_to_string(path).ok()?;

    parse_forced(&text)
}

/// The input a hook file's contents name, or `None` for anything else.
pub fn parse_forced(text: &str) -> Option<ForcedInput> {
    match text.trim().to_ascii_lowercase().as_str() {
        "fullscreen" => Some(ForcedInput::Fullscreen),
        "battery" => Some(ForcedInput::Battery),
        _ => None,
    }
}

/// Whether the wallpaper should be suspended for `state`.
///
/// A locked session always pauses: the desktop is not on screen, and animating it behind a lock
/// screen is pure waste. Fullscreen and battery are the user's switches, and both default to on.
pub fn should_pause(state: PauseState, settings: &WallpaperSettings) -> bool {
    if state.locked {
        return true;
    }

    (state.fullscreen && settings.pause_on_fullscreen)
        || (state.on_battery && settings.pause_on_battery)
}

/// The decision for one raw `ACLineStatus`: `0` is offline (battery), `1` is online, `255` unknown.
///
/// Unknown counts as mains: pausing a wallpaper because a probe failed would be a silent, permanent
/// freeze.
pub fn is_on_battery(ac_line_status: u8) -> bool {
    ac_line_status == 0
}

/// Whether the machine is on battery right now.
pub fn on_battery(log: Option<&LogHandle>) -> bool {
    let mut status = windows::Win32::System::Power::SYSTEM_POWER_STATUS {
        ACLineStatus: ACLINE_UNKNOWN,
        ..Default::default()
    };

    // SAFETY: `status` is a live, writable struct of the type the API fills.
    if unsafe { GetSystemPowerStatus(&mut status) }.is_err() {
        if let Some(log) = log {
            log.warn(
                "wallpaper",
                "GetSystemPowerStatus failed, assuming mains power",
            );
        }

        return false;
    }

    is_on_battery(status.ACLineStatus)
}

/// Whether the foreground window is a fullscreen application, excluding our own windows.
pub fn foreground_fullscreen() -> bool {
    // SAFETY: `GetForegroundWindow` only reads the desktop state.
    let window = unsafe { GetForegroundWindow() };
    if window::is_empty_handle(window) {
        return false;
    }

    let mut process = 0u32;
    // SAFETY: the process id out-parameter is optional and points at a live `u32`.
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut process));
    }

    is_fullscreen(window, process, std::process::id())
}

/// The whole check for one window: visible, not ours, not a shell surface, and covering its monitor.
pub fn is_fullscreen(window: HWND, window_process: u32, own_process: u32) -> bool {
    if window::is_empty_handle(window) || window_process == own_process {
        return false;
    }

    // SAFETY: the handle was checked above; a stale value makes these calls fail, which returns
    // `false` below.
    if !unsafe { IsWindowVisible(window).as_bool() } {
        return false;
    }

    if is_shell_class(&class_name(window)) {
        return false;
    }

    let mut bounds = RECT::default();
    // SAFETY: `bounds` is a live, writable rectangle.
    if unsafe { GetWindowRect(window, &mut bounds) }.is_err() {
        return false;
    }

    // SAFETY: the handle was checked; the monitor handle is only read.
    let monitor = unsafe { MonitorFromWindow(window, MONITOR_DEFAULTTOPRIMARY) };
    if monitor.is_invalid() {
        return false;
    }

    let mut info = MONITORINFO {
        cbSize: std::mem::size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    // SAFETY: `info` is initialised with its own size and is writable.
    if !unsafe { GetMonitorInfoW(monitor, &mut info) }.as_bool() {
        return false;
    }

    covers(
        RectI::from(bounds),
        RectI::from(info.rcMonitor),
        EDGE_TOLERANCE,
    )
}

/// Whether `window` covers `monitor` within `tolerance` pixels.
///
/// A window slightly larger than the monitor counts; a window a taskbar away from being fullscreen
/// does not.
pub fn covers(window: RectI, monitor: RectI, tolerance: i32) -> bool {
    let tolerance = tolerance.max(0);

    window.left <= monitor.left + tolerance
        && window.top <= monitor.top + tolerance
        && window.right >= monitor.right - tolerance
        && window.bottom >= monitor.bottom - tolerance
}

/// Whether a window class is one of the shell's own surfaces.
pub fn is_shell_class(class: &str) -> bool {
    SHELL_CLASSES
        .iter()
        .any(|shell| shell.eq_ignore_ascii_case(class))
}

/// Asks Windows to report session lock and unlock to `window`.
///
/// Failure is not fatal: the wallpaper keeps drawing, and only the "locked" input of the pause
/// decision stays false for the life of the process.
pub fn register_session_notifications(window: HWND, log: Option<&LogHandle>) -> bool {
    // SAFETY: the handle is this thread's control window, which outlives the registration.
    match unsafe { WTSRegisterSessionNotification(window, NOTIFY_FOR_THIS_SESSION) } {
        Ok(()) => true,
        Err(error) => {
            if let Some(log) = log {
                log.warn(
                    "wallpaper",
                    &format!(
                        "session notifications unavailable ({error}); lock will not pause the scene"
                    ),
                );
            }

            false
        }
    }
}

/// The class name of a window, or an empty string when it has none.
fn class_name(window: HWND) -> String {
    let mut buffer = [0u16; 128];

    // SAFETY: the buffer is a live, writable array and the count matches its length.
    let written = unsafe { GetClassNameW(window, &mut buffer) };
    if written <= 0 {
        return String::new();
    }

    window::from_wide(&buffer[..written as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(pause_on_fullscreen: bool, pause_on_battery: bool) -> WallpaperSettings {
        WallpaperSettings {
            pause_on_fullscreen,
            pause_on_battery,
            ..WallpaperSettings::default()
        }
    }

    fn state(fullscreen: bool, on_battery: bool, locked: bool) -> PauseState {
        PauseState {
            fullscreen,
            on_battery,
            locked,
        }
    }

    #[test]
    fn the_pause_matrix_covers_every_combination() {
        let all = settings(true, true);
        let none = settings(false, false);
        let fullscreen_only = settings(true, false);
        let battery_only = settings(false, true);

        // Eight sampled states against four settings: the two switches decide, the lock always wins.
        let cases: [(PauseState, bool, bool, bool, bool); 8] = [
            // state,                       all,  none, fullscreen, battery
            (state(false, false, false), false, false, false, false),
            (state(true, false, false), true, false, true, false),
            (state(false, true, false), true, false, false, true),
            (state(true, true, false), true, false, true, true),
            (state(false, false, true), true, true, true, true),
            (state(true, false, true), true, true, true, true),
            (state(false, true, true), true, true, true, true),
            (state(true, true, true), true, true, true, true),
        ];

        for (sampled, with_all, with_none, with_fullscreen, with_battery) in cases {
            assert_eq!(
                should_pause(sampled, &all),
                with_all,
                "both switches on: {sampled:?}"
            );
            assert_eq!(
                should_pause(sampled, &none),
                with_none,
                "both switches off pauses only on a lock: {sampled:?}"
            );
            assert_eq!(
                should_pause(sampled, &fullscreen_only),
                with_fullscreen,
                "fullscreen switch only: {sampled:?}"
            );
            assert_eq!(
                should_pause(sampled, &battery_only),
                with_battery,
                "battery switch only: {sampled:?}"
            );
        }
    }

    #[test]
    fn a_locked_session_pauses_even_with_every_switch_off() {
        assert!(should_pause(
            state(false, false, true),
            &settings(false, false)
        ));
    }

    #[test]
    fn only_a_reported_zero_means_battery() {
        assert!(is_on_battery(0));
        assert!(!is_on_battery(1));
        assert!(!is_on_battery(ACLINE_UNKNOWN));
        assert!(!is_on_battery(2));
    }

    #[test]
    fn a_fullscreen_window_covers_its_monitor_within_tolerance() {
        let monitor = RectI::new(0, 0, 1920, 1080);

        assert!(covers(RectI::new(0, 0, 1920, 1080), monitor, 2));
        assert!(covers(RectI::new(-1, -1, 1921, 1081), monitor, 2));
        assert!(
            covers(RectI::new(2, 2, 1918, 1078), monitor, 2),
            "two pixels inside the edges is still fullscreen"
        );

        assert!(
            !covers(RectI::new(0, 0, 1920, 1040), monitor, 2),
            "a taskbar is not fullscreen"
        );
        assert!(!covers(RectI::new(0, 0, 1600, 1080), monitor, 2));
        assert!(
            covers(RectI::new(0, 0, 1920, 1080), monitor, -5),
            "a negative tolerance behaves as zero, it does not refuse every window"
        );
    }

    #[test]
    fn the_monitor_being_to_the_left_does_not_break_the_cover_test() {
        let monitor = RectI::new(-2560, -200, -640, 1240);
        let window = RectI::new(-2560, -200, -640, 1240);

        assert!(covers(window, monitor, EDGE_TOLERANCE));
    }

    #[test]
    fn the_shells_own_windows_are_never_fullscreen_applications() {
        for class in SHELL_CLASSES {
            assert!(is_shell_class(class), "{class} must be recognised");
            assert!(
                is_shell_class(&class.to_lowercase()),
                "the match is case-insensitive"
            );
        }

        assert!(!is_shell_class("Chrome_WidgetWin_1"));
        assert!(!is_shell_class(""));
    }

    #[test]
    fn a_state_says_all_three_inputs_in_one_line() {
        let described = state(true, false, true).describe();

        assert!(described.contains("fullscreen=true"));
        assert!(described.contains("battery=false"));
        assert!(described.contains("locked=true"));
    }

    #[test]
    fn the_hook_file_names_one_input_and_ignores_anything_else() {
        assert_eq!(parse_forced("fullscreen"), Some(ForcedInput::Fullscreen));
        assert_eq!(
            parse_forced("  FullScreen \r\n"),
            Some(ForcedInput::Fullscreen)
        );
        assert_eq!(parse_forced("battery"), Some(ForcedInput::Battery));

        for nothing in ["", "   ", "none", "locked", "true"] {
            assert_eq!(
                parse_forced(nothing),
                None,
                "'{nothing}' must not force anything"
            );
        }
    }

    #[test]
    fn a_forced_input_is_named_the_way_the_file_spells_it() {
        assert_eq!(ForcedInput::Fullscreen.as_str(), "fullscreen");
        assert_eq!(ForcedInput::Battery.as_str(), "battery");
        assert_eq!(
            parse_forced(ForcedInput::Fullscreen.as_str()),
            Some(ForcedInput::Fullscreen)
        );
    }

    #[test]
    fn the_hooks_are_off_unless_the_variable_asks_for_them() {
        // The variable is not set in a test run, so the hook file is never even looked at.
        assert_eq!(hooks(), None);
    }
}
