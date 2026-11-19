#![forbid(unsafe_op_in_unsafe_fn)]

//! `neb-discord` — the Rich Presence process and its diagnostic modes.
//!
//! Without arguments it is the resident process the supervisor starts: it reads `config.json`,
//! serves `ProjectNeb.discord`, and reconnects to Discord in the background.
//!
//! Three switches exist for evidence gathering and are deliberately outside the dashboard's
//! contract:
//!
//! * `--state` prints one `discord.getState` document and exits;
//! * `--once` tries one real connection, prints the state before and after, and exits non-zero when
//!   the result is not `ready`;
//! * `--pipe` and `--app-id` let that attempt target the fake pipe or a placeholder application.

use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use neb_core::config::ConfigStore;
use neb_core::ipc::ServerHandle;
use neb_core::logging;
use neb_discord::ipc;
use neb_discord::service::{Options, PipeSelector, Service};
use serde_json::Value;

const USAGE: &str = "\
usage: neb-discord [--help] [--state] [--once] [--app-id ID] [--pipe NAME] [--timeout-ms MS]

  With no arguments: serve 'ProjectNeb.discord' and keep Discord Rich Presence running.
  --state           print one discord.getState document and exit
  --once            try one connection, print the state, exit 1 unless it became ready
  --app-id ID       use ID instead of discord.appId from config.json
  --pipe NAME       use \\\\.\\pipe\\NAME instead of discord-ipc-0..9 (diagnostics)
  --timeout-ms MS   how long --once waits for READY or an error (default 8000)
  --help            this text
";

/// Milliseconds `--once` waits by default. The handshake itself times out at five seconds, so this
/// leaves room for one pipe attempt plus the state transition.
const DEFAULT_TIMEOUT_MS: u64 = 8_000;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let args = match Args::parse(&argv) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("neb-discord: {error}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if args.help {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    match run(args) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("neb-discord: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<ExitCode, String> {
    let store = ConfigStore::at_default_location();
    let config = store.load();
    let log = logging::init(&neb_core::paths::logs_dir()).ok();

    let mut options = Options::from_config(&config, Some(store), log);
    if let Some(app_id) = args.app_id {
        options.app_id = app_id.trim().to_owned();
    }
    if let Some(pipe) = args.pipe {
        options.pipe = PipeSelector::Named(pipe);
    }

    if args.state {
        options.connect_on_start = false;
    }
    if args.once {
        options.connect_on_start = !options.app_id.is_empty();
    }

    let service = Service::start(options).map_err(|error| error.to_string())?;

    if args.state {
        print_json(&service.state());
        service.shutdown();
        return Ok(ExitCode::SUCCESS);
    }

    if args.once {
        println!("STATE_INITIAL={}", compact(&service.state()));
        let final_state = wait_for_outcome(&service, Duration::from_millis(args.timeout_ms));
        println!("STATE_FINAL={}", compact(&final_state));

        let ready = final_state["state"].as_str() == Some("ready");
        service.shutdown();
        return Ok(if ready {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        });
    }

    let server = ipc::serve(service.handle()).map_err(|error| error.to_string())?;
    wait_forever(&server);
    Ok(ExitCode::SUCCESS)
}

/// Waits until the service is ready or has reported an error.
fn wait_for_outcome(service: &Service, timeout: Duration) -> Value {
    let deadline = Instant::now() + timeout;

    loop {
        let state = service.state();
        let phase = state["state"].as_str().unwrap_or("disconnected");
        if matches!(phase, "ready" | "error") || Instant::now() >= deadline {
            return state;
        }
        thread::sleep(Duration::from_millis(50));
    }
}

/// Parks the resident process. Shutdown is by ending the process or by the supervisor.
fn wait_forever(_server: &ServerHandle) {
    loop {
        thread::park();
    }
}

fn print_json(value: &Value) {
    match serde_json::to_string_pretty(value) {
        Ok(text) => println!("{text}"),
        Err(_) => println!("{value}"),
    }
}

fn compact(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable>".to_owned())
}

/// What the command line asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Args {
    help: bool,
    state: bool,
    once: bool,
    app_id: Option<String>,
    pipe: Option<String>,
    timeout_ms: u64,
}

impl Args {
    fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut args = Self {
            timeout_ms: DEFAULT_TIMEOUT_MS,
            ..Self::default()
        };
        let mut index = 0;

        while index < arguments.len() {
            let argument = arguments[index].as_str();
            index += 1;

            match argument {
                "--help" | "-h" => args.help = true,
                "--state" => args.state = true,
                "--once" => args.once = true,
                "--app-id" => args.app_id = Some(next(arguments, &mut index, argument)?),
                "--pipe" => args.pipe = Some(next(arguments, &mut index, argument)?),
                "--timeout-ms" => {
                    let value = next(arguments, &mut index, argument)?;
                    args.timeout_ms = value
                        .parse()
                        .map_err(|_| format!("{argument} needs a number of milliseconds"))?;
                }
                other => return Err(format!("unknown argument '{other}'")),
            }
        }

        if args.state && args.once {
            return Err("--state and --once are different modes; pick one".to_owned());
        }

        Ok(args)
    }
}

/// The value after `flag`.
fn next(arguments: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    let value = arguments
        .get(*index)
        .cloned()
        .ok_or_else(|| format!("{flag} needs a value"))?;
    *index += 1;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn no_arguments_is_resident_mode() {
        let args = Args::parse(&[]).expect("parse");

        assert!(!args.state);
        assert!(!args.once);
        assert_eq!(args.timeout_ms, DEFAULT_TIMEOUT_MS);
    }

    #[test]
    fn the_diagnostic_switches_parse() {
        let args = Args::parse(&argv(&[
            "--once",
            "--app-id",
            "123",
            "--pipe",
            "discord-ipc-test",
            "--timeout-ms",
            "1500",
        ]))
        .expect("parse");

        assert!(args.once);
        assert_eq!(args.app_id.as_deref(), Some("123"));
        assert_eq!(args.pipe.as_deref(), Some("discord-ipc-test"));
        assert_eq!(args.timeout_ms, 1500);
    }

    #[test]
    fn a_missing_value_or_a_conflicting_mode_is_a_usage_error() {
        assert!(Args::parse(&argv(&["--app-id"])).is_err());
        assert!(Args::parse(&argv(&["--state", "--once"])).is_err());
        assert!(Args::parse(&argv(&["--timeout-ms", "soon"])).is_err());
    }
}
