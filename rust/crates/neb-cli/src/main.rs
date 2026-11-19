#![forbid(unsafe_op_in_unsafe_fn)]

//! `neb-cli` — the diagnostics command line for ProjectNeb.
//!
//! Duties: show the configuration exactly as the app reads it, print live CPU and memory, call the
//! running process over a named pipe, and list top-level windows. It deliberately has no argument
//! parser dependency: the command surface is small, fixed and parsed by hand in [`args`], which keeps
//! the resident memory of the tool far below the 8 MB budget.
//!
//! Exit codes are `0` for success and `1` for any failure; `--json` switches a command to a single
//! machine-readable document instead of lines meant for a human.

mod args;
mod commands;

use std::process::ExitCode;

use commands::CliError;

/// What the tool understands.
const USAGE: &str = "\
usage: neb-cli <command> [options]

commands:
  config dump [--json]                        print the config the app would read
  config path [--json]                        print where that config lives
  config validate [--json]                    check the config on disk
  metrics [--count N] [--interval-ms MS] [--json]
                                              CPU and memory samples
  ping <pipe> [--timeout-ms MS] [--json]      call 'ping' on a running process
  windows [--json]                            visible top-level windows
  mem [--json]                                this process's working set
  release --version X.Y.Z --notes TEXT        publish a release, its manifest and changelog
          [--channel beta] [--manifest-repo DIR] [--dry-run]
          [--skip-tests] [--skip-installer]
  help                                        this text

exit codes: 0 success, 1 failure
";

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    match run(&argv) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if matches!(error, CliError::Usage(_)) {
                eprint!("{USAGE}");
            }
            eprintln!("neb-cli: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Dispatches one invocation; `argv` excludes the program name.
fn run(argv: &[String]) -> Result<(), CliError> {
    let Some(command) = argv.first().map(String::as_str) else {
        return Err(CliError::Usage("no command given".to_owned()));
    };
    let rest = &argv[1..];

    match command {
        "help" | "--help" | "-h" => {
            print!("{USAGE}");
            Ok(())
        }
        "config" => commands::config::run(rest),
        "metrics" => commands::metrics::run(rest),
        "ping" => commands::ping::run(rest),
        "windows" => commands::windows::run(rest),
        "mem" => commands::metrics::run_mem(rest),
        "release" => commands::release::run(rest),
        // Diagnostics for the Theme Manager; deliberately absent from the usage text.
        "themes" => commands::themes::run(rest),
        other => Err(CliError::Usage(format!("unknown command '{other}'"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn no_command_is_a_usage_error() {
        assert!(matches!(run(&[]), Err(CliError::Usage(_))));
    }

    #[test]
    fn an_unknown_command_is_a_usage_error() {
        match run(&argv(&["frobnicate"])) {
            Err(CliError::Usage(message)) => assert!(message.contains("frobnicate")),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn help_is_success_and_mentions_every_command() {
        assert!(run(&argv(&["help"])).is_ok());

        for command in ["config", "metrics", "ping", "windows", "mem", "release"] {
            assert!(USAGE.contains(command), "usage must mention {command}");
        }
    }
}
