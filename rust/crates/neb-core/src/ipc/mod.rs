//! Named-pipe IPC, compatible on the wire with the C# build's `Neb.Core.Ipc`.
//!
//! The protocol is newline-delimited UTF-8 JSON over a byte-mode named pipe: one request line in,
//! one response line out, several clients at once, each served on its own thread. `IpcServer.cs`
//! and `IpcClient.cs` are the reference implementation; [`protocol`] documents the envelope.
//!
//! * [`serve`] listens on a pipe and answers with [`Response`]s produced by the caller's handler.
//! * [`call`] performs one request on a fresh connection and returns the `result` value.
//!
//! No async runtime is involved: the accept loop and every client are plain threads, which is what
//! the resident processes need and what keeps the binary small.

mod client;
mod protocol;
mod server;

use std::time::Duration;

use windows::Win32::Foundation::{CloseHandle, HANDLE};

pub use client::call;
pub use protocol::{
    DEFAULT_PIPE_NAME, DEFAULT_TIMEOUT, MAX_LINE_BYTES, Request, RequestError, Response,
};
pub use server::{MAX_CLIENTS, ServerHandle, serve};

/// Everything that can go wrong on the way to a response.
///
/// [`IpcError::Server`] is the important one for callers: it carries the message the server sent,
/// so a failed operation is reported to the user in the server's words rather than a generic
/// transport error.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The name is empty or looks like a path; a pipe name is a single path component.
    #[error("'{0}' is not a pipe name: pass a name such as ProjectNeb.ipc, not a path")]
    InvalidPipeName(String),

    /// A Win32 call failed.
    #[error("cannot {action} pipe '{pipe}': {source}")]
    Win32 {
        /// Verbed operation, e.g. `create an instance of`.
        action: &'static str,
        /// Pipe the operation targeted.
        pipe: String,
        /// The underlying error.
        #[source]
        source: windows::core::Error,
    },

    /// A thread could not be started.
    #[error("cannot {action}: {source}")]
    Io {
        /// Verbed operation, e.g. `spawn the accept thread`.
        action: &'static str,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// Nobody is listening on that pipe.
    #[error("no ipc server is listening on pipe '{pipe}'")]
    Unavailable {
        /// Pipe the client tried to reach.
        pipe: String,
    },

    /// The server did not answer in time.
    #[error("ipc call '{method}' timed out after {timeout:?}")]
    Timeout {
        /// Method that was called.
        method: String,
        /// How long the call waited.
        timeout: Duration,
    },

    /// The server closed the connection before answering.
    #[error("ipc call '{method}' closed before a response arrived")]
    Closed {
        /// Method that was called.
        method: String,
    },

    /// The answer was not the envelope the protocol promises.
    #[error("ipc call '{method}' got a malformed response: {reason}")]
    Protocol {
        /// Method that was called.
        method: String,
        /// What was wrong with the bytes.
        reason: String,
    },

    /// The server answered `ok: false`; `message` is its own explanation.
    #[error("ipc call '{method}' failed: {message}")]
    Server {
        /// Method that was called.
        method: String,
        /// The `error` string of the response.
        message: String,
    },

    /// A message could not be rendered as JSON.
    #[error("ipc message is not valid json: {0}")]
    Json(#[from] serde_json::Error),
}

/// A Win32 handle this module owns and closes when dropped. Handles a pipe instance, a client
/// connection or a wait event, all of which are closed the same way.
#[derive(Debug)]
pub(crate) struct OwnedHandle(HANDLE);

// SAFETY: a handle is an opaque kernel object id, not a pointer into this process. Failing to move
// it to the thread that serves that client is the only thing that would be unsafe, and Win32's pipe
// API is built around exactly that move. Nothing touches the handle after `Drop` closes it.
unsafe impl Send for OwnedHandle {}

impl OwnedHandle {
    /// Takes ownership of `handle`.
    pub(crate) fn new(handle: HANDLE) -> Self {
        Self(handle)
    }

    /// The raw handle behind this owner.
    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Renders `\\.\pipe\<name>` as a NUL-terminated UTF-16 buffer.
pub(crate) fn pipe_path(pipe: &str) -> Result<Vec<u16>, IpcError> {
    if pipe.trim().is_empty() || pipe.contains(['\\', '/']) {
        return Err(IpcError::InvalidPipeName(pipe.to_owned()));
    }

    Ok(format!(r"\\.\pipe\{pipe}")
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pipe_name_becomes_a_device_path() {
        let wide = pipe_path("ProjectNeb.ipc").expect("valid name");
        let text = String::from_utf16(&wide[..wide.len() - 1]).expect("utf-16");

        assert_eq!(text, r"\\.\pipe\ProjectNeb.ipc");
        assert_eq!(wide.last(), Some(&0));
    }

    #[test]
    fn paths_and_blank_names_are_refused() {
        for name in ["", "   ", r"foo\bar", "foo/bar"] {
            match pipe_path(name) {
                Err(IpcError::InvalidPipeName(rejected)) => assert_eq!(rejected, name),
                other => panic!("'{name}' must be rejected, got {other:?}"),
            }
        }
    }
}
