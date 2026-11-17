//! Argument parsing, by hand.
//!
//! The surface is a handful of options, so a parser crate would cost more binary and one more
//! dependency than it saves. Unknown options and missing or non-numeric values are usage errors,
//! reported with exit code 1.

use crate::commands::CliError;

/// Samples [`metrics`](crate::commands::metrics) takes when `--count` is not given.
///
/// Two, not one: the very first CPU reading of a process has no interval behind it and the command
/// takes that one as a throwaway, so a single reported sample would have nothing to compare against.
pub const DEFAULT_COUNT: u32 = 2;

/// Most samples a single `metrics` run may ask for.
///
/// A sample costs a millisecond of sleep at the very least, and the samples are collected into one
/// document; the ceiling keeps `--count 4000000000` a usage error instead of an allocation failure.
pub const MAX_COUNT: u32 = 1000;

/// Pause between samples when `--interval-ms` is not given.
pub const DEFAULT_INTERVAL_MS: u64 = 1000;

/// How long `ping` waits when `--timeout-ms` is not given, matching the IPC client default.
pub const DEFAULT_TIMEOUT_MS: u64 = neb_core::ipc::DEFAULT_TIMEOUT.as_millis() as u64;

/// Everything a command may be given.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Arguments that are not options, in order: the subcommand, then its operands.
    pub positional: Vec<String>,
    /// `--json`: emit one machine-readable document.
    pub json: bool,
    /// `--count N`: how many samples to take.
    pub count: u32,
    /// `--interval-ms N`: pause between samples.
    pub interval_ms: u64,
    /// `--timeout-ms N`: how long `ping` waits for an answer.
    pub timeout_ms: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            positional: Vec::new(),
            json: false,
            count: DEFAULT_COUNT,
            interval_ms: DEFAULT_INTERVAL_MS,
            timeout_ms: DEFAULT_TIMEOUT_MS,
        }
    }
}

impl Options {
    /// Parses the arguments that follow the command name.
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] for an unknown option, a missing value, a value that is not a number, or
    /// `--count 0`.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut options = Self::default();
        let mut rest = args.iter();

        while let Some(argument) = rest.next() {
            match argument.as_str() {
                "--json" => options.json = true,
                "--count" => options.count = number("--count", rest.next())?,
                "--interval-ms" => options.interval_ms = number("--interval-ms", rest.next())?,
                "--timeout-ms" => options.timeout_ms = number("--timeout-ms", rest.next())?,
                other if other.starts_with("--") => {
                    return Err(CliError::Usage(format!("unknown option '{other}'")));
                }
                other => options.positional.push(other.to_owned()),
            }
        }

        if options.count == 0 {
            return Err(CliError::Usage("--count must be at least 1".to_owned()));
        }

        if options.count > MAX_COUNT {
            return Err(CliError::Usage(format!(
                "--count must be at most {MAX_COUNT}"
            )));
        }

        Ok(options)
    }
}

/// Reads the value of an option, which must be present and numeric.
fn number<T>(option: &str, value: Option<&String>) -> Result<T, CliError>
where
    T: std::str::FromStr,
{
    let value = value.ok_or_else(|| CliError::Usage(format!("{option} needs a value")))?;

    value
        .parse()
        .map_err(|_| CliError::Usage(format!("{option} expects a number, got '{value}'")))
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
        assert_eq!(options.count, 2);
        assert_eq!(options.interval_ms, 1000);
        assert_eq!(options.timeout_ms, 10_000);
        assert!(!options.json);
    }

    #[test]
    fn options_and_operands_are_separated() {
        let options = Options::parse(&argv(&[
            "dump",
            "--json",
            "--count",
            "3",
            "--interval-ms",
            "250",
        ]))
        .expect("parse");

        assert_eq!(options.positional, vec!["dump".to_owned()]);
        assert!(options.json);
        assert_eq!(options.count, 3);
        assert_eq!(options.interval_ms, 250);
    }

    #[test]
    fn an_unknown_option_is_refused() {
        match Options::parse(&argv(&["--verbose"])) {
            Err(CliError::Usage(message)) => assert!(message.contains("--verbose")),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_or_bad_value_is_refused() {
        assert!(matches!(
            Options::parse(&argv(&["--count"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            Options::parse(&argv(&["--count", "many"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            Options::parse(&argv(&["--interval-ms", "-5"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn zero_samples_is_a_usage_error() {
        match Options::parse(&argv(&["--count", "0"])) {
            Err(CliError::Usage(message)) => assert!(message.contains("--count")),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn an_absurd_sample_count_is_a_usage_error_rather_than_an_allocation() {
        match Options::parse(&argv(&["--count", "4000000000"])) {
            Err(CliError::Usage(message)) => assert!(message.contains(&MAX_COUNT.to_string())),
            other => panic!("expected a usage error, got {other:?}"),
        }

        assert!(Options::parse(&argv(&["--count", "1000"])).is_ok());
        assert!(Options::parse(&argv(&["--count", "1001"])).is_err());
    }

    #[test]
    fn a_single_dash_operand_is_not_an_option() {
        let options = Options::parse(&argv(&["ping", "-"])).expect("parse");

        assert_eq!(options.positional, vec!["ping".to_owned(), "-".to_owned()]);
    }
}
