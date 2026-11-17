//! The listening side: one accept loop, one thread per client.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, HANDLE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_NONE, OPEN_EXISTING, PIPE_ACCESS_DUPLEX,
    ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS,
    PIPE_TYPE_BYTE, PIPE_UNLIMITED_INSTANCES, PIPE_WAIT, PeekNamedPipe,
};
use windows::core::{HRESULT, PCWSTR};

use super::{IpcError, MAX_LINE_BYTES, OwnedHandle, Request, Response, pipe_path};

/// Size of the kernel buffer on each direction of a pipe instance.
const BUFFER_BYTES: u32 = 64 * 1024;

/// Pause before creating a replacement instance after a failure, so a broken name cannot spin hot.
const RETRY_DELAY: Duration = Duration::from_millis(20);

/// Read size of the per-client loop.
const READ_CHUNK: usize = 8 * 1024;

/// Most clients served at once.
///
/// Every client costs a thread and a read buffer, so this ceiling is what keeps a flood of local
/// connections from growing the resident process past its memory budget: beyond it the accept loop
/// answers with a "busy" error and hangs up instead of spawning another thread.
pub const MAX_CLIENTS: usize = 32;

/// How long a refusal waits for the refused client's request before closing anyway.
const REFUSAL_WAIT: Duration = Duration::from_millis(100);

/// Most of a refused request that is read, so answering "busy" cannot itself retain a big message.
const REFUSAL_READ_BYTES: usize = 8 * 1024;

/// A running listener. Dropping it stops the accept loop.
#[derive(Debug)]
pub struct ServerHandle {
    pipe: String,
    stop: Arc<AtomicBool>,
    clients: Arc<AtomicUsize>,
    accept: Option<JoinHandle<()>>,
}

impl ServerHandle {
    /// Name this server listens on.
    pub fn pipe_name(&self) -> &str {
        &self.pipe
    }

    /// How many clients are being served right now.
    ///
    /// Never more than [`MAX_CLIENTS`]; useful to a caller that wants to back off before the server
    /// starts refusing connections.
    pub fn client_count(&self) -> usize {
        self.clients.load(Ordering::SeqCst)
    }

    /// Stops the accept loop and waits for it. Client threads finish with their connections.
    pub fn shutdown(mut self) {
        self.stop_listening();
    }

    /// Signals the accept loop and wakes it out of `ConnectNamedPipe`.
    fn stop_listening(&mut self) {
        let Some(accept) = self.accept.take() else {
            return;
        };

        self.stop.store(true, Ordering::SeqCst);

        // The accept loop is blocked inside `ConnectNamedPipe`. A throwaway client connection is the
        // portable way to unblock it; the loop then sees the flag and exits.
        if let Ok(path) = pipe_path(&self.pipe) {
            // SAFETY: `path` is NUL-terminated and outlives the call; the returned handle is closed
            // immediately, and a failure is expected when the loop already exited.
            unsafe {
                if let Ok(handle) = CreateFileW(
                    PCWSTR(path.as_ptr()),
                    (windows::Win32::Foundation::GENERIC_READ
                        | windows::Win32::Foundation::GENERIC_WRITE)
                        .0,
                    FILE_SHARE_NONE,
                    None,
                    OPEN_EXISTING,
                    FILE_ATTRIBUTE_NORMAL,
                    None,
                ) {
                    let _ = windows::Win32::Foundation::CloseHandle(handle);
                }
            }
        }

        let _ = accept.join();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.stop_listening();
    }
}

/// Starts listening on `pipe` and answers every request with `handler`.
///
/// The first pipe instance exists before this function returns, so a client that connects right
/// after starting is served; further instances are created as clients arrive
/// ([`PIPE_UNLIMITED_INSTANCES`]) and remote clients are refused
/// ([`PIPE_REJECT_REMOTE_CLIENTS`]).
///
/// At most [`MAX_CLIENTS`] clients are served at once; beyond that the server answers
/// `{"ok":false,"error":"ipc server is busy …"}` and closes the connection without starting a thread.
///
/// # The handler contract
///
/// `handler` runs on a client thread. It must not panic: the shipped profile builds with
/// `panic = "abort"`, where a panic in a handler ends the whole process. A build that unwinds catches
/// the panic instead, answers `{"ok":false,"error":"the ipc handler panicked"}`, and keeps serving —
/// which is what the development and test profiles do. Handle every error inside the handler; treat a
/// panic as a bug, not as error handling.
///
/// # Errors
///
/// [`IpcError::InvalidPipeName`] for a blank or path-like name, [`IpcError::Win32`] when the first
/// instance cannot be created, [`IpcError::Io`] when the accept thread cannot be started.
pub fn serve<H>(pipe: &str, handler: H) -> Result<ServerHandle, IpcError>
where
    H: Fn(Request) -> Response + Send + Sync + 'static,
{
    let path = pipe_path(pipe)?;
    let first = create_instance(&path, pipe)?;
    let stop = Arc::new(AtomicBool::new(false));
    let clients = Arc::new(AtomicUsize::new(0));
    let handler = Arc::new(handler);
    let name = pipe.to_owned();

    let accept = thread::Builder::new()
        .name(format!("neb-ipc-{pipe}"))
        .spawn({
            let stop = Arc::clone(&stop);
            let clients = Arc::clone(&clients);
            let loop_name = name.clone();
            move || accept_loop(path, loop_name, first, handler, stop, clients)
        })
        .map_err(|source| IpcError::Io {
            action: "spawn the ipc accept thread",
            source,
        })?;

    Ok(ServerHandle {
        pipe: name,
        stop,
        clients,
        accept: Some(accept),
    })
}

/// Creates one unconnected instance of `path`.
fn create_instance(path: &[u16], pipe: &str) -> Result<OwnedHandle, IpcError> {
    // SAFETY: `path` is a NUL-terminated UTF-16 buffer that outlives the call; the arguments are
    // the documented values for a byte-mode duplex server and no security descriptor is passed, so
    // the instance gets the default DACL of the process (current user).
    let handle = unsafe {
        CreateNamedPipeW(
            PCWSTR(path.as_ptr()),
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
        return Err(IpcError::Win32 {
            action: "create an instance of",
            pipe: pipe.to_owned(),
            source: windows::core::Error::from_thread(),
        });
    }

    Ok(OwnedHandle::new(handle))
}

/// Waits for a client on `instance`; `None` when the instance cannot be used and must be replaced.
fn wait_for_client(instance: OwnedHandle) -> Option<OwnedHandle> {
    // SAFETY: `instance` owns a live server handle and no overlapped structure is used, so this
    // blocks until a client connects or the instance fails.
    match unsafe { ConnectNamedPipe(instance.raw(), None) } {
        Ok(()) => Some(instance),
        // A client that connected between create and this call is success, not failure.
        Err(error) if error.code() == HRESULT::from_win32(ERROR_PIPE_CONNECTED.0) => Some(instance),
        Err(_) => None,
    }
}

/// Accepts clients until `stop` is set, refusing connections beyond [`MAX_CLIENTS`].
fn accept_loop<H>(
    path: Vec<u16>,
    pipe: String,
    first: OwnedHandle,
    handler: Arc<H>,
    stop: Arc<AtomicBool>,
    clients: Arc<AtomicUsize>,
) where
    H: Fn(Request) -> Response + Send + Sync + 'static,
{
    let mut pending = Some(first);

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }

        let instance = match pending.take() {
            Some(instance) => instance,
            None => match create_instance(&path, &pipe) {
                Ok(instance) => instance,
                Err(_) => {
                    thread::sleep(RETRY_DELAY);
                    continue;
                }
            },
        };

        let Some(instance) = wait_for_client(instance) else {
            thread::sleep(RETRY_DELAY);
            continue;
        };

        if clients.load(Ordering::SeqCst) >= MAX_CLIENTS {
            // Prepare the next instance first, so a refusal does not leave a gap in the pipe name.
            pending = create_instance(&path, &pipe).ok();

            // Let the client finish writing its request, then answer and hang up. Without this the
            // client's write can race the close and surface as a broken pipe instead of "busy".
            let id = absorb_request(instance.raw());
            let _ = write_response(instance.raw(), &busy_response(id));
            continue;
        }

        // Prepare the next instance before handing this one to a client thread, so a caller that
        // connects right after this one is accepted never sees "no such pipe".
        pending = create_instance(&path, &pipe).ok();

        clients.fetch_add(1, Ordering::SeqCst);
        let handler = Arc::clone(&handler);
        let stop = Arc::clone(&stop);
        let clients_for_thread = Arc::clone(&clients);
        let spawned = thread::Builder::new()
            .name("neb-ipc-client".to_owned())
            .spawn(move || {
                let _slot = ClientSlot(clients_for_thread);
                serve_client(instance, handler, stop);
            });

        if spawned.is_err() {
            // The closure was dropped before it could take its slot back.
            clients.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Holds one of the [`MAX_CLIENTS`] slots for as long as a client is being served.
#[derive(Debug)]
struct ClientSlot(Arc<AtomicUsize>);

impl Drop for ClientSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Answers one connected client until it disconnects or the server stops.
///
/// The instance is closed when this returns; no `DisconnectNamedPipe` is issued, so bytes already
/// written (a rejection message in particular) stay readable to the client instead of being dropped.
fn serve_client<H>(instance: OwnedHandle, handler: Arc<H>, stop: Arc<AtomicBool>)
where
    H: Fn(Request) -> Response,
{
    let mut pending: Vec<u8> = Vec::with_capacity(READ_CHUNK);
    let mut chunk = [0u8; READ_CHUNK];

    loop {
        if stop.load(Ordering::SeqCst) {
            break;
        }

        let mut read = 0u32;
        // SAFETY: `instance` owns a connected duplex handle for the whole loop, and `chunk` is a
        // live buffer whose length is passed as the byte count.
        let result = unsafe { ReadFile(instance.raw(), Some(&mut chunk), Some(&mut read), None) };

        // A client that dies mid-line shows up as a failed read or a zero-byte one. Neither is a
        // panic: the thread ends and the instance is closed by `Drop`.
        if result.is_err() || read == 0 {
            break;
        }

        pending.extend_from_slice(&chunk[..read as usize]);

        while let Some(position) = pending.iter().position(|byte| *byte == b'\n') {
            let mut line: Vec<u8> = pending.drain(..=position).collect();
            line.pop();

            if line.last() == Some(&b'\r') {
                line.pop();
            }

            if line.is_empty() {
                continue;
            }

            if line.len() > MAX_LINE_BYTES {
                reject_oversized(instance.raw());
                return;
            }

            if !answer(instance.raw(), &line, handler.as_ref()) {
                return;
            }
        }

        if pending.len() > MAX_LINE_BYTES {
            reject_oversized(instance.raw());
            return;
        }
    }
}

/// Rejects an oversized line and gives up on the connection.
///
/// The buffer is dropped with this frame and no further bytes are read: a peer that cannot frame a
/// request must not cost a thread and a megabyte for as long as it feels like holding the pipe open.
/// The instance closes when the caller returns, which still delivers the error line it just wrote.
fn reject_oversized(handle: HANDLE) {
    let _ = write_response(handle, &oversized_response());
}

/// The answer to a line that is too long to accept.
fn oversized_response() -> Response {
    Response::failure(
        "",
        format!("ipc request line exceeds {MAX_LINE_BYTES} bytes and was rejected"),
    )
}

/// The answer to a connection that arrived while every slot was taken.
fn busy_response(id: String) -> Response {
    Response::failure(
        id,
        format!("ipc server is busy: {MAX_CLIENTS} clients are already being served"),
    )
}

/// Waits, briefly, for a refused client's request and returns its id.
///
/// Consuming the request is what lets the client's `WriteFile` complete before the server closes the
/// pipe, so the refusal is what the client sees instead of a broken pipe. Only the id is kept — the
/// rest is discarded as it arrives, and at most [`REFUSAL_READ_BYTES`] of the request is ever held —
/// and the wait is bounded by [`REFUSAL_WAIT`], so a client that connects and says nothing cannot
/// stall the accept loop for longer than that.
fn absorb_request(handle: HANDLE) -> String {
    let deadline = Instant::now() + REFUSAL_WAIT;
    let mut line: Vec<u8> = Vec::with_capacity(256);
    let mut chunk = [0u8; 1024];

    while Instant::now() < deadline && line.len() < REFUSAL_READ_BYTES {
        let mut available = 0u32;
        // SAFETY: `handle` is a live connected pipe; `available` is a live out-parameter and no
        // data is copied into a caller buffer.
        let peeked = unsafe { PeekNamedPipe(handle, None, 0, None, Some(&mut available), None) };

        if peeked.is_err() {
            break;
        }

        if available == 0 {
            thread::sleep(Duration::from_millis(1));
            continue;
        }

        let mut read = 0u32;
        // SAFETY: a single read into a live buffer, with the byte count as a live out-parameter.
        let result = unsafe { ReadFile(handle, Some(&mut chunk), Some(&mut read), None) };
        if result.is_err() || read == 0 {
            break;
        }

        line.extend_from_slice(&chunk[..read as usize]);
        if line.contains(&b'\n') {
            break;
        }
    }

    request_id(&String::from_utf8_lossy(&line))
}

/// The `id` of a request line, or `""` when it has none — the same tolerance the C# server has.
fn request_id(line: &str) -> String {
    match serde_json::from_str::<serde_json::Value>(line.trim_end_matches(['\r', '\n'])) {
        Ok(serde_json::Value::Object(object)) => object
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        _ => String::new(),
    }
}

/// Parses and answers one line; `false` when the connection should stop.
fn answer<H>(handle: HANDLE, line: &[u8], handler: &H) -> bool
where
    H: Fn(Request) -> Response,
{
    let Ok(text) = std::str::from_utf8(line) else {
        let response = Response::failure("", "ipc request is not valid utf-8");
        return write_response(handle, &response).is_ok();
    };

    let response = match Request::parse(text) {
        Ok(request) => {
            let mut response = invoke(handler, request.clone());
            // The server owns correlation: a handler cannot answer under a different id.
            response.id = request.id;
            response
        }
        Err(error) => Response::failure("", error.to_string()),
    };

    write_response(handle, &response).is_ok()
}

/// Runs the handler, catching a panic in a build that can unwind.
///
/// The shipped profile is `panic = "abort"`, where a panicking handler necessarily ends the process;
/// [`serve`] documents that contract. Development and test builds unwind, and there a panic in one
/// handler must not be able to take the resident process down for every other client, so it becomes
/// an ordinary error response. See the `#[cfg]` split: one definition per profile, no runtime branch.
#[cfg(panic = "unwind")]
fn invoke<H>(handler: &H, request: Request) -> Response
where
    H: Fn(Request) -> Response,
{
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| handler(request))) {
        Ok(response) => response,
        Err(_) => Response::failure("", PANICKED_MESSAGE),
    }
}

/// Runs the handler in a build that aborts on panic; there is nothing to catch.
#[cfg(not(panic = "unwind"))]
fn invoke<H>(handler: &H, request: Request) -> Response
where
    H: Fn(Request) -> Response,
{
    handler(request)
}

/// The answer to a request whose handler panicked; only reachable where panics unwind.
#[cfg(panic = "unwind")]
const PANICKED_MESSAGE: &str = "the ipc handler panicked";

/// Writes one response line, handling partial writes.
fn write_response(handle: HANDLE, response: &Response) -> Result<(), IpcError> {
    let bytes = response.to_line()?;
    let mut written_total = 0usize;

    while written_total < bytes.len() {
        let mut written = 0u32;
        // SAFETY: `handle` is a live connected pipe handle and the slice borrows `bytes`, which is
        // alive for the whole call.
        let result = unsafe {
            WriteFile(
                handle,
                Some(&bytes[written_total..]),
                Some(&mut written),
                None,
            )
        };

        if result.is_err() || written == 0 {
            return Err(IpcError::Protocol {
                method: String::new(),
                reason: "the client closed while a response was being written".to_owned(),
            });
        }

        written_total += written as usize;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::{DEFAULT_TIMEOUT, call};
    use serde_json::{Value, json};
    use std::io::{BufRead, BufReader, Write};
    use std::sync::atomic::AtomicUsize;
    use std::time::Instant;

    /// A pipe name no other test, process or run can collide with.
    fn unique_pipe(tag: &str) -> String {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let run = NEXT.fetch_add(1, Ordering::SeqCst);

        format!(
            "ProjectNeb.test.{tag}.{}.{run}.{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|elapsed| elapsed.subsec_nanos())
                .unwrap_or_default()
        )
    }

    /// Starts a server whose handler echoes `params` back and rejects every other method.
    fn echo_server() -> (ServerHandle, String) {
        let pipe = unique_pipe("echo");
        let server = serve(&pipe, |request: Request| match request.method.as_str() {
            "echo" => Response::success(request.id, request.params.unwrap_or(Value::Null)),
            other => Response::failure(request.id, format!("unknown method: {other}")),
        })
        .expect("start the server");

        (server, pipe)
    }

    fn raw_client(pipe: &str) -> std::fs::File {
        let path = format!(r"\\.\pipe\{pipe}");
        let deadline = Instant::now() + Duration::from_secs(5);

        loop {
            match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
            {
                Ok(client) => return client,
                // ERROR_FILE_NOT_FOUND: the server is between instances.
                // ERROR_PIPE_BUSY: every instance is serving someone else.
                Err(error)
                    if matches!(error.raw_os_error(), Some(2 | 231))
                        && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("connect a raw client to {path}: {error}"),
            }
        }
    }

    /// Echoes `value`, retrying only while the server answers that it is busy.
    ///
    /// The server refuses beyond [`MAX_CLIENTS`], so a test that wants 500 calls in flight must wait
    /// for a slot like a real client would; any other error is a failure.
    fn echo_when_not_busy(pipe: &str, value: i32) -> Value {
        for _ in 0..2_000 {
            match call(pipe, "echo", Some(json!(value)), DEFAULT_TIMEOUT) {
                Ok(answer) => return answer,
                Err(IpcError::Server { message, .. }) if message.contains("busy") => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("the call must succeed: {error}"),
            }
        }

        panic!("the server never freed a slot");
    }

    /// Waits until `condition` holds, failing the test after five seconds.
    fn wait_for(condition: impl Fn() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(5);

        while !condition() {
            assert!(Instant::now() < deadline, "timed out waiting until {what}");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn an_echo_call_round_trips() {
        let (_server, pipe) = echo_server();

        let result = call(
            &pipe,
            "echo",
            Some(json!({ "text": "привет" })),
            DEFAULT_TIMEOUT,
        )
        .expect("call must succeed");

        assert_eq!(result, json!({ "text": "привет" }));
    }

    #[test]
    fn a_handler_failure_arrives_as_the_servers_message() {
        let (_server, pipe) = echo_server();

        match call(&pipe, "explode", None, DEFAULT_TIMEOUT) {
            Err(IpcError::Server { method, message }) => {
                assert_eq!(method, "explode");
                assert_eq!(message, "unknown method: explode");
            }
            other => panic!("expected the server's error, got {other:?}"),
        }
    }

    #[test]
    fn the_error_message_is_the_one_the_handler_wrote() {
        let pipe = unique_pipe("boom");
        let _server = serve(&pipe, |request: Request| {
            Response::failure(request.id, "диск занят")
        })
        .expect("start the server");

        let error = call(&pipe, "anything", None, DEFAULT_TIMEOUT).expect_err("must fail");

        assert_eq!(error.to_string(), "ipc call 'anything' failed: диск занят");
    }

    #[test]
    fn five_hundred_concurrent_calls_all_get_their_own_answer() {
        let (_server, pipe) = echo_server();

        let calls: Vec<_> = (0..500)
            .map(|index| {
                let pipe = pipe.clone();
                std::thread::spawn(move || echo_when_not_busy(&pipe, index))
            })
            .collect();

        for (index, call) in calls.into_iter().enumerate() {
            let result = call.join().expect("the calling thread must not panic");
            assert_eq!(result, json!(index));
        }
    }

    #[test]
    fn the_server_serves_the_cap_and_refuses_the_next_client() {
        let (server, pipe) = echo_server();

        // Hold exactly the cap's worth of connections open; each is accepted and parked in a read.
        let held: Vec<std::fs::File> = (0..MAX_CLIENTS).map(|_| raw_client(&pipe)).collect();
        wait_for(|| server.client_count() == MAX_CLIENTS, "all slots taken");

        // The next client is answered by the accept loop itself, without a thread or a buffer, and
        // the refusal is correlated to the request it sent.
        let mut extra = raw_client(&pipe);
        extra
            .write_all(b"{\"id\":\"busy-1\",\"method\":\"echo\"}\n")
            .expect("write the request that will be refused");
        let mut answer = String::new();
        BufReader::new(&mut extra)
            .read_line(&mut answer)
            .expect("the refusal must arrive");
        let value: Value = serde_json::from_str(&answer).expect("the refusal is json");

        assert_eq!(value["ok"], json!(false), "{value}");
        assert_eq!(value["id"], json!("busy-1"), "the refusal is correlated");
        let message = value["error"].as_str().expect("an error message");
        assert!(
            message.contains("busy") && message.contains(&MAX_CLIENTS.to_string()),
            "{message}"
        );
        assert_eq!(
            server.client_count(),
            MAX_CLIENTS,
            "a refusal must not consume a slot"
        );

        // Once the held clients go away the server serves again.
        drop(held);
        wait_for(|| server.client_count() == 0, "all slots released");
        let result = call(&pipe, "echo", Some(json!("alive")), DEFAULT_TIMEOUT)
            .expect("the server must still be serving");
        assert_eq!(result, json!("alive"));
    }

    #[cfg(panic = "unwind")]
    #[test]
    fn a_panicking_handler_answers_with_an_error_and_the_server_survives() {
        let pipe = unique_pipe("panic");
        let _server = serve(&pipe, |request: Request| {
            if request.method == "explode" {
                panic!("the handler blew up on purpose");
            }
            Response::success(request.id, json!("fine"))
        })
        .expect("start the server");

        match call(&pipe, "explode", None, DEFAULT_TIMEOUT) {
            Err(IpcError::Server { message, .. }) => {
                assert_eq!(message, "the ipc handler panicked");
            }
            other => panic!("expected the panic to become an error, got {other:?}"),
        }

        let result = call(&pipe, "ping", None, DEFAULT_TIMEOUT)
            .expect("a panicking handler must not stop the server");
        assert_eq!(result, json!("fine"));
    }

    #[test]
    fn a_request_id_is_read_the_way_the_c_sharp_server_reads_it() {
        assert_eq!(request_id(r#"{"id":"7","method":"ping"}"#), "7");
        assert_eq!(request_id("{\"id\":\"a\",\"method\":\"ping\"}\r\n"), "a");
        assert_eq!(request_id(r#"{"method":"ping"}"#), "");
        assert_eq!(request_id(r#"{"id":7,"method":"ping"}"#), "");
        assert_eq!(request_id("not json"), "");
        assert_eq!(request_id(""), "");
    }

    #[test]
    fn a_client_that_dies_mid_line_does_not_take_the_server_down() {
        let (_server, pipe) = echo_server();

        let mut client = raw_client(&pipe);
        client
            .write_all(br#"{"id":"1","method":"echo","par"#)
            .expect("write half a request");
        drop(client);

        let result = call(&pipe, "echo", Some(json!("still here")), DEFAULT_TIMEOUT)
            .expect("the server must serve the next client");

        assert_eq!(result, json!("still here"));
    }

    #[test]
    fn an_oversized_line_is_rejected_with_a_clear_error() {
        let (_server, pipe) = echo_server();

        let mut client = raw_client(&pipe);
        let line = vec![b'a'; MAX_LINE_BYTES + 4096];
        // The server answers as soon as the cap is passed and keeps draining, so this write ends.
        let _ = client.write_all(&line);
        let _ = client.write_all(b"\n");

        let mut reader = BufReader::new(client);
        let mut answer = String::new();
        reader.read_line(&mut answer).expect("read the rejection");
        let value: Value = serde_json::from_str(&answer).expect("the rejection is json");

        assert_eq!(value["ok"], json!(false));
        let message = value["error"].as_str().expect("an error message");
        assert!(message.contains("exceeds"), "{message}");
        assert_eq!(value["id"], json!(""));
    }

    #[test]
    fn a_malformed_line_gets_an_error_and_the_connection_stays_usable() {
        let (_server, pipe) = echo_server();

        let mut client = raw_client(&pipe);
        client
            .write_all(b"{\"id\":\"4\"}\n")
            .expect("write a request without a method");

        let mut reader = BufReader::new(&client);
        let mut answer = String::new();
        reader.read_line(&mut answer).expect("read the error");

        let value: Value = serde_json::from_str(&answer).expect("valid json");
        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error"], json!("ipc request.method is required"));

        let mut client = BufReader::new(client);
        client
            .get_mut()
            .write_all(b"{\"id\":\"5\",\"method\":\"echo\",\"params\":1}\n")
            .expect("write a valid request");
        let mut answer = String::new();
        client.read_line(&mut answer).expect("read the answer");
        let value: Value = serde_json::from_str(&answer).expect("valid json");
        assert_eq!(value, json!({ "id": "5", "ok": true, "result": 1 }));
    }

    #[test]
    fn an_empty_line_is_ignored() {
        let (_server, pipe) = echo_server();

        let mut client = raw_client(&pipe);
        client
            .write_all(b"\n\n{\"id\":\"1\",\"method\":\"echo\",\"params\":\"x\"}\n")
            .expect("write");

        let mut reader = BufReader::new(client);
        let mut answer = String::new();
        reader.read_line(&mut answer).expect("read");
        assert_eq!(answer.trim_end(), r#"{"id":"1","ok":true,"result":"x"}"#);
    }

    #[test]
    fn a_slow_handler_times_out_with_a_typed_error() {
        let pipe = unique_pipe("slow");
        let _server = serve(&pipe, |request: Request| {
            std::thread::sleep(Duration::from_millis(300));
            Response::success(request.id, json!("late"))
        })
        .expect("start the server");

        let started = Instant::now();
        let error =
            call(&pipe, "wait", None, Duration::from_millis(60)).expect_err("must time out");

        match error {
            IpcError::Timeout { method, timeout } => {
                assert_eq!(method, "wait");
                assert_eq!(timeout, Duration::from_millis(60));
            }
            other => panic!("expected a timeout, got {other:?}"),
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the timeout must be honoured, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn calling_a_pipe_nobody_listens_on_says_so() {
        let pipe = unique_pipe("absent");

        match call(&pipe, "ping", None, Duration::from_millis(200)) {
            Err(IpcError::Unavailable { pipe: reported }) => assert_eq!(reported, pipe),
            other => panic!("expected an unavailable server, got {other:?}"),
        }
    }

    #[test]
    fn a_path_like_pipe_name_is_refused_when_serving() {
        match serve(r"foo\bar", |_| Response::empty("")) {
            Err(IpcError::InvalidPipeName(name)) => assert_eq!(name, r"foo\bar"),
            other => panic!("expected a rejected name, got {other:?}"),
        }
    }

    #[test]
    fn shutdown_releases_the_pipe_name() {
        let pipe = unique_pipe("shutdown");
        let server = serve(&pipe, |request: Request| Response::empty(request.id)).expect("serve");
        assert_eq!(server.pipe_name(), pipe);

        server.shutdown();

        // The name is free again: nothing answers on it.
        assert!(matches!(
            call(&pipe, "ping", None, Duration::from_millis(200)),
            Err(IpcError::Unavailable { .. })
        ));
    }
}
