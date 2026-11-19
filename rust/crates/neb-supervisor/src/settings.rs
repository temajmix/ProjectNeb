//! Opening the main window, `neb-app.exe`.
//!
//! One helper serves every caller: the start-up path, the `app.openSettings` IPC method and the
//! tray row. A running supervisor answers [`OPEN_METHOD`], so asking it is the only way a second
//! launch reaches the window instead of starting a second supervisor.
//!
//! The window itself is a GUI binary, so `CreateProcessW` is called with `CREATE_NO_WINDOW`: a
//! console flashing behind the window would be a bug. The handles are closed immediately, because
//! this process only launches the window, it does not supervise it.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neb_core::ipc::{self, IpcError};
use serde_json::Value;
use windows::Win32::Foundation::{CloseHandle, LPARAM, WPARAM};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, IsWindow, PostMessageW, WM_CLOSE};
use windows::core::PCWSTR;

use crate::supervisor::{PIPE, Reporter};

/// Executable name of the main window, looked up next to `ProjectNeb.exe`.
pub const APP_EXE: &str = "neb-app.exe";

/// Window class `neb-app.exe` registers, used to close it when the product stops.
pub const WINDOW_CLASS: &str = "ProjectNeb.Settings.Rust";

/// How long a shutdown waits for the settings window to disappear.
const CLOSE_BUDGET: Duration = Duration::from_secs(2);

/// Pause between checks while the window is closing.
const CLOSE_POLL: Duration = Duration::from_millis(20);

/// IPC method that opens (or focuses) the main window on the running supervisor.
pub const OPEN_METHOD: &str = "app.openSettings";

/// How long a launch waits for the running supervisor to take the request.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(2);

/// Pause between attempts while the supervisor is still bringing its pipe up.
const RETRY_PAUSE: Duration = Duration::from_millis(50);

/// Whether [`spawn`] has an executable to start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// The window executable exists and can be started.
    Spawn,
    /// There is no `neb-app.exe`; nothing to start.
    Missing,
}

impl Launch {
    /// Whether a process should be created.
    pub fn spawns(self) -> bool {
        self == Self::Spawn
    }
}

/// The spawn decision for an executable path: only a path that exists is started.
///
/// Split out from Windows so the choice is testable without creating a process.
pub fn decision(exe: Option<&Path>) -> Launch {
    match exe {
        Some(path) if path.is_file() => Launch::Spawn,
        _ => Launch::Missing,
    }
}

/// `neb-app.exe` next to this executable, or the installed copy under the data root.
pub fn app_exe() -> Option<PathBuf> {
    let sibling = std::env::current_exe()
        .ok()?
        .parent()
        .map(|dir| dir.join(APP_EXE))
        .filter(|path| path.is_file());

    sibling.or_else(|| {
        let installed = neb_core::paths::try_root()
            .ok()
            .map(|root| root.join("app").join(APP_EXE));

        installed.filter(|path| path.is_file())
    })
}

/// Starts the window and closes the process handles at once.
///
/// # Errors
///
/// The reason the executable is missing or Windows refused the process, as a sentence for a log.
pub fn spawn(exe: &Path) -> Result<(), String> {
    if !decision(Some(exe)).spawns() {
        return Err(format!("'{}' does not exist", exe.display()));
    }

    // `CreateProcessW` may write to the command line, hence the mutable buffer; quoting keeps a
    // path with spaces one argument.
    let mut command: Vec<u16> = format!("\"{}\"", exe.display())
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let application = wide(&exe.to_string_lossy());

    let startup = STARTUPINFOW {
        cb: std::mem::size_of::<STARTUPINFOW>() as u32,
        ..STARTUPINFOW::default()
    };
    let mut process = PROCESS_INFORMATION::default();

    // SAFETY: both strings are NUL-terminated and outlive the call; the startup info is a valid
    // struct of the size it reports; on success the two handles below are ours to close.
    let created = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(windows::core::PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR::null(),
            &startup,
            &mut process,
        )
    };

    match created {
        Ok(()) => {
            // SAFETY: the call succeeded, so both handles are live and owned by this process.
            unsafe {
                let _ = CloseHandle(process.hThread);
                let _ = CloseHandle(process.hProcess);
            }
            Ok(())
        }
        Err(error) => Err(format!("cannot start '{}': {error}", exe.display())),
    }
}

/// Starts `neb-app.exe` when it exists and logs the outcome.
pub fn launch(reporter: &Reporter) {
    let Some(exe) = app_exe() else {
        reporter
            .warn("app: neb-app.exe is not next to ProjectNeb.exe; the main window is unavailable");
        return;
    };

    match spawn(&exe) {
        Ok(()) => reporter.info(&format!("app: opened the main window '{}'", exe.display())),
        Err(reason) => reporter.warn(&format!("app: {reason}")),
    }
}

/// Closes the settings window, waiting briefly for it to disappear.
///
/// The window is a separate process that the supervisor deliberately does not supervise, so
/// stopping every child would leave it running after "Exit" in the tray and would block an
/// updater from replacing `neb-app.exe`. Posting `WM_CLOSE` is the same request a user makes by
/// clicking the close button: the window tears down its WebView2 controller and exits normally.
pub fn close_window() {
    let class = wide(WINDOW_CLASS);

    // SAFETY: the class name is NUL-terminated and outlives the call; a null title asks for any
    // window of that class.
    let Ok(hwnd) = (unsafe { FindWindowW(PCWSTR(class.as_ptr()), PCWSTR::null()) }) else {
        return;
    };
    if hwnd.is_invalid() {
        return;
    }

    // SAFETY: the handle is a live top-level window of another process; both calls accept an
    // opaque handle and do not dereference it here.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    }

    let deadline = Instant::now() + CLOSE_BUDGET;
    while Instant::now() < deadline {
        // SAFETY: `IsWindow` only reads the window state for the handle it is given.
        if !unsafe { IsWindow(Some(hwnd)).as_bool() } {
            return;
        }
        std::thread::sleep(CLOSE_POLL);
    }
}

/// Asks the running supervisor to open or focus the window, retrying until `timeout` runs out.
///
/// A no-op retry is what makes a second launch that races the first supervisor's pipe creation
/// succeed instead of failing; a server-side refusal is returned at once, because trying again
/// would not change it.
///
/// # Errors
///
/// The last transport error, or [`IpcError::Server`] when the supervisor refused the request.
pub fn request(timeout: Duration) -> Result<Value, IpcError> {
    let deadline = Instant::now() + timeout;
    let mut last = None;

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(last.unwrap_or_else(|| IpcError::Timeout {
                method: OPEN_METHOD.to_owned(),
                timeout,
            }));
        }

        match ipc::call(PIPE, OPEN_METHOD, None, remaining) {
            Ok(value) => return Ok(value),
            Err(error @ (IpcError::Unavailable { .. } | IpcError::Timeout { .. })) => {
                last = Some(error);
            }
            Err(error @ IpcError::Closed { .. }) => last = Some(error),
            Err(error) => return Err(error),
        }

        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            break;
        }
        std::thread::sleep(RETRY_PAUSE.min(left));
    }

    Err(last.unwrap_or_else(|| IpcError::Timeout {
        method: OPEN_METHOD.to_owned(),
        timeout,
    }))
}

/// The tray row and any other in-process caller: ask the running supervisor first, so a second
/// click focuses the window, and start the executable directly when no supervisor answers.
pub fn open(reporter: &Reporter) {
    if request(OPEN_TIMEOUT).is_ok() {
        reporter.info("app: the running supervisor was asked to open the main window");
        return;
    }

    launch(reporter);
}

/// Encodes `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_existing_executable_is_spawned_and_a_missing_one_is_not() {
        let directory = tempfile::tempdir().expect("temp dir");
        let exe = directory.path().join(APP_EXE);
        std::fs::write(&exe, b"").expect("write");

        assert!(decision(Some(&exe)).spawns());
        assert_eq!(decision(Some(&exe)), Launch::Spawn);
        assert_eq!(
            decision(Some(&directory.path().join("missing.exe"))),
            Launch::Missing
        );
        assert_eq!(decision(None), Launch::Missing);
        assert!(!decision(None).spawns());
    }
}
