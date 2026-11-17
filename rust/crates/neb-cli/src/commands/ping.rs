//! `neb-cli ping <pipe>`: one IPC call, answered by whichever process owns that pipe.
//!
//! The transport is the same named-pipe protocol the app and the shell use, so a successful ping
//! proves the pipe, the framing, the JSON envelope and the timeout path all work against the live
//! process, not just against a test double.

use std::time::Duration;

use neb_core::ipc::{DEFAULT_PIPE_NAME, call};
use serde_json::{Value, json};

use super::{CliError, render_json, write_json, write_line};
use crate::args::Options;

/// Runs `neb-cli ping <pipe>`.
pub fn run(args: &[String]) -> Result<(), CliError> {
    let options = Options::parse(args)?;

    let Some(pipe) = options.positional.first() else {
        return Err(CliError::Usage(format!(
            "ping needs a pipe name, for example: neb-cli ping {DEFAULT_PIPE_NAME}"
        )));
    };

    if options.positional.len() > 1 {
        return Err(CliError::Usage(
            "ping takes exactly one pipe name".to_owned(),
        ));
    }

    let result = call(
        pipe,
        "ping",
        None,
        Duration::from_millis(options.timeout_ms),
    )?;

    if options.json {
        return write_json(&json!({
            "pipe": pipe,
            "ok": true,
            "result": result,
        }));
    }

    match &result {
        Value::Null => write_line("pong"),
        Value::String(text) => write_line(text),
        other => write_line(&render_json(other)?),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_pipe_name_is_a_usage_error() {
        match run(&[]) {
            Err(CliError::Usage(message)) => assert!(message.contains(DEFAULT_PIPE_NAME)),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn a_second_operand_is_a_usage_error() {
        assert!(matches!(
            run(&["one".to_owned(), "two".to_owned()]),
            Err(CliError::Usage(_))
        ));
    }
}
