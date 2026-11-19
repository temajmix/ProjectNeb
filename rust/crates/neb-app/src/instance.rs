//! One settings window per session.
//!
//! The first process owns [`INSTANCE_MUTEX`] and creates the window. A second process opens the
//! same mutex, sees that it already existed, finds the window by [`CLASS_NAME`] and brings it to the
//! front instead of creating a second WebView2 controller.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, SW_RESTORE, SetForegroundWindow, ShowWindow,
};
use windows::core::PCWSTR;

use crate::window::CLASS_NAME;

/// Mutex that makes sure a session has exactly one settings window.
pub const INSTANCE_MUTEX: &str = r"Local\ProjectNeb.App";

/// Environment variable that renames the single-instance mutex.
///
/// It exists so a QA run of a development build can open next to the installed one instead of
/// merely focusing it; an unset or blank value keeps [`INSTANCE_MUTEX`].
pub const INSTANCE_VARIABLE: &str = "NEB_APP_INSTANCE";

/// The mutex name for this process: [`INSTANCE_MUTEX`], or the [`INSTANCE_VARIABLE`] override.
pub fn mutex_name() -> String {
    std::env::var(INSTANCE_VARIABLE)
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| INSTANCE_MUTEX.to_owned())
}

/// How long a second process waits for the first window to appear.
///
/// The window is created after the data root, logging and COM are ready, so a launch that races the
/// first one can arrive a moment before the class exists.
pub const FOCUS_TIMEOUT: Duration = Duration::from_secs(2);

/// Pause between lookups while the first window is being created.
const FOCUS_POLL: Duration = Duration::from_millis(50);

/// What a second process does with the window it found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Restore and foreground the existing window.
    BringToFront,
    /// The window disappeared before it could be found; the process still exits successfully.
    AlreadyGone,
}

impl Focus {
    /// Whether the window should be restored and brought forward.
    pub fn brings_to_front(self) -> bool {
        self == Self::BringToFront
    }
}

/// The focus decision, split out from Win32 so it is testable without a desktop.
pub fn decision(found: bool) -> Focus {
    if found {
        Focus::BringToFront
    } else {
        Focus::AlreadyGone
    }
}

/// A named mutex held for as long as this process owns the window slot.
#[derive(Debug)]
pub struct Instance {
    handle: HANDLE,
}

impl Instance {
    /// Creates or opens `name`; the second value is `true` when this process is the first.
    ///
    /// # Errors
    ///
    /// The Win32 error when the mutex cannot be created at all.
    pub fn acquire(name: &str) -> Result<(Self, bool), windows::core::Error> {
        let wide = wide(name);

        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call, no security
        // attributes are asked for and the mutex is created unowned.
        let handle = unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }?;

        // SAFETY: the call above succeeded; the last error is how Win32 reports an existing object.
        let first = unsafe { GetLastError() } != ERROR_ALREADY_EXISTS;

        Ok((Self { handle }, first))
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateMutexW` and is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Finds the existing window, restores it and brings it to the front.
///
/// Returns [`Focus::AlreadyGone`] when no window appeared within [`FOCUS_TIMEOUT`]; a second
/// process that raced a closing window is not an error.
pub fn focus_existing() -> Focus {
    let class = wide(CLASS_NAME);
    let deadline = Instant::now() + FOCUS_TIMEOUT;

    loop {
        // SAFETY: the class name is NUL-terminated and outlives the call; a null title asks for
        // any window of that class.
        let found = unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) };

        if let Ok(hwnd) = found {
            if !hwnd.is_invalid() {
                if decision(true).brings_to_front() {
                    // SAFETY: `hwnd` belongs to another process but is a live top-level window;
                    // both calls take a handle and do not dereference it in this process.
                    unsafe {
                        let _ = ShowWindow(hwnd, SW_RESTORE);
                        let _ = SetForegroundWindow(hwnd);
                    }
                }

                return Focus::BringToFront;
            }
        }

        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return decision(false);
        }
        std::thread::sleep(FOCUS_POLL.min(left));
    }
}

/// Encodes `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_window_is_restored_and_a_missing_one_is_not_an_error() {
        assert_eq!(decision(true), Focus::BringToFront);
        assert!(decision(true).brings_to_front());
        assert_eq!(decision(false), Focus::AlreadyGone);
        assert!(!decision(false).brings_to_front());
    }

    #[test]
    fn the_first_acquire_wins_and_the_second_sees_the_object() {
        let name = format!(r"Local\ProjectNeb.App.test.{}", std::process::id());

        let (_first, won) = Instance::acquire(&name).expect("first mutex");
        assert!(won);

        let (_second, won) = Instance::acquire(&name).expect("second mutex");
        assert!(!won, "the second process must find the existing instance");
    }
}
