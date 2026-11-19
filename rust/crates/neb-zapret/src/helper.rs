//! The elevated half: one `winws.exe` per invocation, and its status file.
//!
//! `neb-zapret.exe --run <strategy> --pack <dir> --status <file>` starts the chosen strategy,
//! writes `status.json`, waits for `winws.exe` to end, and writes the final status. The manager
//! deliberately cannot do this itself: creating the WinDivert service needs administrator rights.
//!
//! `neb-zapret.exe --stop --pack <dir> --status <file>` is the other half. It marks the status as
//! stop-requested, terminates the recorded pid and writes the stopped document. Because the run
//! helper re-reads the status before its final write, a deliberate stop is never reported as a
//! crash with the exit code `TerminateProcess` happens to produce.

use std::io;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use neb_core::config::ConfigStore;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;

use crate::host;
use crate::pack::{Pack, PackError};
use crate::paths::Paths;
use crate::settings;
use crate::status::{self, StatusFile};
use crate::strategy::{self, Strategy, StrategyError};

/// Why the helper could not do its job.
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    /// No data directory, so neither the config nor the status can be found.
    #[error(transparent)]
    Paths(#[from] neb_core::paths::PathsError),

    /// The pack is not usable.
    #[error(transparent)]
    Pack(#[from] PackError),

    /// The strategy scripts could not be read.
    #[error(transparent)]
    Strategy(#[from] StrategyError),

    /// The strategy is not in the pack.
    #[error("strategy '{strategy}' is not in this pack; available: {available}")]
    UnknownStrategy {
        /// The requested name.
        strategy: String,
        /// The names that do exist.
        available: String,
    },

    /// `winws.exe` needs an elevated token and this helper does not have one.
    #[error("winws.exe needs administrator rights; run the helper elevated")]
    NeedsAdmin,

    /// A file operation failed.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation.
        action: &'static str,
        /// File the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The child process could not be created.
    #[error("cannot start '{}': {source}", path.display())]
    Spawn {
        /// The executable that failed to start.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The status file could not be written or read.
    #[error("cannot use '{}': {source}", path.display())]
    Status {
        /// The status file.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// `winws.exe` could not be terminated.
    #[error("cannot terminate winws.exe (pid {pid}): {source}")]
    Terminate {
        /// The pid the status file recorded.
        pid: u32,
        /// The underlying host error.
        #[source]
        source: crate::host::HostError,
    },
}

/// Starts `strategy` from the pack and waits for `winws.exe` to end.
///
/// The final status is the return value and is also on disk, so a manager that was not watching
/// still sees the outcome. A failure before `winws.exe` starts is written as a stopped status with
/// [`StatusFile::last_error`] set, which is what makes a cancelled UAC prompt visible.
///
/// # Errors
///
/// [`HelperError`] for every failure; the status file is written first whenever its path is known.
pub fn run(
    strategy_name: &str,
    pack: Option<&Path>,
    status_path: Option<&Path>,
) -> Result<StatusFile, HelperError> {
    let fallback_status = status_path
        .map(Path::to_path_buf)
        .or_else(|| Paths::try_default().ok().map(|paths| paths.status_file));

    let context = match Context::resolve(pack, fallback_status.as_deref()) {
        Ok(context) => context,
        Err(error) => {
            if let Some(path) = &fallback_status {
                let failed = StatusFile {
                    strategy: strategy_name.to_owned(),
                    running: false,
                    pid: None,
                    last_error: Some(error.to_string()),
                    message: error.to_string(),
                    ..StatusFile::default()
                };
                let _ = status::write(path, &failed);
            }

            return Err(error);
        }
    };

    match run_inner(strategy_name, &context) {
        Ok(status) => Ok(status),
        Err(error) => {
            let failed = StatusFile {
                strategy: strategy_name.to_owned(),
                running: false,
                pid: None,
                elevated: context.elevated,
                pack_version: context.pack_version.clone(),
                last_error: Some(error.to_string()),
                message: error.to_string(),
                ..StatusFile::default()
            };
            let _ = status::write(&context.status_path, &failed);
            Err(error)
        }
    }
}

/// Stops the process recorded in the status file, if there is one.
///
/// # Errors
///
/// [`HelperError::NeedsAdmin`] when the helper is not elevated and the pid needs it, and
/// [`HelperError::Status`] when the status file cannot be read or written.
pub fn stop(pack: Option<&Path>, status_path: Option<&Path>) -> Result<StatusFile, HelperError> {
    let context = Context::resolve(pack, status_path)?;

    if !context.elevated {
        return Err(HelperError::NeedsAdmin);
    }

    let mut current = status::read(&context.status_path)
        .map_err(|source| HelperError::Status {
            path: context.status_path.clone(),
            source,
        })?
        .unwrap_or_default();

    let Some(pid) = current.pid.filter(|_| current.running) else {
        current.running = false;
        current.pid = None;
        current.message = "winws.exe is not running".to_owned();
        write_status(&context.status_path, &current)?;
        return Ok(current);
    };

    // Mark the stop first: the run helper re-reads this before its final write and then does not
    // call a deliberate termination a crash.
    current.stop_requested = true;
    write_status(&context.status_path, &current)?;

    host::terminate(pid).map_err(|source| HelperError::Terminate { pid, source })?;

    current.running = false;
    current.pid = None;
    current.exit_code = None;
    current.last_error = None;
    current.message = format!("winws.exe (pid {pid}) stopped");
    write_status(&context.status_path, &current)?;

    Ok(current)
}

/// Everything the worker needs, resolved once.
#[derive(Debug)]
struct Context {
    pack: Pack,
    status_path: PathBuf,
    elevated: bool,
    pack_version: String,
}

impl Context {
    fn resolve(pack: Option<&Path>, status_path: Option<&Path>) -> Result<Self, HelperError> {
        let paths = Paths::try_default();

        let status_path = match status_path {
            Some(path) => path.to_path_buf(),
            None => paths
                .as_ref()
                .map_err(|error| HelperError::Paths(*error))?
                .status_file
                .clone(),
        };

        let pack_path = match pack {
            Some(path) => Some(path.to_path_buf()),
            None => {
                let paths = paths.as_ref().map_err(|error| HelperError::Paths(*error))?;
                let config = ConfigStore::new(&paths.config_file).load();
                settings::pack_path(None, &config, paths)
            }
        };

        let Some(pack_path) = pack_path else {
            return Err(HelperError::Io {
                action: "find",
                path: paths
                    .map(|paths| paths.data_dir)
                    .unwrap_or_else(|_| PathBuf::from("zapret")),
                source: io::Error::new(
                    io::ErrorKind::NotFound,
                    "no pack is imported; run `neb-zapret --import <folder>` first",
                ),
            });
        };

        let pack = Pack::open(pack_path)?;
        let pack_version = pack.version.clone();

        Ok(Self {
            pack,
            status_path,
            elevated: host::is_admin(),
            pack_version,
        })
    }
}

fn run_inner(strategy_name: &str, context: &Context) -> Result<StatusFile, HelperError> {
    let strategies = strategy::load(&context.pack)?;
    let strategy = pick(&strategies.strategies, strategy_name).ok_or_else(|| {
        let available = strategies
            .strategies
            .iter()
            .map(|strategy| strategy.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        HelperError::UnknownStrategy {
            strategy: strategy_name.to_owned(),
            available,
        }
    })?;

    if !context.elevated {
        return Err(HelperError::NeedsAdmin);
    }

    let mut command = Command::new(&strategy.program);
    command.args(&strategy.args);
    command.current_dir(context.pack.bin_dir());
    command.creation_flags(CREATE_NO_WINDOW.0);

    let mut child = command.spawn().map_err(|source| HelperError::Spawn {
        path: strategy.program.clone(),
        source,
    })?;
    let pid = child.id();

    let started = StatusFile::started(pid, &strategy.name, &context.pack_version, context.elevated);
    write_status(&context.status_path, &started)?;

    let exit = child.wait();
    let previous = status::read(&context.status_path)
        .ok()
        .flatten()
        .unwrap_or_default();
    let stop_requested = previous.stop_requested;
    let exit_code = exit.as_ref().ok().and_then(|status| status.code());

    let mut final_status = started;
    final_status.running = false;
    final_status.pid = None;
    final_status.exit_code = exit_code;
    final_status.stop_requested = stop_requested;
    final_status.last_error = if stop_requested {
        None
    } else {
        match (exit, exit_code) {
            (Err(error), _) => Some(format!("cannot wait for winws.exe: {error}")),
            (Ok(_), Some(0)) | (Ok(_), None) => None,
            (Ok(_), Some(code)) => Some(format!("winws.exe exited with code {code}")),
        }
    };
    final_status.message = if stop_requested {
        "winws.exe stopped".to_owned()
    } else if let Some(error) = &final_status.last_error {
        error.clone()
    } else {
        "winws.exe exited".to_owned()
    };

    write_status(&context.status_path, &final_status)?;
    Ok(final_status)
}

fn pick<'a>(strategies: &'a [Strategy], name: &str) -> Option<&'a Strategy> {
    strategies
        .iter()
        .find(|strategy| strategy.name == name)
        .or_else(|| {
            strategies
                .iter()
                .find(|strategy| strategy.name.eq_ignore_ascii_case(name.trim()))
        })
}

fn write_status(path: &Path, status: &StatusFile) -> Result<(), HelperError> {
    status::write(path, status).map_err(|source| HelperError::Status {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{BIN_DIR, LISTS_DIR, WINDIVERT_SYS, WINWS_EXE};
    use std::fs;
    use tempfile::tempdir;

    fn fake_pack(root: &Path) {
        fs::create_dir_all(root.join(BIN_DIR)).expect("bin");
        fs::create_dir_all(root.join(LISTS_DIR)).expect("lists");
        fs::write(root.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        fs::write(root.join(BIN_DIR).join(WINDIVERT_SYS), b"sys").expect("driver");
        fs::write(
            root.join("general.bat"),
            "@echo off\r\nstart \"\" \"%~dp0bin\\winws.exe\" --new\r\n",
        )
        .expect("strategy");
    }

    #[test]
    fn picking_a_strategy_is_exact_then_case_insensitive() {
        let strategies = vec![Strategy {
            name: "general (ALT)".to_owned(),
            file: "general (ALT).bat".to_owned(),
            program: PathBuf::from("winws.exe"),
            args: Vec::new(),
            argument_count: 0,
        }];

        assert!(pick(&strategies, "general (ALT)").is_some());
        assert!(pick(&strategies, " general (alt) ").is_some());
        assert!(pick(&strategies, "general").is_none());
    }

    #[test]
    fn a_pack_without_the_requested_strategy_names_the_ones_it_has() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path().join("pack");
        fake_pack(&root);
        let status = directory.path().join("status.json");

        let error = run("missing", Some(&root), Some(&status)).expect_err("must fail");

        assert!(
            error.to_string().contains("general") && error.to_string().contains("missing"),
            "{error}"
        );
        let written = status::read(&status).expect("read").expect("a status");
        assert!(!written.running);
        assert!(written.last_error.is_some());
    }

    #[test]
    fn a_pack_without_a_winws_binary_reports_the_missing_file() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path().join("pack");
        fs::create_dir_all(root.join(BIN_DIR)).expect("bin");
        fs::write(root.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        let status = directory.path().join("status.json");

        let error = run("general", Some(&root), Some(&status)).expect_err("must fail");

        assert!(error.to_string().contains(WINDIVERT_SYS), "{error}");
        assert!(
            status::read(&status).expect("read").is_some(),
            "a pre-spawn failure is written for the manager"
        );
    }

    #[test]
    fn stopping_without_a_status_says_nothing_was_running() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path().join("pack");
        fake_pack(&root);
        let status = directory.path().join("status.json");

        if host::is_admin() {
            let stopped = stop(Some(&root), Some(&status)).expect("stop");
            assert!(!stopped.running);
            assert!(stopped.message.contains("not running"));
        } else {
            assert!(matches!(
                stop(Some(&root), Some(&status)),
                Err(HelperError::NeedsAdmin)
            ));
        }
    }

    #[test]
    fn a_status_without_a_pid_is_already_stopped() {
        let directory = tempdir().expect("temp dir");
        let status = directory.path().join("status.json");
        status::write(&status, &StatusFile::default()).expect("write");

        if host::is_admin() {
            let stopped = stop(None, Some(&status)).expect("stop");
            assert_eq!(stopped.pid, None);
            assert!(!stopped.running);
        }
    }
}
