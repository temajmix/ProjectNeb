//! The calling side: one connection per call, so concurrent calls cannot interleave.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{
    ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_IO_PENDING, ERROR_NO_DATA, ERROR_PIPE_BUSY,
    GENERIC_READ, GENERIC_WRITE, HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_FLAG_OVERLAPPED, FILE_SHARE_NONE, OPEN_EXISTING, ReadFile, WriteFile,
};
use windows::Win32::System::IO::{CancelIoEx, GetOverlappedResult, OVERLAPPED};
use windows::Win32::System::Pipes::WaitNamedPipeW;
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{HRESULT, PCWSTR};

use super::protocol::{MAX_LINE_BYTES, Request, Response};
use super::{IpcError, OwnedHandle, pipe_path};

/// Size of one read from the pipe.
const READ_CHUNK: usize = 4 * 1024;

/// How long a call keeps retrying while the pipe does not exist yet.
///
/// A server creates its first instance before it reports itself started, but a caller racing that
/// start (or a server between two instances) can see "no such pipe" for a moment. Beyond this grace
/// the answer really is "nobody is listening", and the call fails fast instead of waiting out the
/// whole timeout.
const CONNECT_GRACE: Duration = Duration::from_millis(500);

/// Ids for this process's calls. The C# build numbers its calls the same way, and an id only has to
/// be unique among the requests this client has in flight.
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

/// Performs one request on a fresh connection and returns the `result`.
///
/// A server-side failure is [`IpcError::Server`] carrying the server's own message; a server that
/// does not answer in `timeout` is [`IpcError::Timeout`]; a pipe with no listener is
/// [`IpcError::Unavailable`]. A success without a value yields [`Value::Null`].
///
/// # Errors
///
/// Every variant of [`IpcError`] except the ones about serving.
pub fn call(
    pipe: &str,
    method: &str,
    params: Option<Value>,
    timeout: Duration,
) -> Result<Value, IpcError> {
    let path = pipe_path(pipe)?;
    let call = Call {
        pipe,
        method,
        timeout,
        deadline: Instant::now() + timeout,
    };

    let handle = connect(&call, &path)?;

    let request = Request {
        id: NEXT_ID.fetch_add(1, Ordering::Relaxed).to_string(),
        method: method.to_owned(),
        params,
    };
    let mut line = serde_json::to_vec(&request)?;
    line.push(b'\n');
    write_all(&call, handle.raw(), &mut line)?;

    let mut pending = Vec::with_capacity(READ_CHUNK);
    let answer = read_line(&call, handle.raw(), &mut pending)?;

    decode(&call, &request.id, &answer)
}

/// What one call is and how long it has left.
#[derive(Debug, Clone, Copy)]
struct Call<'a> {
    pipe: &'a str,
    method: &'a str,
    timeout: Duration,
    deadline: Instant,
}

impl Call<'_> {
    /// Milliseconds left, rounded up so a sub-millisecond remainder still gets a wait; `0` means the
    /// deadline has passed.
    fn remaining_millis(&self) -> u32 {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return 0;
        }

        let millis = remaining.as_millis().max(1).min(u128::from(u32::MAX));
        u32::try_from(millis).unwrap_or(u32::MAX)
    }

    fn timeout_error(&self) -> IpcError {
        IpcError::Timeout {
            method: self.method.to_owned(),
            timeout: self.timeout,
        }
    }

    fn win32(&self, action: &'static str, source: windows::core::Error) -> IpcError {
        IpcError::Win32 {
            action,
            pipe: self.pipe.to_owned(),
            source,
        }
    }

    fn closed(&self) -> IpcError {
        IpcError::Closed {
            method: self.method.to_owned(),
        }
    }

    fn protocol(&self, reason: impl Into<String>) -> IpcError {
        IpcError::Protocol {
            method: self.method.to_owned(),
            reason: reason.into(),
        }
    }
}

/// Opens a connection, waiting while every instance of the pipe is busy.
fn connect(call: &Call<'_>, path: &[u16]) -> Result<OwnedHandle, IpcError> {
    let started = Instant::now();

    loop {
        // SAFETY: `path` is a NUL-terminated UTF-16 buffer alive for the call; the handle is owned
        // by the returned `OwnedHandle`, which closes it once.
        let created = unsafe {
            CreateFileW(
                PCWSTR(path.as_ptr()),
                (GENERIC_READ | GENERIC_WRITE).0,
                FILE_SHARE_NONE,
                None,
                OPEN_EXISTING,
                FILE_FLAG_OVERLAPPED,
                None,
            )
        };

        match created {
            Ok(handle) => return Ok(OwnedHandle::new(handle)),
            Err(error) => {
                let code = error.code();
                if code == HRESULT::from_win32(ERROR_PIPE_BUSY.0) {
                    // Every instance is serving someone else: wait for one to free up. Windows has
                    // no "wait for a pipe to appear", so `FILE_NOT_FOUND` below really is no server.
                    if call.remaining_millis() == 0 {
                        return Err(call.timeout_error());
                    }

                    // SAFETY: `path` is the same live NUL-terminated buffer; a failure only means
                    // the wait expired, and the loop then re-checks the deadline.
                    unsafe {
                        let _ = WaitNamedPipeW(PCWSTR(path.as_ptr()), call.remaining_millis());
                    }
                    continue;
                }

                if code == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0) {
                    if started.elapsed() < CONNECT_GRACE && call.remaining_millis() > 0 {
                        // The server may be between instances, or still starting.
                        std::thread::sleep(Duration::from_millis(5));
                        continue;
                    }

                    return Err(IpcError::Unavailable {
                        pipe: call.pipe.to_owned(),
                    });
                }

                return Err(call.win32("connect to", error));
            }
        }
    }
}

/// Writes every byte of `bytes`, or explains why not.
fn write_all(call: &Call<'_>, handle: HANDLE, bytes: &mut [u8]) -> Result<(), IpcError> {
    let mut written = 0;

    while written < bytes.len() {
        let count = transfer(call, handle, &mut bytes[written..], Direction::Write)?;
        if count == 0 {
            return Err(call.closed());
        }
        written += count;
    }

    Ok(())
}

/// Reads one newline-terminated line, enforcing [`MAX_LINE_BYTES`] and the deadline.
fn read_line(call: &Call<'_>, handle: HANDLE, pending: &mut Vec<u8>) -> Result<Vec<u8>, IpcError> {
    let mut chunk = [0u8; READ_CHUNK];

    loop {
        if let Some(position) = pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = pending.drain(..=position).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            return Ok(line);
        }

        if pending.len() > MAX_LINE_BYTES {
            return Err(call.protocol(format!("the response line exceeds {MAX_LINE_BYTES} bytes")));
        }

        let count = transfer(call, handle, &mut chunk, Direction::Read)?;
        if count == 0 {
            return Err(call.closed());
        }

        pending.extend_from_slice(&chunk[..count]);
    }
}

/// Which way bytes are moving.
#[derive(Debug, Clone, Copy)]
enum Direction {
    Read,
    Write,
}

/// One overlapped read or write, bounded by the call's deadline.
fn transfer(
    call: &Call<'_>,
    handle: HANDLE,
    buffer: &mut [u8],
    direction: Direction,
) -> Result<usize, IpcError> {
    // SAFETY: a manual-reset event with no name; the returned handle is owned by `event` below.
    let event = unsafe { CreateEventW(None, true, false, None) }
        .map_err(|error| call.win32("create an event for", error))?;
    let event = OwnedHandle::new(event);

    let mut overlapped = OVERLAPPED {
        hEvent: event.raw(),
        ..OVERLAPPED::default()
    };
    let mut transferred = 0u32;

    // SAFETY: the pipe handle is live and owned by the caller, `buffer` is a live slice that
    // outlives the operation (a timeout waits for the cancellation to complete before returning),
    // and `overlapped` is a live structure tied to `event`.
    let started = unsafe {
        match direction {
            Direction::Read => ReadFile(
                handle,
                Some(&mut buffer[..]),
                Some(&mut transferred),
                Some(&mut overlapped),
            ),
            Direction::Write => WriteFile(
                handle,
                Some(&buffer[..]),
                Some(&mut transferred),
                Some(&mut overlapped),
            ),
        }
    };

    match started {
        Ok(()) => Ok(transferred as usize),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_IO_PENDING.0) => {
            // SAFETY: `event` owns a live, unnamed manual-reset event created above; waiting on it
            // neither moves nor frees the handle, which stays owned until this function returns.
            let wait = unsafe { WaitForSingleObject(event.raw(), call.remaining_millis()) };

            if wait == WAIT_OBJECT_0 {
                let mut completed = 0u32;
                // SAFETY: the overlapped operation belongs to `handle`, its event is signalled, and
                // `completed` is a live out-parameter.
                unsafe { GetOverlappedResult(handle, &overlapped, &mut completed, false) }
                    .map_err(|error| ended_because(call, error))?;
                return Ok(completed as usize);
            }

            if wait == WAIT_TIMEOUT {
                let mut cancelled = 0u32;
                // SAFETY: both calls target this handle's own pending operation. `CancelIoEx` asks
                // the kernel to stop it, and `GetOverlappedResult` with `bwait = true` blocks until
                // it has actually stopped. That barrier is what lets `buffer` — borrowed by the
                // caller and released when this function returns — go out of scope safely.
                unsafe {
                    let _ = CancelIoEx(handle, Some(&overlapped));
                    let _ = GetOverlappedResult(handle, &overlapped, &mut cancelled, true);
                }
                return Err(call.timeout_error());
            }

            // SAFETY: reads the calling thread's last error, which `WaitForSingleObject` set.
            Err(call.win32("wait for bytes on", windows::core::Error::from_thread()))
        }
        Err(error) => Err(ended_because(call, error)),
    }
}

/// Turns "the other end went away" into [`IpcError::Closed`] and everything else into a Win32 error.
fn ended_because(call: &Call<'_>, error: windows::core::Error) -> IpcError {
    let code = error.code();
    if code == HRESULT::from_win32(ERROR_BROKEN_PIPE.0)
        || code == HRESULT::from_win32(ERROR_NO_DATA.0)
    {
        return call.closed();
    }

    call.win32("transfer bytes on", error)
}

/// Reads the response envelope, surfacing the server's error message verbatim.
///
/// The response must be addressed to this request — a client that accepted another caller's answer
/// would return the wrong result and be off by one for every later answer. The one exception is the
/// C# build's own shape for an error it could not correlate (a request it never parsed, or a refusal
/// sent before the request arrived): that envelope carries `"id": ""` and is accepted as an error,
/// never as a result.
fn decode(call: &Call<'_>, expected_id: &str, line: &[u8]) -> Result<Value, IpcError> {
    let response: Response = serde_json::from_slice(line)
        .map_err(|error| call.protocol(format!("not a response envelope: {error}")))?;

    let uncorrelated_error = response.id.is_empty() && !response.ok;
    if response.id != expected_id && !uncorrelated_error {
        return Err(call.protocol(format!(
            "the response id '{}' does not match the request id '{expected_id}'",
            response.id
        )));
    }

    if response.ok {
        return Ok(response.result.unwrap_or(Value::Null));
    }

    Err(IpcError::Server {
        method: call.method.to_owned(),
        message: response.error.unwrap_or_else(|| {
            format!("ipc call '{}' failed without an error message", call.method)
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A call context for `decode`, which only needs the method for its messages.
    fn call(method: &str) -> Call<'_> {
        Call {
            pipe: "p",
            method,
            timeout: Duration::from_secs(1),
            deadline: Instant::now(),
        }
    }

    #[test]
    fn call_ids_are_unique_and_start_at_one() {
        let first: u64 = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let second: u64 = NEXT_ID.fetch_add(1, Ordering::Relaxed);

        assert_eq!(second, first + 1);
        assert!(first >= 1);
    }

    #[test]
    fn a_success_envelope_yields_its_result() {
        let call = call("m");

        assert_eq!(
            decode(&call, "1", br#"{"id":"1","ok":true,"result":{"a":1}}"#).expect("decode"),
            serde_json::json!({ "a": 1 })
        );
        assert_eq!(
            decode(&call, "1", br#"{"id":"1","ok":true,"result":null}"#).expect("decode"),
            Value::Null
        );
    }

    #[test]
    fn a_failure_envelope_keeps_the_servers_words() {
        let call = call("m");

        match decode(&call, "1", br#"{"id":"1","ok":false,"error":"nope"}"#) {
            Err(IpcError::Server { message, .. }) => assert_eq!(message, "nope"),
            other => panic!("expected the server's message, got {other:?}"),
        }
    }

    #[test]
    fn a_failure_without_a_message_still_explains_itself() {
        let call = call("probe");

        match decode(&call, "1", br#"{"id":"1","ok":false}"#) {
            Err(IpcError::Server { message, .. }) => {
                assert!(message.contains("without an error message"), "{message}");
            }
            other => panic!("expected the server's message, got {other:?}"),
        }
    }

    #[test]
    fn a_non_envelope_is_a_protocol_error() {
        let call = call("m");

        assert!(matches!(
            decode(&call, "1", b"not json"),
            Err(IpcError::Protocol { .. })
        ));
    }

    #[test]
    fn an_answer_addressed_to_another_request_is_a_protocol_error() {
        let call = call("m");

        match decode(
            &call,
            "7",
            br#"{"id":"8","ok":true,"result":"someone else's"}"#,
        ) {
            Err(IpcError::Protocol { reason, .. }) => {
                assert!(reason.contains('8') && reason.contains('7'), "{reason}");
            }
            other => panic!("expected a protocol error, got {other:?}"),
        }
    }

    #[test]
    fn an_uncorrelated_error_is_accepted() {
        let call = call("m");

        // The server cannot echo an id it never received; the C# build answers with "" as well.
        match decode(
            &call,
            "7",
            br#"{"id":"","ok":false,"error":"ipc server is busy"}"#,
        ) {
            Err(IpcError::Server { message, .. }) => assert!(message.contains("busy")),
            other => panic!("expected the server's message, got {other:?}"),
        }
    }

    #[test]
    fn a_success_without_our_id_is_a_protocol_error() {
        let call = call("m");

        assert!(matches!(
            decode(&call, "7", br#"{"id":"","ok":true,"result":1}"#),
            Err(IpcError::Protocol { .. })
        ));
        assert!(matches!(
            decode(&call, "7", br#"{"ok":true,"result":1}"#),
            Err(IpcError::Protocol { .. })
        ));
    }
}
