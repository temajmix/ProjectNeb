//! The command line, parsed by hand.
//!
//! The whole surface is six options, so a parser dependency would cost more than it saves — and this
//! process is expected to stay tiny enough to replace files while a desktop is running.

use std::path::PathBuf;
use std::time::Duration;

/// How long the supervisor gets to exit when `--timeout-secs` is not given.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);

/// What the tool understands.
pub const USAGE: &str = "\
usage: neb-updater.exe --wait-pid <pid> --staging <dir> --app <dir> --restart <exe> [options]

options:
  --wait-pid <pid>        process to wait for, usually the supervisor
  --staging <dir>         directory holding the verified update
  --app <dir>             installation directory to copy into
  --restart <exe>         executable to start once the copy is done
  --restart-arg <value>   one argument for that executable; repeat for more
  --timeout-secs <secs>   how long to wait for the pid (default: 60, 0 checks once)
  --log <file>            log file (default: <app>\\..\\logs\\updater-<unix>.log)
  --help                  this text

exit codes: 0 success, 1 failure, 2 the pid did not exit in time (nothing was copied)
";

/// Everything the command line can say.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// `--wait-pid`: the process to wait for.
    pub wait_pid: u32,
    /// `--staging`: where the verified update waits.
    pub staging: PathBuf,
    /// `--app`: the installation directory.
    pub app: PathBuf,
    /// `--restart`: the executable to start afterwards.
    pub restart: PathBuf,
    /// `--restart-arg`: arguments for [`Options::restart`].
    pub restart_args: Vec<String>,
    /// `--timeout-secs`: how long the pid gets, possibly zero.
    pub timeout: Duration,
    /// `--log`: the file to write, or `None` for the default location.
    pub log: Option<PathBuf>,
}

/// The result of a successful parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parsed {
    /// A complete command line.
    Run(Options),
    /// `--help`: print [`USAGE`] and exit 0.
    Help,
}

/// Why a command line was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ArgsError {
    /// The command line does not make sense.
    #[error("{0}")]
    Usage(String),
}

impl Options {
    /// Parses the arguments that follow the program name.
    ///
    /// A repeated option keeps the last value, which is how a shell's `alias` habit behaves.
    ///
    /// # Errors
    ///
    /// [`ArgsError::Usage`] for an unknown option, a missing value, a value of the wrong shape, or a
    /// missing required option.
    pub fn parse(argv: &[String]) -> Result<Parsed, ArgsError> {
        let mut wait_pid = None;
        let mut staging = None;
        let mut app = None;
        let mut restart = None;
        let mut restart_args = Vec::new();
        let mut timeout = DEFAULT_TIMEOUT;
        let mut log = None;
        let mut help = false;
        let mut index = 0;

        while index < argv.len() {
            let argument = argv[index].clone();
            index += 1;

            match argument.as_str() {
                "--help" | "-h" => help = true,
                "--wait-pid" => {
                    let value = value("--wait-pid", argv, &mut index)?;
                    wait_pid = Some(positive_pid(value)?);
                }
                "--staging" => staging = Some(PathBuf::from(value("--staging", argv, &mut index)?)),
                "--app" => app = Some(PathBuf::from(value("--app", argv, &mut index)?)),
                "--restart" => restart = Some(PathBuf::from(value("--restart", argv, &mut index)?)),
                "--restart-arg" => {
                    restart_args.push(value("--restart-arg", argv, &mut index)?.to_owned());
                }
                "--timeout-secs" => {
                    timeout =
                        Duration::from_secs(seconds(value("--timeout-secs", argv, &mut index)?)?);
                }
                "--log" => log = Some(PathBuf::from(value("--log", argv, &mut index)?)),
                other if other.starts_with("--") => {
                    return Err(ArgsError::Usage(format!("unknown option '{other}'")));
                }
                other => {
                    return Err(ArgsError::Usage(format!("unexpected argument '{other}'")));
                }
            }
        }

        if help {
            return Ok(Parsed::Help);
        }

        let wait_pid = wait_pid.ok_or_else(|| missing("--wait-pid"))?;
        let staging = staging.ok_or_else(|| missing("--staging"))?;
        let app = app.ok_or_else(|| missing("--app"))?;
        let restart = restart.ok_or_else(|| missing("--restart"))?;

        Ok(Parsed::Run(Self {
            wait_pid,
            staging,
            app,
            restart,
            restart_args,
            timeout,
            log,
        }))
    }
}

/// Reads the value of an option, which must be present.
fn value<'a>(option: &str, argv: &'a [String], index: &mut usize) -> Result<&'a str, ArgsError> {
    let value = argv
        .get(*index)
        .ok_or_else(|| ArgsError::Usage(format!("{option} needs a value")))?;
    *index += 1;

    Ok(value)
}

/// Reads a process id, which must be a number Windows could hand out.
fn positive_pid(value: &str) -> Result<u32, ArgsError> {
    match value.parse::<u32>() {
        Ok(pid) if pid > 0 => Ok(pid),
        _ => Err(ArgsError::Usage(format!(
            "--wait-pid expects a process id, got '{value}'"
        ))),
    }
}

/// Reads a non-negative number of seconds; zero is meaningful and means "check once".
fn seconds(value: &str) -> Result<u64, ArgsError> {
    value.parse::<u64>().map_err(|_| {
        ArgsError::Usage(format!(
            "--timeout-secs expects a number of seconds, got '{value}'"
        ))
    })
}

/// The error for a required option that was not given.
fn missing(option: &str) -> ArgsError {
    ArgsError::Usage(format!("{option} is required"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    fn run(items: &[&str]) -> Options {
        match Options::parse(&argv(items)) {
            Ok(Parsed::Run(options)) => options,
            other => panic!("{items:?} must parse into a run: {other:?}"),
        }
    }

    fn refused(items: &[&str]) -> bool {
        matches!(Options::parse(&argv(items)), Err(ArgsError::Usage(_)))
    }

    /// Every required flag, as the supervisor spells them.
    const FULL: [&str; 8] = [
        "--wait-pid",
        "4242",
        "--staging",
        r"C:\Neb\staging",
        "--app",
        r"C:\Neb\app",
        "--restart",
        r"C:\Neb\app\ProjectNeb.exe",
    ];

    #[test]
    fn every_flag_is_parsed() {
        let mut arguments = FULL.to_vec();
        arguments.extend([
            "--timeout-secs",
            "15",
            "--log",
            r"C:\Neb\logs\update.log",
            "--restart-arg",
            "--quiet",
        ]);

        let options = run(&arguments);

        assert_eq!(options.wait_pid, 4242);
        assert_eq!(options.staging, PathBuf::from(r"C:\Neb\staging"));
        assert_eq!(options.app, PathBuf::from(r"C:\Neb\app"));
        assert_eq!(options.restart, PathBuf::from(r"C:\Neb\app\ProjectNeb.exe"));
        assert_eq!(options.restart_args, vec!["--quiet".to_owned()]);
        assert_eq!(options.timeout, Duration::from_secs(15));
        assert_eq!(options.log, Some(PathBuf::from(r"C:\Neb\logs\update.log")));
    }

    #[test]
    fn the_default_timeout_and_log_match_the_documented_behaviour() {
        let options = run(&FULL);

        assert_eq!(options.timeout, DEFAULT_TIMEOUT);
        assert_eq!(options.log, None);
        assert!(options.restart_args.is_empty());
    }

    #[test]
    fn a_zero_timeout_is_allowed_and_means_check_once() {
        let mut arguments = FULL.to_vec();
        arguments.extend(["--timeout-secs", "0"]);

        assert_eq!(run(&arguments).timeout, Duration::ZERO);
    }

    #[test]
    fn every_required_flag_is_required() {
        for missing in ["--wait-pid", "--staging", "--app", "--restart"] {
            let arguments: Vec<&str> = FULL
                .chunks(2)
                .filter(|pair| pair[0] != missing)
                .flatten()
                .copied()
                .collect();

            assert!(refused(&arguments), "{missing} must be required");
        }
        assert!(refused(&[]));
    }

    #[test]
    fn a_bad_value_is_refused() {
        for arguments in [
            vec!["--wait-pid", "0"],
            vec!["--wait-pid", "-1"],
            vec!["--wait-pid", "soon"],
            vec!["--timeout-secs", "-5"],
            vec!["--timeout-secs", "later"],
            vec!["--staging"],
            vec!["--log"],
            vec!["--restart-arg"],
        ] {
            assert!(refused(&arguments), "{arguments:?} must be refused");
        }
    }

    #[test]
    fn an_unknown_option_and_a_stray_argument_are_refused() {
        assert!(refused(&["--verbose"]));
        assert!(refused(&["update"]));
    }

    #[test]
    fn help_wins_over_a_missing_command() {
        assert_eq!(
            Options::parse(&argv(&["--help"])).expect("help parses"),
            Parsed::Help
        );
    }
}
