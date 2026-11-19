#![forbid(unsafe_op_in_unsafe_fn)]

//! `ProjectNeb` — the single entry point and resident process manager for the Rust build.
//!
//! It owns `neb-shell.exe`, `neb-wallpaper.exe` and `neb-app.exe`: it starts what the configuration
//! enables, opens the main window, restarts a crash no sooner than the throttle allows, repairs a
//! taskbar a dead shell left hidden, and answers on `ProjectNeb.supervisor`. A second launch does
//! not start a second copy: it asks the running instance to focus the window and exits.
//!
//! Two diagnostics modes ride along, because a resident process is hard to watch otherwise:
//! `--soak <minutes> --interval <seconds>` writes a CSV of every `neb-*` working set and prints a
//! summary, and `--soak-check` fails when something grew by more than a fifth. `--spawn-test`
//! supervises one arbitrary command instead of the configured children, which is how the restart
//! policy is exercised without a crash.
//!
//! No async runtime, no GUI, no WebView2: a plain loop over `WaitForSingleObject` and a named pipe.

mod children;
mod settings;
mod soak;
mod supervisor;
mod taskbar_repair;
mod tray;
mod update;
mod windows_mode;

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use neb_core::ipc;
use neb_core::strings::{self, Language, Text};
use serde_json::Value;

use supervisor::{Reporter, Supervisor};

/// What the tool understands.
const USAGE: &str = "\
usage: ProjectNeb [options]

options:
  --no-settings, --tray-only  start the children without opening the main window (autostart)
  --open-settings             open or focus the main window on a running instance
  --shell <path>              shell executable (default: neb-shell.exe next to this one)
  --wallpaper <path>          wallpaper executable (default: neb-wallpaper.exe next to this one)
  --soak <minutes>            start the children and sample every neb-* process
  --interval <seconds>        pause between soak samples (default: 10)
  --soak-check                exit 1 when a process grew more than 20 % during the soak
  --spawn-test <command...>   supervise one command instead of the configured children
  --for <seconds>             stop after that long instead of running until stopped
  --report-memory             print this process's working set and exit
  --no-tray                   do not create the notification icon
  --no-updates                do not check the update manifest
  --quiet                     keep transitions out of stdout
  --help                      this text

exit codes: 0 success, 1 failure (a usage error, an unreachable running instance or a failed
soak check)
";

/// How long a second launch waits for the running supervisor to answer `app.openSettings`.
const SECOND_LAUNCH_TIMEOUT: Duration = Duration::from_secs(2);

/// Why an invocation could not run.
#[derive(Debug, thiserror::Error)]
enum CliError {
    /// The command line does not make sense.
    #[error("{0}")]
    Usage(String),

    /// The supervisor could not start.
    #[error(transparent)]
    Supervisor(#[from] supervisor::SupervisorError),

    /// The soak could not run or could not write its CSV.
    #[error(transparent)]
    Soak(#[from] soak::SoakError),
}

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match run(&argv) {
        Ok(code) => code,
        Err(error) => {
            if matches!(error, CliError::Usage(_)) {
                eprint!("{USAGE}");
            }
            eprintln!("ProjectNeb: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Runs one invocation; `argv` excludes the program name.
fn run(argv: &[String]) -> Result<ExitCode, CliError> {
    let options = Options::parse(argv)?;

    if options.help {
        print!("{USAGE}");
        return Ok(ExitCode::SUCCESS);
    }

    if options.report_memory {
        report_memory();
        return Ok(ExitCode::SUCCESS);
    }

    if options.open_settings {
        return Ok(open_settings_on_a_running_instance(&options));
    }

    let diagnostic = options.soak_minutes.is_some() || options.spawn_test.is_some();

    let mut supervisor = match Supervisor::start(supervisor::Options {
        shell: options.shell,
        wallpaper: options.wallpaper,
        command: options.spawn_test.map(|mut command| {
            let exe = PathBuf::from(command.remove(0));
            (exe, command)
        }),
        echo: !options.quiet,
        tray: options.tray,
        updates: options.updates,
        ..supervisor::Options::default()
    })? {
        Some(supervisor) => supervisor,
        // Another supervisor owns the session. Its window is the one the user asked for, so this
        // process forwards the request and leaves instead of becoming a second resident copy.
        None => {
            return Ok(forward_to_running(|| {
                settings::request(SECOND_LAUNCH_TIMEOUT)
            }));
        }
    };

    if options.settings && !diagnostic {
        supervisor.open_settings();
    }

    if let Some(minutes) = options.soak_minutes {
        let outcome = soak::run(
            &mut supervisor,
            Duration::from_secs(minutes * 60),
            Duration::from_secs(options.interval),
            language(),
        )?;
        supervisor.shutdown();

        return Ok(if options.soak_check && !outcome.passed() {
            ExitCode::FAILURE
        } else {
            ExitCode::SUCCESS
        });
    }

    match options.run_for {
        Some(seconds) => supervisor.pump(Duration::from_secs(seconds)),
        None => supervisor.run(),
    }
    supervisor.shutdown();

    Ok(ExitCode::SUCCESS)
}

/// Forwards the second launch to the instance that owns the mutex.
///
/// Success means the running supervisor took `app.openSettings`; a transport failure is reported
/// with the reason and exits 1, because leaving silently would look like the window opened.
fn forward_outcome(attempt: impl FnOnce() -> Result<Value, ipc::IpcError>) -> Result<(), String> {
    attempt().map(|_| ()).map_err(|error| {
        format!("another instance is running but did not accept 'app.openSettings': {error}")
    })
}

/// The second-launch path: focus the existing window and leave.
fn forward_to_running(attempt: impl FnOnce() -> Result<Value, ipc::IpcError>) -> ExitCode {
    match forward_outcome(attempt) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            Reporter::new(true).error(&reason);
            ExitCode::FAILURE
        }
    }
}

/// `--open-settings`: open the window on a running instance, or start it directly when no
/// supervisor answers.
fn open_settings_on_a_running_instance(options: &Options) -> ExitCode {
    let reporter = Reporter::new(!options.quiet);

    match settings::request(settings::OPEN_TIMEOUT) {
        Ok(_) => {
            reporter.info("app: the running supervisor was asked to open the main window");
            return ExitCode::SUCCESS;
        }
        Err(error) => reporter.info(&format!(
            "app: no running supervisor answered ({error}), starting the window directly"
        )),
    }

    let Some(exe) = settings::app_exe() else {
        reporter.error("app: neb-app.exe is not next to ProjectNeb.exe");
        return ExitCode::FAILURE;
    };

    match settings::spawn(&exe) {
        Ok(()) => ExitCode::SUCCESS,
        Err(reason) => {
            reporter.error(&format!("app: {reason}"));
            ExitCode::FAILURE
        }
    }
}

/// Prints this process's working set, the number the 10 MB budget is about.
fn report_memory() {
    let language = language();
    let label = strings::get(language, Text::SupervisorMemory);

    match neb_core::metrics::working_set_bytes() {
        Some(bytes) => println!("{label}: {:.2} MB", bytes as f64 / (1024.0 * 1024.0)),
        None => println!("{label}: unavailable"),
    }
}

/// The language the user's config asks for; the shipped default when it cannot be read.
fn language() -> Language {
    let tag = std::fs::read(neb_core::paths::config_file())
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|config| {
            config
                .get("language")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        });

    Language::from_tag(tag.as_deref().unwrap_or(""))
}

/// Everything the command line can say.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Options {
    /// `--shell`: the bar to run.
    shell: Option<PathBuf>,
    /// `--wallpaper`: the wallpaper to run.
    wallpaper: Option<PathBuf>,
    /// `--no-settings` / `--tray-only`: start the children without the main window.
    settings: bool,
    /// `--open-settings`: open or focus the window on the running instance.
    open_settings: bool,
    /// `--soak`: how long to sample, in minutes.
    soak_minutes: Option<u64>,
    /// `--interval`: seconds between samples.
    interval: u64,
    /// `--soak-check`: fail the run when something grew past the limit.
    soak_check: bool,
    /// `--spawn-test`: one command to supervise instead of the children.
    spawn_test: Option<Vec<String>>,
    /// `--for`: how long to run, in seconds.
    run_for: Option<u64>,
    /// `--report-memory`: print the working set and exit.
    report_memory: bool,
    /// `--no-tray`: no notification icon.
    tray: bool,
    /// `--no-updates`: no update checks.
    updates: bool,
    /// `--quiet`: no transitions on stdout.
    quiet: bool,
    /// `--help`.
    help: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            shell: None,
            wallpaper: None,
            settings: true,
            open_settings: false,
            soak_minutes: None,
            interval: soak::DEFAULT_INTERVAL.as_secs(),
            soak_check: false,
            spawn_test: None,
            run_for: None,
            report_memory: false,
            tray: true,
            updates: true,
            quiet: false,
            help: false,
        }
    }
}

impl Options {
    /// Parses the arguments that follow the program name.
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] for an unknown option, a missing value, a value that is not a positive
    /// number, or a stray argument.
    fn parse(argv: &[String]) -> Result<Self, CliError> {
        let mut options = Self::default();
        let mut index = 0;

        while index < argv.len() {
            let argument = argv[index].clone();
            index += 1;

            match argument.as_str() {
                "--help" | "-h" => options.help = true,
                "--quiet" => options.quiet = true,
                "--no-tray" => options.tray = false,
                "--no-updates" => options.updates = false,
                "--no-settings" | "--tray-only" => options.settings = false,
                "--open-settings" => options.open_settings = true,
                "--soak-check" => options.soak_check = true,
                "--report-memory" => options.report_memory = true,
                "--shell" => {
                    options.shell = Some(PathBuf::from(value("--shell", argv, &mut index)?))
                }
                "--wallpaper" => {
                    options.wallpaper =
                        Some(PathBuf::from(value("--wallpaper", argv, &mut index)?));
                }
                "--soak" => options.soak_minutes = Some(positive("--soak", argv, &mut index)?),
                "--interval" => options.interval = positive("--interval", argv, &mut index)?,
                "--for" => options.run_for = Some(positive("--for", argv, &mut index)?),
                "--spawn-test" => {
                    let command: Vec<String> = argv[index..].to_vec();
                    index = argv.len();

                    if command.is_empty() {
                        return Err(CliError::Usage(
                            "--spawn-test needs a command, e.g. --spawn-test cmd /c exit 3"
                                .to_owned(),
                        ));
                    }

                    options.spawn_test = Some(command);
                }
                other if other.starts_with("--") => {
                    return Err(CliError::Usage(format!("unknown option '{other}'")));
                }
                other => {
                    return Err(CliError::Usage(format!("unexpected argument '{other}'")));
                }
            }
        }

        Ok(options)
    }
}

/// Reads the value of an option, which must be present.
fn value<'a>(option: &str, argv: &'a [String], index: &mut usize) -> Result<&'a str, CliError> {
    let value = argv
        .get(*index)
        .ok_or_else(|| CliError::Usage(format!("{option} needs a value")))?;
    *index += 1;

    Ok(value)
}

/// Reads the value of an option that must be a positive number.
fn positive(option: &str, argv: &[String], index: &mut usize) -> Result<u64, CliError> {
    let value = value(option, argv, index)?;

    match value.parse::<u64>() {
        Ok(number) if number > 0 => Ok(number),
        _ => Err(CliError::Usage(format!(
            "{option} expects a positive number, got '{value}'"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn the_defaults_match_the_documented_behaviour() {
        let options = Options::parse(&argv(&[])).expect("parse");

        assert_eq!(options, Options::default());
        assert_eq!(options.interval, 10);
        assert!(options.soak_minutes.is_none());
        assert!(!options.quiet);
        assert!(options.tray, "the shipped run has a notification icon");
        assert!(options.updates);
        assert!(options.settings, "a normal launch opens the main window");
        assert!(!options.open_settings);
    }

    #[test]
    fn the_window_can_be_skipped_for_autostart_and_tray_only_is_an_alias() {
        let hidden = Options::parse(&argv(&["--no-settings"])).expect("parse");
        assert!(!hidden.settings);
        assert!(hidden.tray, "the tray still appears");

        let alias = Options::parse(&argv(&["--tray-only"])).expect("parse");
        assert!(!alias.settings);
        assert_eq!(alias, hidden);
    }

    #[test]
    fn open_settings_is_its_own_mode() {
        let options = Options::parse(&argv(&["--open-settings"])).expect("parse");

        assert!(options.open_settings);
        assert!(
            options.settings,
            "the flag does not change the default launch"
        );
    }

    #[test]
    fn a_second_launch_forwards_and_exits_zero_only_when_the_instance_answers() {
        let focused = forward_outcome(|| Ok(serde_json::json!({ "ok": true })));
        assert_eq!(focused, Ok(()));

        let failed = forward_outcome(|| {
            Err(ipc::IpcError::Unavailable {
                pipe: supervisor::PIPE.to_owned(),
            })
        });
        let reason = failed.expect_err("a dead pipe is a failure");
        assert!(reason.contains("app.openSettings"), "{reason}");
    }

    #[test]
    fn the_tray_and_the_updater_can_be_turned_off() {
        let options = Options::parse(&argv(&["--no-tray", "--no-updates"])).expect("parse");

        assert!(!options.tray);
        assert!(!options.updates);
    }

    #[test]
    fn a_soak_is_configured_by_minutes_and_seconds() {
        let options = Options::parse(&argv(&["--soak", "1", "--interval", "10", "--soak-check"]))
            .expect("parse");

        assert_eq!(options.soak_minutes, Some(1));
        assert_eq!(options.interval, 10);
        assert!(options.soak_check);
    }

    #[test]
    fn spawn_test_takes_everything_that_follows() {
        let options = Options::parse(&argv(&[
            "--for",
            "75",
            "--spawn-test",
            "cmd",
            "/c",
            "exit 3",
        ]))
        .expect("parse");

        assert_eq!(options.run_for, Some(75));
        assert_eq!(
            options.spawn_test,
            Some(vec!["cmd".to_owned(), "/c".to_owned(), "exit 3".to_owned()])
        );
    }

    #[test]
    fn executables_can_be_overridden() {
        let options = Options::parse(&argv(&[
            "--shell",
            r"C:\build\neb-shell.exe",
            "--wallpaper",
            r"C:\build\neb-wallpaper.exe",
        ]))
        .expect("parse");

        assert_eq!(
            options.shell,
            Some(PathBuf::from(r"C:\build\neb-shell.exe"))
        );
        assert_eq!(
            options.wallpaper,
            Some(PathBuf::from(r"C:\build\neb-wallpaper.exe"))
        );
    }

    #[test]
    fn an_unknown_option_and_a_stray_argument_are_refused() {
        assert!(matches!(
            Options::parse(&argv(&["--verbose"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            Options::parse(&argv(&["soak"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn a_missing_or_non_positive_value_is_refused() {
        for arguments in [
            vec!["--soak"],
            vec!["--soak", "0"],
            vec!["--soak", "soon"],
            vec!["--interval", "0"],
            vec!["--for", "-5"],
            vec!["--shell"],
        ] {
            assert!(
                matches!(Options::parse(&argv(&arguments)), Err(CliError::Usage(_))),
                "{arguments:?} must be refused"
            );
        }
    }

    #[test]
    fn spawn_test_without_a_command_is_refused() {
        assert!(matches!(
            Options::parse(&argv(&["--spawn-test"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn help_mentions_every_mode() {
        for switch in [
            "--shell",
            "--soak",
            "--soak-check",
            "--spawn-test",
            "--report-memory",
            "--no-settings",
            "--tray-only",
            "--open-settings",
        ] {
            assert!(USAGE.contains(switch), "usage must mention {switch}");
        }
    }
}
