//! The two Win32 calls this process exists for: wait for a pid, then start the supervisor again.
//!
//! Waiting uses a synchronisation handle rather than a poll: `OpenProcess(PROCESS_SYNCHRONIZE)` plus
//! `WaitForSingleObject` blocks until the process object is signalled, which happens when the process
//! and every handle to it are gone — the exact condition that releases the image files. The timeout is
//! the one the caller passed, in milliseconds.
//!
//! Starting the supervisor mirrors the supervisor's own child spawn: `CreateProcessW` with an explicit
//! application name and a command line quoted by `CommandLineToArgvW`'s rules. `CREATE_NO_WINDOW` keeps
//! the restart from flashing a console, because this process has none to lend it.

use std::ffi::OsStr;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_INVALID_PARAMETER, GetLastError, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CreateProcessW, OpenProcess, PROCESS_INFORMATION, PROCESS_SYNCHRONIZE,
    STARTUPINFOW, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// How the wait ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitOutcome {
    /// The process exists no longer: the files it held are free.
    Exited,
    /// Windows never knew the pid — it exited before this process opened it.
    AlreadyGone,
    /// The process outlived the timeout.
    TimedOut,
}

/// Why a wait or a restart failed.
#[derive(Debug, thiserror::Error)]
pub enum ProcessError {
    /// `OpenProcess` refused, so the wait cannot even begin.
    #[error("cannot open process {pid}: {source}")]
    Open {
        /// The pid that was asked for.
        pid: u32,
        /// The Win32 error.
        #[source]
        source: windows::core::Error,
    },

    /// `WaitForSingleObject` failed.
    #[error("waiting for process {pid} failed: {source}")]
    Wait {
        /// The pid that was being waited for.
        pid: u32,
        /// The Win32 error.
        #[source]
        source: windows::core::Error,
    },

    /// `CreateProcessW` refused.
    #[error("cannot start '{}': {source}", exe.display())]
    Spawn {
        /// Executable that was about to run.
        exe: PathBuf,
        /// The Win32 error.
        #[source]
        source: windows::core::Error,
    },
}

/// Waits for `pid` to exit, at most `timeout`.
///
/// A pid Windows no longer knows is not a failure: the supervisor may have exited between the moment
/// it wrote the argument and the moment this process opened it, which is exactly the state the copy
/// wants. A pid this process is not allowed to open is a failure, because the update cannot prove the
/// files are free.
///
/// # Errors
///
/// [`ProcessError::Open`] or [`ProcessError::Wait`] when Win32 refuses.
pub fn wait_for_pid(pid: u32, timeout: Duration) -> Result<WaitOutcome, ProcessError> {
    // SAFETY: `OpenProcess` only needs a pid and an access mask; the returned handle is owned by the
    // guard below and closed exactly once when the wait is over.
    let opened = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) };

    let handle = match opened {
        Ok(handle) => OwnedHandle(handle),
        Err(source) => {
            // SAFETY: the call above failed, so the last error describes why; nothing else in this
            // process ran between the two calls.
            let code = unsafe { GetLastError() };
            if code == ERROR_INVALID_PARAMETER {
                return Ok(WaitOutcome::AlreadyGone);
            }
            return Err(ProcessError::Open { pid, source });
        }
    };

    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);

    // SAFETY: the handle is a live process handle owned by `handle`, and the millisecond count is a
    // plain value; the guard keeps the handle open until after this call returns.
    let event = unsafe { WaitForSingleObject(handle.0, millis) };

    if event == WAIT_OBJECT_0 {
        Ok(WaitOutcome::Exited)
    } else if event == WAIT_TIMEOUT {
        Ok(WaitOutcome::TimedOut)
    } else {
        // The wait returned something other than the two documented outcomes, i.e. failure; the last
        // error describes it.
        Err(ProcessError::Wait {
            pid,
            source: windows::core::Error::from_thread(),
        })
    }
}

/// Starts `exe` in `app`, returning the new process id.
///
/// `exe` is resolved the way a shell would: an absolute path is used as it is, a name that exists
/// beside the application wins, and a bare name is looked up in `PATH`. Arguments are quoted by
/// [`command_line`], so a path with spaces survives.
///
/// # Errors
///
/// [`ProcessError::Spawn`] when Windows cannot start the executable.
pub fn restart(exe: &Path, args: &[String], app: &Path) -> Result<u32, ProcessError> {
    let resolved = resolve(exe, app);
    let application = wide(&resolved.to_string_lossy());
    let mut command = wide(&command_line(&resolved, args));
    let directory = wide(&app.to_string_lossy());

    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..STARTUPINFOW::default()
    };
    let mut info = PROCESS_INFORMATION::default();

    // SAFETY: every pointer is a live, NUL-terminated buffer for the duration of the call, the startup
    // structure declares its own size, the process information is a valid out-parameter and the child
    // inherits none of our handles.
    let started = unsafe {
        CreateProcessW(
            PCWSTR(application.as_ptr()),
            Some(PWSTR(command.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_NO_WINDOW,
            None,
            PCWSTR(directory.as_ptr()),
            &startup,
            &mut info,
        )
    };

    if let Err(source) = started {
        return Err(ProcessError::Spawn {
            exe: exe.to_path_buf(),
            source,
        });
    }

    // SAFETY: both handles came from the successful call above and are closed exactly once; the new
    // process keeps running without them.
    unsafe {
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }

    Ok(info.dwProcessId)
}

/// The command line `CreateProcessW` parses back into `exe` followed by `args`.
///
/// The quoting rules are `CommandLineToArgvW`'s: a space or a tab forces quotes, and a run of
/// backslashes is only doubled in front of a quote that belongs to the value.
pub fn command_line(exe: &Path, args: &[String]) -> String {
    let mut line = quote(&exe.to_string_lossy());
    for argument in args {
        line.push(' ');
        line.push_str(&quote(argument));
    }
    line
}

/// One argument, quoted when it has to be.
fn quote(argument: &str) -> String {
    if !argument.is_empty() && !argument.contains([' ', '\t', '"']) {
        return argument.to_owned();
    }

    let mut quoted = String::with_capacity(argument.len() + 2);
    quoted.push('"');

    let mut backslashes = 0;
    for character in argument.chars() {
        match character {
            '\\' => {
                backslashes += 1;
                quoted.push('\\');
            }
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push('\\');
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                backslashes = 0;
                quoted.push(other);
            }
        }
    }

    // A trailing run must be doubled so the closing quote is not escaped.
    quoted.extend(std::iter::repeat_n('\\', backslashes));
    quoted.push('"');
    quoted
}

/// Resolves `exe` the way a shell would, without ever refusing to try.
fn resolve(exe: &Path, app: &Path) -> PathBuf {
    if exe.is_absolute() {
        return exe.to_path_buf();
    }

    let beside = app.join(exe);
    if beside.is_file() {
        return beside;
    }

    let searched = resolve_in_path(exe, std::env::var_os("PATH").as_deref());
    if searched.is_file() {
        return searched;
    }

    exe.to_path_buf()
}

/// Resolves a bare executable name against `PATH`, appending `.exe` when the name has no extension.
///
/// A name that is already a path is returned unchanged, and a name nothing matches is returned
/// unchanged too, so the failure comes from Windows with the name the caller typed.
fn resolve_in_path(name: &Path, path: Option<&OsStr>) -> PathBuf {
    let text = name.to_string_lossy();

    if name.is_absolute() || text.contains(['\\', '/']) {
        return name.to_path_buf();
    }

    let Some(path) = path else {
        return name.to_path_buf();
    };

    for directory in std::env::split_paths(path) {
        let direct = directory.join(name);
        if direct.is_file() {
            return direct;
        }

        if name.extension().is_none() {
            let with_extension = directory.join(format!("{text}.exe"));
            if with_extension.is_file() {
                return with_extension;
            }
        }
    }

    name.to_path_buf()
}

/// A handle this module owns for the length of one call.
#[derive(Debug)]
struct OwnedHandle(HANDLE);

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle came from `OpenProcess` and is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_quotes_only_what_needs_it() {
        let exe = PathBuf::from(r"C:\Program Files\Neb\ProjectNeb.exe");

        assert_eq!(
            command_line(&exe, &[]),
            r#""C:\Program Files\Neb\ProjectNeb.exe""#
        );
        assert_eq!(
            command_line(&exe, &["--quiet".to_owned()]),
            r#""C:\Program Files\Neb\ProjectNeb.exe" --quiet"#
        );
        assert_eq!(
            command_line(
                &PathBuf::from("cmd.exe"),
                &["/c".to_owned(), "exit 3".to_owned()]
            ),
            r#"cmd.exe /c "exit 3""#
        );
    }

    #[test]
    fn a_trailing_backslash_and_an_embedded_quote_survive_quoting() {
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("plain"), "plain");
    }

    #[test]
    fn a_name_beside_the_application_wins_over_one_on_the_path() {
        let root = tempfile::tempdir().expect("temp dir");
        let app = root.path().join("app");
        let elsewhere = root.path().join("bin");
        std::fs::create_dir_all(&app).expect("app dir");
        std::fs::create_dir_all(&elsewhere).expect("bin dir");
        std::fs::write(app.join("ProjectNeb.exe"), b"").expect("write");
        std::fs::write(elsewhere.join("ProjectNeb.exe"), b"").expect("write");

        assert_eq!(
            resolve(Path::new("ProjectNeb.exe"), &app),
            app.join("ProjectNeb.exe")
        );
    }

    #[test]
    fn a_bare_name_is_found_on_the_path_with_the_extension_added() {
        let root = tempfile::tempdir().expect("temp dir");
        let bin = root.path().join("bin");
        std::fs::create_dir_all(&bin).expect("bin dir");
        std::fs::write(bin.join("neb-tool.exe"), b"").expect("write");

        assert_eq!(
            resolve_in_path(Path::new("neb-tool"), Some(bin.as_os_str())),
            bin.join("neb-tool.exe")
        );
    }

    #[test]
    fn waiting_with_no_timeout_reports_a_live_process_as_still_running() {
        let outcome =
            wait_for_pid(std::process::id(), Duration::ZERO).expect("this process exists");

        assert_eq!(outcome, WaitOutcome::TimedOut);
    }

    #[test]
    fn a_pid_windows_never_knew_is_not_an_error() {
        // 0 is the System Idle Process, which `OpenProcess` always refuses with ERROR_INVALID_PARAMETER.
        let outcome =
            wait_for_pid(0, Duration::from_secs(1)).expect("a missing pid is not a failure");

        assert_eq!(outcome, WaitOutcome::AlreadyGone);
    }
}
