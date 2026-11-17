//! `neb-cli windows`: the visible top-level windows and who owns them.
//!
//! The shell will eventually need exactly this list (to find fullscreen windows, to place its own),
//! so the query lives here first, behind a command that can be run against a live desktop. Text
//! output is one window per line — `hwnd pid class title` — and `--json` is the same data keyed by
//! name. Titles arrive as UTF-16 and are converted lossily, so a window with characters outside the
//! basic plane cannot make the tool fail.

use serde::Serialize;
use windows::Win32::Foundation::{HWND, LPARAM, TRUE};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
    IsWindowVisible,
};
use windows::core::BOOL;

use super::{CliError, to_json, write_json, write_line};
use crate::args::Options;

/// One visible top-level window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowInfo {
    /// Window handle, decimal in JSON and hexadecimal in text.
    pub hwnd: isize,
    /// Process that owns the window.
    pub pid: u32,
    /// Window class name, e.g. `Chrome_WidgetWin_1`.
    pub class: String,
    /// Window title; windows with a blank title are not listed.
    pub title: String,
}

/// Runs `neb-cli windows`.
pub fn run(args: &[String]) -> Result<(), CliError> {
    let options = Options::parse(args)?;

    if let Some(argument) = options.positional.first() {
        return Err(CliError::Usage(format!(
            "windows takes no arguments, got '{argument}'"
        )));
    }

    let windows = list();

    if options.json {
        return write_json(&to_json(&windows)?);
    }

    for window in &windows {
        write_line(&format!(
            "0x{:08X} {} {} {}",
            window.hwnd, window.pid, window.class, window.title
        ))?;
    }

    Ok(())
}

/// Enumerates the visible top-level windows that have a title, in the order Windows reports them.
pub fn list() -> Vec<WindowInfo> {
    let mut windows: Vec<WindowInfo> = Vec::new();

    // SAFETY: `lparam` carries a pointer to `windows`, which outlives the call. `collect` is the
    // only code that touches it and runs synchronously inside `EnumWindows`.
    unsafe {
        let _ = EnumWindows(
            Some(collect),
            LPARAM(&mut windows as *mut Vec<WindowInfo> as isize),
        );
    }

    windows
}

/// Appends one window per callback and asks Windows for the next one.
///
/// # Safety
///
/// `lparam` must be the pointer to a live `Vec<WindowInfo>` passed by [`list`].
unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: guaranteed by this function's contract; the vector is alive for the whole enumeration.
    let windows = unsafe { &mut *(lparam.0 as *mut Vec<WindowInfo>) };

    // SAFETY: `hwnd` is a window handle supplied by the enumeration.
    if !unsafe { IsWindowVisible(hwnd) }.as_bool() {
        return TRUE;
    }

    let Some(title) = window_title(hwnd) else {
        return TRUE;
    };

    let mut process = 0u32;
    // SAFETY: `hwnd` is live and `process` is a live out-parameter.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&mut process));
    }

    windows.push(WindowInfo {
        hwnd: hwnd.0 as isize,
        pid: process,
        class: window_class(hwnd),
        title,
    });

    TRUE
}

/// The window's title, or `None` when it has none worth listing.
fn window_title(hwnd: HWND) -> Option<String> {
    // SAFETY: `hwnd` is a live window handle; the length is queried before anything is read.
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return None;
    }

    let capacity = usize::try_from(length).ok()? + 1;
    let mut buffer = vec![0u16; capacity];

    // SAFETY: the buffer holds `capacity` UTF-16 units, which is one more than the reported length,
    // so the terminator fits.
    let copied = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    let copied = usize::try_from(copied).ok().filter(|copied| *copied > 0)?;

    let title = String::from_utf16_lossy(&buffer[..copied.min(buffer.len())]);

    if title.trim().is_empty() {
        return None;
    }

    Some(title)
}

/// The window's class name, or an empty string when Windows does not answer.
fn window_class(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];

    // SAFETY: the buffer is live and its length is passed as the maximum character count.
    let copied = unsafe { GetClassNameW(hwnd, &mut buffer) };
    let Some(copied) = usize::try_from(copied).ok().filter(|copied| *copied > 0) else {
        return String::new();
    };

    String::from_utf16_lossy(&buffer[..copied.min(buffer.len())])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_windows_is_safe_and_self_consistent() {
        let windows = list();

        for window in &windows {
            assert_ne!(window.hwnd, 0, "a listed window has a handle");
            assert!(!window.title.trim().is_empty(), "{window:?}");
            assert!(!window.class.is_empty(), "{window:?}");
        }
    }

    #[test]
    fn a_listed_window_serialises_with_the_documented_keys() {
        let value = to_json(&WindowInfo {
            hwnd: 0x1234,
            pid: 42,
            class: "Notepad".to_owned(),
            title: "Безымянный — Блокнот".to_owned(),
        })
        .expect("serialise");

        assert_eq!(value["hwnd"], serde_json::json!(0x1234));
        assert_eq!(value["pid"], serde_json::json!(42));
        assert_eq!(value["class"], serde_json::json!("Notepad"));
        assert_eq!(value["title"], serde_json::json!("Безымянный — Блокнот"));
    }

    #[test]
    fn operands_are_refused() {
        assert!(matches!(
            run(&["extra".to_owned()]),
            Err(CliError::Usage(_))
        ));
    }
}
