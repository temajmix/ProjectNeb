//! The privileged boundary: who starts the helper, and how the manager sees a pid.
//!
//! `neb-zapret.exe` itself is an ordinary, non-elevated process. `winws.exe` needs administrator
//! rights because it installs and talks to the WinDivert driver, so every transition that touches
//! it is delegated to the same executable with `--run` or `--stop`:
//!
//! * an already elevated manager spawns the helper directly;
//! * an ordinary manager asks the shell for `runas`, which shows the UAC consent dialog.
//!
//! The `runas` call blocks until the user answers, so it runs on a worker thread: `zapret.start`
//! must be able to answer "ждём подтверждения UAC" while the dialog is still up. A cancelled prompt
//! becomes [`Host::take_error`] instead of a stuck command.

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;

use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, HINSTANCE};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    PROCESS_TERMINATE, TerminateProcess,
};
use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HRESULT, PCWSTR};

/// Exit code Windows reports for a process that is still running.
const STILL_ACTIVE: u32 = 259;

/// How a helper invocation reached the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Launch {
    /// The helper was spawned as a child, so its pid is known; the winws pid comes from the status.
    Direct(u32),
    /// The shell was asked to elevate the helper; the UAC dialog decides when it starts.
    Elevated,
}

/// Everything that can go wrong between a command and a helper.
#[derive(Debug, thiserror::Error)]
pub enum HostError {
    /// The running executable could not be located, so no helper can be started.
    #[error("cannot locate neb-zapret.exe: {0}")]
    CurrentExe(io::Error),

    /// The helper process could not be created.
    #[error("cannot start the zapret helper: {0}")]
    Spawn(#[source] io::Error),

    /// The elevation worker thread could not be created.
    #[error("cannot start the elevation worker: {0}")]
    Thread(#[source] io::Error),

    /// ShellExecuteW refused to elevate.
    #[error("cannot elevate the zapret helper (ShellExecuteW returned {code}{message})")]
    Elevation {
        /// The value ShellExecuteW returned; values up to 32 are error codes.
        code: isize,
        /// A short explanation when the code is a known one, empty otherwise.
        message: String,
    },

    /// A Win32 call failed.
    #[error("cannot {action}: {source}")]
    Win32 {
        /// Verbed operation.
        action: &'static str,
        /// The underlying error.
        #[source]
        source: windows::core::Error,
    },
}

/// The side effects the manager needs, isolated so its state machine is testable without UAC.
pub trait Host: Send + Sync + std::fmt::Debug {
    /// Whether the manager itself is elevated.
    fn is_admin(&self) -> bool;

    /// Starts the helper for `strategy`, reading `pack` and writing `status`.
    ///
    /// `elevate` asks for `runas`; it is ignored when the manager is already elevated.
    ///
    /// # Errors
    ///
    /// [`HostError`] when the helper cannot be created or the shell refuses the elevation.
    fn start(
        &self,
        strategy: &str,
        pack: &Path,
        status: &Path,
        elevate: bool,
    ) -> Result<Launch, HostError>;

    /// Starts the helper in its `--stop` mode.
    ///
    /// # Errors
    ///
    /// [`HostError`] when the helper cannot be created or the shell refuses the elevation.
    fn stop(&self, pack: &Path, status: &Path, elevate: bool) -> Result<(), HostError>;

    /// Whether `pid` names a live process.
    fn pid_alive(&self, pid: u32) -> bool;

    /// Takes the error a background elevation reported, if any.
    fn take_error(&self) -> Option<String>;
}

/// The real host: Win32, the current executable and a background UAC thread.
#[derive(Debug, Clone)]
pub struct RealHost {
    errors: Arc<Mutex<Option<String>>>,
}

impl RealHost {
    /// A host with no background error yet.
    pub fn new() -> Self {
        Self {
            errors: Arc::new(Mutex::new(None)),
        }
    }

    /// Runs `args` elevated on a worker thread and remembers a refusal for the next poll.
    fn elevate_async(&self, exe: PathBuf, args: Vec<String>) -> Result<(), HostError> {
        let errors = Arc::clone(&self.errors);

        thread::Builder::new()
            .name("neb-zapret-uac".to_owned())
            .spawn(move || {
                if let Err(error) = shell_execute(&exe, &args) {
                    let mut slot = errors.lock().unwrap_or_else(|poison| poison.into_inner());
                    *slot = Some(error.to_string());
                }
            })
            .map_err(HostError::Thread)?;

        Ok(())
    }

    /// Starts the helper as a child, hidden and without a console window.
    fn spawn_helper(exe: &Path, args: &[String]) -> Result<u32, HostError> {
        use std::os::windows::process::CommandExt;

        let mut command = std::process::Command::new(exe);
        command.args(args);
        command.creation_flags(CREATE_NO_WINDOW.0);
        command
            .spawn()
            .map(|child| child.id())
            .map_err(HostError::Spawn)
    }

    fn helper_arguments(
        mode: &str,
        strategy: Option<&str>,
        pack: &Path,
        status: &Path,
    ) -> Vec<String> {
        let mut args = vec![mode.to_owned()];
        if let Some(strategy) = strategy {
            args.push(strategy.to_owned());
        }
        args.push("--pack".to_owned());
        args.push(pack.display().to_string());
        args.push("--status".to_owned());
        args.push(status.display().to_string());
        args
    }
}

impl Default for RealHost {
    fn default() -> Self {
        Self::new()
    }
}

impl Host for RealHost {
    fn is_admin(&self) -> bool {
        is_admin()
    }

    fn start(
        &self,
        strategy: &str,
        pack: &Path,
        status: &Path,
        elevate: bool,
    ) -> Result<Launch, HostError> {
        let exe = std::env::current_exe().map_err(HostError::CurrentExe)?;
        let args = Self::helper_arguments("--run", Some(strategy), pack, status);

        if elevate && !is_admin() {
            self.elevate_async(exe, args)?;
            return Ok(Launch::Elevated);
        }

        Self::spawn_helper(&exe, &args).map(Launch::Direct)
    }

    fn stop(&self, pack: &Path, status: &Path, elevate: bool) -> Result<(), HostError> {
        let exe = std::env::current_exe().map_err(HostError::CurrentExe)?;
        let args = Self::helper_arguments("--stop", None, pack, status);

        if elevate && !is_admin() {
            self.elevate_async(exe, args)
        } else {
            Self::spawn_helper(&exe, &args).map(|_pid| ())
        }
    }

    fn pid_alive(&self, pid: u32) -> bool {
        pid_alive(pid)
    }

    fn take_error(&self) -> Option<String> {
        self.errors
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take()
    }
}

/// Whether this process holds an elevated token.
///
/// `IsUserAnAdmin` is deprecated in favour of a token query, but it is the answer the rest of the
/// Windows ecosystem and the old pack's scripts use, and it costs one call.
pub fn is_admin() -> bool {
    // SAFETY: the call has no arguments, allocates nothing and changes no state.
    unsafe { IsUserAnAdmin().as_bool() }
}

/// Whether `pid` names a process that has not exited.
///
/// A pid can be reused, so this is only a hint; the manager combines it with the status document
/// and the strategy it recorded.
pub fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }

    // SAFETY: the pid is a plain value and no handle is stored beyond the close below.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }) else {
        return false;
    };

    let mut code = 0u32;
    // SAFETY: `handle` is a live process handle with query rights, and `code` is a live out-param.
    let queried = unsafe { GetExitCodeProcess(handle, &mut code) }.is_ok();

    // SAFETY: the handle is owned by this function and closed exactly once.
    unsafe {
        let _ = CloseHandle(handle);
    }

    queried && code == STILL_ACTIVE
}

/// Ends `pid`, treating a process that is already gone as stopped.
///
/// # Errors
///
/// [`HostError::Win32`] when the process exists but cannot be opened or terminated.
pub fn terminate(pid: u32) -> Result<(), HostError> {
    // SAFETY: the pid is a plain value and the returned handle is closed below.
    let handle = match unsafe { OpenProcess(PROCESS_TERMINATE, false, pid) } {
        Ok(handle) => handle,
        // ERROR_INVALID_PARAMETER: the process already exited.
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INVALID_PARAMETER.0) => {
            return Ok(());
        }
        Err(source) => {
            return Err(HostError::Win32 {
                action: "open winws.exe for termination",
                source,
            });
        }
    };

    // SAFETY: `handle` has PROCESS_TERMINATE and is closed exactly once below.
    let result = unsafe { TerminateProcess(handle, 1) };

    // SAFETY: the handle is owned by this function and closed exactly once.
    unsafe {
        let _ = CloseHandle(handle);
    }

    result.map_err(|source| HostError::Win32 {
        action: "terminate winws.exe",
        source,
    })
}

/// Brings up the UAC consent dialog for `exe args` and waits until the user answers.
fn shell_execute(exe: &Path, args: &[String]) -> Result<(), HostError> {
    let operation = wide("runas");
    let file = wide(&exe.display().to_string());
    let parameters = wide(&windows_command_line(args));
    let directory = wide(
        &exe.parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_default(),
    );

    // SAFETY: every string is a NUL-terminated UTF-16 buffer that outlives the call, and the
    // optional window handle is the documented "no parent" value.
    let result: HINSTANCE = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR(directory.as_ptr()),
            SW_HIDE,
        )
    };

    let code = result.0 as isize;
    if code <= 32 {
        return Err(HostError::Elevation {
            code,
            message: shell_error_message(code),
        });
    }

    Ok(())
}

/// Quotes one argument the way `CommandLineToArgvW` reads it back.
pub fn quote_argument(argument: &str) -> String {
    let simple = !argument.is_empty()
        && !argument
            .chars()
            .any(|character| character.is_whitespace() || character == '"');

    if simple {
        return argument.to_owned();
    }

    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');
    let mut backslashes = 0;

    for character in argument.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                quoted.push_str(&"\\".repeat(backslashes * 2 + 1));
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                quoted.push_str(&"\\".repeat(backslashes));
                backslashes = 0;
                quoted.push(other);
            }
        }
    }

    quoted.push_str(&"\\".repeat(backslashes * 2));
    quoted.push('"');
    quoted
}

/// The raw command line ShellExecuteW takes.
pub fn windows_command_line(args: &[String]) -> String {
    args.iter()
        .map(|argument| quote_argument(argument))
        .collect::<Vec<_>>()
        .join(" ")
}

fn shell_error_message(code: isize) -> String {
    match code {
        2 => ": the file was not found".to_owned(),
        5 => ": access denied, the UAC prompt was cancelled".to_owned(),
        31 => ": no association for the file type".to_owned(),
        1223 => ": the UAC prompt was cancelled".to_owned(),
        _ => String::new(),
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_arguments_are_not_quoted() {
        for argument in ["--new", "general", "C:\\zapret\\bin\\winws.exe"] {
            assert_eq!(quote_argument(argument), argument);
        }
    }

    #[test]
    fn an_argument_with_spaces_is_quoted() {
        assert_eq!(quote_argument("general (ALT)"), "\"general (ALT)\"");
        assert_eq!(
            quote_argument(r"C:\Program Files\zapret"),
            r#""C:\Program Files\zapret""#
        );
    }

    #[test]
    fn embedded_quotes_and_trailing_backslashes_survive() {
        assert_eq!(quote_argument(r#"a"b"#), r#""a\"b""#);
        assert_eq!(
            quote_argument(r"C:\path with space\"),
            r#""C:\path with space\\""#
        );
    }

    #[test]
    fn a_command_line_round_trips_through_quoting() {
        let args = vec![
            "--run".to_owned(),
            "general (ALT)".to_owned(),
            "--pack".to_owned(),
            r"C:\Users\a b\AppData\Local\ProjectNeb\zapret".to_owned(),
        ];

        assert_eq!(
            windows_command_line(&args),
            r#"--run "general (ALT)" --pack "C:\Users\a b\AppData\Local\ProjectNeb\zapret""#
        );
    }

    #[test]
    fn a_pid_that_cannot_be_opened_is_not_alive() {
        assert!(!pid_alive(0));
        assert!(!pid_alive(u32::MAX));
    }

    #[test]
    fn this_process_is_alive_and_elevation_has_an_answer() {
        assert!(pid_alive(std::process::id()));
        let _ = is_admin();
    }
}
