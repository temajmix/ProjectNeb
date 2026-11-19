//! The 2-second foreground sample the rules run against.
//!
//! Only the Win32 calls that can answer in a few microseconds are used: no process enumeration, no
//! hooks and no injected code. The window handle is read first, then its process id, executable,
//! class and title. Any single field that cannot be read is left empty, so a partially readable
//! window still feeds the rules instead of dropping the sample.

use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetClassNameW, GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
};
use windows::core::PWSTR;

use crate::rules::Foreground;

/// Longest executable path Windows accepts, in UTF-16 units, with room for the terminator.
const MAX_PATH_UNITS: usize = 32_768;

/// Longest class name Windows documents.
const CLASS_UNITS: usize = 256;

/// Longest title accepted for a rule; the full title is rarely useful and a huge one only costs
/// memory every two seconds.
const TITLE_UNITS: usize = 1024;

/// Samples the foreground window.
///
/// `None` when there is no foreground window or all three readable fields are empty, e.g. while the
/// secure desktop is up.
pub fn sample() -> Option<Foreground> {
    // SAFETY: `GetForegroundWindow` reads a window handle and has no preconditions. A null handle is
    // the documented "no foreground window" answer and is checked before use.
    let window = unsafe { GetForegroundWindow() };
    if window.is_invalid() {
        return None;
    }

    let mut pid = 0_u32;
    // SAFETY: the window handle came from the call above and the process id pointer is valid for the
    // duration of the call.
    unsafe {
        GetWindowThreadProcessId(window, Some(&mut pid));
    }

    let foreground = Foreground {
        exe: executable(pid),
        class: window_class(window),
        title: window_title(window),
    };

    (!foreground.is_empty()).then_some(foreground)
}

/// Full path of `pid`, or an empty string when the process cannot be queried.
fn executable(pid: u32) -> String {
    if pid == 0 {
        return String::new();
    }

    // SAFETY: the call only opens a handle with the least privilege that can answer the path query;
    // the handle is closed exactly once below.
    let Ok(process) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) })
    else {
        return String::new();
    };

    let mut buffer = vec![0_u16; MAX_PATH_UNITS];
    let mut length = buffer.len() as u32;

    // SAFETY: the process handle is live, the buffer is writable for the length passed, and the
    // length pointer is valid.
    let result = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_WIN32,
            PWSTR(buffer.as_mut_ptr()),
            &mut length,
        )
    };

    // SAFETY: the handle is owned by this function and is closed exactly once, here.
    unsafe {
        let _ = CloseHandle(process);
    }

    match result {
        Ok(()) => text(&buffer[..length as usize]),
        Err(_) => String::new(),
    }
}

/// Class of `window`.
fn window_class(window: windows::Win32::Foundation::HWND) -> String {
    let mut buffer = [0_u16; CLASS_UNITS];

    // SAFETY: the window handle is live and the buffer is writable for its full length.
    let length = unsafe { GetClassNameW(window, &mut buffer) };
    if length <= 0 {
        return String::new();
    }

    text(&buffer[..length as usize])
}

/// Title of `window`.
fn window_title(window: windows::Win32::Foundation::HWND) -> String {
    let mut buffer = vec![0_u16; TITLE_UNITS];

    // SAFETY: the window handle is live and the buffer is writable for its full length.
    let length = unsafe { GetWindowTextW(window, &mut buffer) };
    if length <= 0 {
        return String::new();
    }

    text(&buffer[..length as usize])
}

/// UTF-16 to lossy UTF-8; a title may contain an unpaired surrogate and must not panic.
fn text(wide: &[u16]) -> String {
    String::from_utf16_lossy(wide).trim().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sampling_the_desktop_never_panics_and_never_returns_empty_fields() {
        if let Some(foreground) = sample() {
            assert!(
                !foreground.is_empty(),
                "a returned sample must have at least one field"
            );
            assert!(
                !foreground.exe.contains('\0'),
                "the executable path is NUL free"
            );
        }
    }

    #[test]
    fn a_wide_string_is_trimmed_and_lossy() {
        let mut wide = "  notepad  ".encode_utf16().collect::<Vec<_>>();
        wide.push(0xD800);

        let text = text(&wide);
        assert!(text.starts_with("notepad"));
        assert!(text.ends_with('\u{fffd}'), "{text}");
    }
}
