//! The client end of a Discord IPC named pipe.
//!
//! Discord listens on the first free name in `\\.\pipe\discord-ipc-0` through
//! `\\.\pipe\discord-ipc-9`; [`connect`] tries them in that order. The handle is opened with
//! read/write access and no sharing restrictions, because Discord hands one endpoint to this process
//! and keeps the other.
//!
//! Reads are polled with `PeekNamedPipe` rather than blocking in `ReadFile`. A blocking read would
//! leave the reader thread stuck after `disconnect`, whereas a poll notices the stop flag within
//! [`READ_POLL`]. Writes take a mutex so two commands cannot interleave their halves of a frame.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_BROKEN_PIPE, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND,
    ERROR_PIPE_NOT_CONNECTED, GENERIC_READ, GENERIC_WRITE, HANDLE,
};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING, ReadFile,
    WriteFile,
};
use windows::Win32::System::Pipes::{PeekNamedPipe, WaitNamedPipeW};
use windows::core::{HRESULT, PCWSTR};

use crate::protocol::{Frame, HEADER_BYTES, MAX_FRAME_BYTES, ProtocolError};

/// Device path prefix Discord uses.
pub const PIPE_PREFIX: &str = r"\\.\pipe\discord-ipc-";

/// How many numbered Discord pipes exist (`discord-ipc-0` ..= `discord-ipc-9`).
pub const PIPE_SLOTS: u32 = 10;

/// How often a blocked reader checks the stop flag and for new bytes.
pub const READ_POLL: Duration = Duration::from_millis(20);

/// How long a write of one frame may be reported as incomplete before it is retried.
const WRITE_RETRY: Duration = Duration::from_millis(5);

/// Why a pipe could not be opened or used.
#[derive(Debug, thiserror::Error)]
pub enum PipeError {
    /// None of the ten Discord pipes answered.
    #[error("discord is not running: no pipe answered among {tried:?}")]
    NotFound {
        /// Every path that was tried, in order.
        tried: Vec<String>,
    },

    /// A pipe exists but could not be opened.
    #[error("cannot open discord pipe '{path}': {source}")]
    Open {
        /// Device path that was tried.
        path: String,
        /// The Win32 error.
        #[source]
        source: windows::core::Error,
    },

    /// The pipe failed while a frame was being read or written.
    #[error("discord pipe '{pipe}' failed: {source}")]
    Win32 {
        /// Logical pipe name.
        pipe: String,
        /// The Win32 error.
        #[source]
        source: windows::core::Error,
    },

    /// The peer closed the pipe.
    #[error("discord closed the pipe")]
    Closed,

    /// A frame was malformed.
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    /// A blocking operation was stopped by its owner.
    #[error("the discord pipe read was stopped")]
    Stopped,
}

/// One owned Win32 pipe handle.
///
/// The handle is a kernel object id, not a pointer into this process, so it is safe to move it to the
/// reader thread and share the `Arc` with the writer.
#[derive(Debug)]
struct PipeHandle {
    handle: HANDLE,
    name: String,
}

// SAFETY: a HANDLE is an opaque kernel object id. Closing it is the only operation `Drop` performs,
// and `Arc<PipeHandle>` keeps that close after the last reader and writer are gone.
unsafe impl Send for PipeHandle {}
unsafe impl Sync for PipeHandle {}

impl Drop for PipeHandle {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: the handle is owned by this value and is closed exactly once, here.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}

/// A connected Discord pipe. Clones share the same handle and the same write lock.
#[derive(Debug, Clone)]
pub struct Connection {
    pipe: Arc<PipeHandle>,
    writes: Arc<Mutex<()>>,
}

impl Connection {
    /// Wraps a handle this value now owns.
    pub(crate) fn from_handle(handle: HANDLE, name: impl Into<String>) -> Self {
        Self {
            pipe: Arc::new(PipeHandle {
                handle,
                name: name.into(),
            }),
            writes: Arc::new(Mutex::new(())),
        }
    }

    /// Logical name, e.g. `discord-ipc-0`.
    pub fn name(&self) -> &str {
        &self.pipe.name
    }

    /// Writes one whole frame, holding the write lock for its duration.
    ///
    /// # Errors
    ///
    /// [`PipeError::Protocol`] when the frame cannot be encoded, [`PipeError::Win32`] when the pipe
    /// refuses the bytes.
    pub fn write_frame(&self, frame: &Frame) -> Result<(), PipeError> {
        let bytes = frame.encode()?;
        let _guard = self
            .writes
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());

        let mut written = 0;
        while written < bytes.len() {
            let mut count = 0_u32;
            // SAFETY: the handle is live for as long as this `Arc` exists, and the slice is a valid
            // readable buffer of the length passed.
            unsafe {
                WriteFile(
                    self.pipe.handle,
                    Some(&bytes[written..]),
                    Some(&mut count),
                    None,
                )
            }
            .map_err(|source| self.win32(source))?;

            if count == 0 {
                thread::sleep(WRITE_RETRY);
                continue;
            }
            written += count as usize;
        }

        Ok(())
    }

    /// Reads one frame, polling for bytes and returning `None` once `stop` is set.
    ///
    /// # Errors
    ///
    /// [`PipeError::Closed`] when the peer hung up, [`PipeError::Protocol`] for a malformed frame,
    /// [`PipeError::Win32`] when the pipe fails.
    pub fn read_frame(&self, stop: &AtomicBool) -> Result<Option<Frame>, PipeError> {
        loop {
            if stop.load(Ordering::SeqCst) {
                return Ok(None);
            }

            let available = self.available()?;
            if available >= HEADER_BYTES as u32 {
                break;
            }

            thread::sleep(READ_POLL);
        }

        let mut header = [0_u8; HEADER_BYTES];
        self.read_exact(&mut header, stop)?;

        let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(PipeError::Protocol(ProtocolError::TooLarge {
                size: length,
                limit: MAX_FRAME_BYTES,
            }));
        }

        let mut body = vec![0_u8; length];
        self.read_exact(&mut body, stop)?;

        let mut bytes = Vec::with_capacity(HEADER_BYTES + length);
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&body);
        Ok(Some(Frame::decode(&bytes)?))
    }

    /// Bytes waiting in the pipe, without consuming them.
    fn available(&self) -> Result<u32, PipeError> {
        let mut available = 0_u32;

        // SAFETY: the handle is live and none of the optional buffers are asked for.
        unsafe { PeekNamedPipe(self.pipe.handle, None, 0, None, Some(&mut available), None) }
            .map_err(|source| self.win32(source))?;

        Ok(available)
    }

    /// Fills `buffer`, returning early when `stop` is set.
    fn read_exact(&self, buffer: &mut [u8], stop: &AtomicBool) -> Result<(), PipeError> {
        let mut filled = 0;

        while filled < buffer.len() {
            if stop.load(Ordering::SeqCst) {
                return Err(PipeError::Stopped);
            }

            let available = self.available()?;
            if available == 0 {
                thread::sleep(READ_POLL);
                continue;
            }

            let take = (available as usize).min(buffer.len() - filled);
            let mut count = 0_u32;
            // SAFETY: the handle is live and the destination slice has at least `take` writable
            // bytes; the read count is written by the call.
            unsafe {
                ReadFile(
                    self.pipe.handle,
                    Some(&mut buffer[filled..filled + take]),
                    Some(&mut count),
                    None,
                )
            }
            .map_err(|source| self.win32(source))?;

            if count == 0 {
                return Err(PipeError::Closed);
            }
            filled += count as usize;
        }

        Ok(())
    }

    fn win32(&self, source: windows::core::Error) -> PipeError {
        let code = source.code();
        if code == HRESULT::from_win32(ERROR_BROKEN_PIPE.0)
            || code == HRESULT::from_win32(ERROR_PIPE_NOT_CONNECTED.0)
        {
            return PipeError::Closed;
        }

        PipeError::Win32 {
            pipe: self.pipe.name.clone(),
            source,
        }
    }
}

/// Tries `\\.\pipe\discord-ipc-0` through `\\.\pipe\discord-ipc-9`.
///
/// # Errors
///
/// [`PipeError::NotFound`] when every slot is empty, [`PipeError::Open`] for the last failure of a
/// pipe that exists but cannot be opened.
pub fn connect() -> Result<Connection, PipeError> {
    let mut tried = Vec::with_capacity(PIPE_SLOTS as usize);
    let mut last_open = None;

    for slot in 0..PIPE_SLOTS {
        let name = format!("discord-ipc-{slot}");
        let path = format!("{PIPE_PREFIX}{slot}");
        tried.push(path);

        match open(&name, Duration::ZERO) {
            Ok(connection) => return Ok(connection),
            Err(error) => {
                if let PipeError::Open { source, .. } = &error {
                    // A missing pipe is the ordinary "this slot is free"; a sharing or access error
                    // is worth remembering if no later slot works.
                    let absent = source.code() == HRESULT::from_win32(ERROR_FILE_NOT_FOUND.0)
                        || source.code() == HRESULT::from_win32(ERROR_PATH_NOT_FOUND.0);
                    if !absent {
                        last_open = Some(error);
                    }
                }
            }
        }
    }

    match last_open {
        Some(error) => Err(error),
        None => Err(PipeError::NotFound { tried }),
    }
}

/// Opens one logical pipe name, e.g. `discord-ipc-test`. Used by tests and diagnostics.
///
/// # Errors
///
/// [`PipeError::Open`] when the pipe cannot be opened in `timeout`.
pub fn connect_named(name: &str, timeout: Duration) -> Result<Connection, PipeError> {
    open(name, timeout)
}

/// Opens `\\.\pipe\<name>`.
fn open(name: &str, timeout: Duration) -> Result<Connection, PipeError> {
    let path = format!(r"\\.\pipe\{name}");
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let millis = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX);

    // Waiting first is what makes a pipe that is momentarily busy (another client connecting) work
    // instead of surfacing ERROR_PIPE_BUSY. A zero timeout is an immediate check.
    // SAFETY: the path is NUL-terminated and outlives the call; the timeout is a plain millisecond
    // count and the return value is only a boolean.
    unsafe {
        let _ = WaitNamedPipeW(PCWSTR(wide.as_ptr()), millis);
    }

    // SAFETY: the path is NUL-terminated and outlives the call; no security attributes are passed;
    // on success the returned handle is owned by this process and moved into `Connection`.
    let handle = unsafe {
        CreateFileW(
            PCWSTR(wide.as_ptr()),
            (GENERIC_READ | GENERIC_WRITE).0,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        )
    }
    .map_err(|source| PipeError::Open {
        path: path.clone(),
        source,
    })?;

    Ok(Connection::from_handle(handle, name))
}
