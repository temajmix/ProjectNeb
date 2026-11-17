//! The commands, one module each.

pub mod config;
pub mod metrics;
pub mod ping;
pub mod windows;

use std::fmt;
use std::io::{self, Write};

/// Why a command failed. `main` prints it and exits with code 1.
#[derive(Debug)]
pub enum CliError {
    /// The invocation itself is wrong; the usage text is printed along with it.
    Usage(String),
    /// The config store refused to read or write.
    Config(neb_core::config::ConfigError),
    /// An IPC call failed.
    Ipc(neb_core::ipc::IpcError),
    /// Anything else, already phrased for the user.
    Failed(String),
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(message) => write!(formatter, "{message}"),
            Self::Config(error) => write!(formatter, "{error}"),
            Self::Ipc(error) => write!(formatter, "{error}"),
            Self::Failed(message) => write!(formatter, "{message}"),
        }
    }
}

impl std::error::Error for CliError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Config(error) => Some(error),
            Self::Ipc(error) => Some(error),
            Self::Usage(_) | Self::Failed(_) => None,
        }
    }
}

impl From<neb_core::config::ConfigError> for CliError {
    fn from(error: neb_core::config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<neb_core::ipc::IpcError> for CliError {
    fn from(error: neb_core::ipc::IpcError) -> Self {
        Self::Ipc(error)
    }
}

/// Writes one line to stdout.
pub(crate) fn write_line(text: &str) -> Result<(), CliError> {
    let mut out = io::stdout().lock();

    writeln!(out, "{text}")
        .map_err(|error| CliError::Failed(format!("cannot write to stdout: {error}")))
}

/// Writes `value` as one compact JSON document and a newline.
pub(crate) fn write_json(value: &serde_json::Value) -> Result<(), CliError> {
    write_line(&render_json(value)?)
}

/// Writes `value` as indented JSON.
pub(crate) fn write_json_pretty(value: &serde_json::Value) -> Result<(), CliError> {
    let text = serde_json::to_string_pretty(value)
        .map_err(|error| CliError::Failed(format!("cannot render json: {error}")))?;

    write_line(&text)
}

/// Renders `value` compactly, for embedding in a line of output.
pub(crate) fn render_json(value: &serde_json::Value) -> Result<String, CliError> {
    serde_json::to_string(value)
        .map_err(|error| CliError::Failed(format!("cannot render json: {error}")))
}

/// Converts a serialisable value into a JSON value.
pub(crate) fn to_json<T>(value: &T) -> Result<serde_json::Value, CliError>
where
    T: serde::Serialize,
{
    serde_json::to_value(value)
        .map_err(|error| CliError::Failed(format!("cannot render json: {error}")))
}
