//! A miniature Discord for the test suite.
//!
//! The thread owns one `\\.\pipe\discord-ipc-test` instance, reads the handshake, answers `READY`,
//! and acknowledges `SET_ACTIVITY`. Every frame in both directions is appended to a log, which is what
//! lets a test assert the exact sequence rather than only the eventual state.

use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::PIPE_ACCESS_DUPLEX;
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows::core::{HRESULT, PCWSTR};

use crate::pipe::Connection;
use crate::protocol::{Frame, Opcode};

/// Size of each kernel buffer.
const BUFFER_BYTES: u32 = 64 * 1024;

/// The one fake server name the handshake test uses.
pub const TEST_PIPE: &str = "discord-ipc-test";

/// A fake Discord instance running on its own thread.
#[derive(Debug)]
pub struct FakePipe {
    name: String,
    frames: Arc<Mutex<Vec<String>>>,
    join: Option<JoinHandle<()>>,
}

impl FakePipe {
    /// Starts the server. `ready_delay` is the pause between the handshake and `READY`, so a test can
    /// observe the `connecting` state without a race.
    pub fn start(name: &str, ready_delay: Duration) -> Self {
        let frames = Arc::new(Mutex::new(Vec::new()));
        let thread_frames = Arc::clone(&frames);
        let thread_name = name.to_owned();

        let join = thread::Builder::new()
            .name(format!("fake-discord-{name}"))
            .spawn(move || serve(&thread_name, ready_delay, &thread_frames))
            .expect("spawn the fake discord");

        Self {
            name: name.to_owned(),
            frames,
            join: Some(join),
        }
    }

    /// Every frame seen so far, oldest first.
    pub fn frames(&self) -> Vec<String> {
        self.frames
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }

    /// Waits for the server thread and returns the complete log.
    pub fn finish(mut self) -> Vec<String> {
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
        self.frames()
    }
}

impl Drop for FakePipe {
    fn drop(&mut self) {
        if let Some(join) = self.join.take() {
            // Unblock `ConnectNamedPipe` when the test failed before a client connected.
            let _ = crate::pipe::connect_named(&self.name, Duration::ZERO);
            let _ = join.join();
        }
    }
}

fn serve(name: &str, ready_delay: Duration, frames: &Mutex<Vec<String>>) {
    let path = format!(r"\\.\pipe\{name}");
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();

    // SAFETY: the path is NUL-terminated and outlives the call; the arguments are the documented
    // byte-mode duplex server values and no security descriptor is passed.
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(wide.as_ptr()),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            BUFFER_BYTES,
            BUFFER_BYTES,
            0,
            None,
        )
    };
    if handle.is_invalid() {
        record(
            frames,
            format!(
                "server: cannot create '{path}': {}",
                windows::core::Error::from_thread()
            ),
        );
        return;
    }

    // SAFETY: the handle is a live unconnected server instance.
    match unsafe { ConnectNamedPipe(handle, None) } {
        Ok(()) => {}
        Err(error) if error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => {}
        Err(error) => {
            record(frames, format!("server: connect failed: {error}"));
            close(handle);
            return;
        }
    }

    let connection = Connection::from_handle(handle, name);
    let stop = std::sync::atomic::AtomicBool::new(false);

    let handshake = match connection.read_frame(&stop) {
        Ok(Some(frame)) => {
            record(frames, format!("recv {}", frame.summary()));
            frame
        }
        Ok(None) => return,
        Err(error) => {
            record(frames, format!("recv error: {error}"));
            return;
        }
    };

    if handshake.op != Opcode::Handshake {
        record(frames, "server: first frame was not a handshake".to_owned());
        return;
    }

    thread::sleep(ready_delay);

    let ready = Frame::new(
        Opcode::Frame,
        json!({
            "cmd": "DISPATCH",
            "evt": "READY",
            "data": {
                "v": 1,
                "config": {},
                "user": { "id": "1", "username": "fake-discord" }
            }
        }),
    );
    if send(&connection, frames, &ready).is_err() {
        return;
    }

    loop {
        match connection.read_frame(&stop) {
            Ok(Some(frame)) => {
                record(frames, format!("recv {}", frame.summary()));
                if frame.op == Opcode::Close {
                    return;
                }
                if let Some(ack) = acknowledge(&frame) {
                    if send(&connection, frames, &ack).is_err() {
                        return;
                    }
                }
            }
            Ok(None) | Err(_) => return,
        }
    }
}

/// The answer the real client would send: an activity acknowledgement or a pong.
fn acknowledge(frame: &Frame) -> Option<Frame> {
    match frame.op {
        Opcode::Ping => Some(Frame::pong(frame.payload.clone())),
        Opcode::Frame if frame.command_name() == Some("SET_ACTIVITY") => Some(Frame::new(
            Opcode::Frame,
            json!({
                "cmd": "SET_ACTIVITY",
                "data": Value::Null,
                "evt": Value::Null,
                "nonce": frame.payload.get("nonce").cloned().unwrap_or(Value::Null),
            }),
        )),
        _ => None,
    }
}

fn send(connection: &Connection, frames: &Mutex<Vec<String>>, frame: &Frame) -> Result<(), ()> {
    match connection.write_frame(frame) {
        Ok(()) => {
            record(frames, format!("send {}", frame.summary()));
            Ok(())
        }
        Err(error) => {
            record(frames, format!("send error: {error}"));
            Err(())
        }
    }
}

fn record(frames: &Mutex<Vec<String>>, line: String) {
    frames
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .push(line);
}

/// Closes a handle that was not handed to [`Connection`].
fn close(handle: HANDLE) {
    // SAFETY: the caller passes an owned handle that has not been wrapped yet.
    unsafe {
        let _ = windows::Win32::Foundation::CloseHandle(handle);
    }
}
