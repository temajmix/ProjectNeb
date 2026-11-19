//! The small HTTP client the account flow needs.
//!
//! `http://` goes straight to a `TcpStream`: the tests and the loopback sign-in only ever talk to
//! `127.0.0.1`, and a direct socket keeps them independent of the machine's proxy configuration.
//! `https://` (the production worker) goes through WinHTTP, so the system TLS stack and proxy
//! settings apply without pulling a TLS implementation into the workspace.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

/// Timeout for one connect, send or receive step.
pub const HTTP_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest response body this client will accept.
pub const MAX_BODY: usize = 2 * 1024 * 1024;

/// One HTTP response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpResponse {
    /// Status code, e.g. `200`.
    pub status: u16,
    /// Raw body bytes.
    pub body: Vec<u8>,
}

impl HttpResponse {
    /// The body as UTF-8, with invalid bytes replaced.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Whether the status is `2xx`.
    pub fn ok(&self) -> bool {
        (200..300).contains(&self.status)
    }
}

/// Why a request did not produce a response.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The URL is not `http`/`https` or has no host.
    #[error("'{0}' is not an http(s) URL")]
    InvalidUrl(String),
    /// The socket refused or dropped the request.
    #[error("cannot reach '{url}': {message}")]
    Transport {
        /// The requested URL.
        url: String,
        /// What the socket or WinHTTP said.
        message: String,
    },
    /// The response did not look like HTTP.
    #[error("'{0}' did not answer with HTTP")]
    Malformed(String),
}

/// The parts of a URL this client needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Target {
    secure: bool,
    host: String,
    port: u16,
    path: String,
    url: String,
}

fn parse_target(url: &str) -> Result<Target, HttpError> {
    let (secure, rest) = if let Some(rest) = url.strip_prefix("https://") {
        (true, rest)
    } else if let Some(rest) = url.strip_prefix("http://") {
        (false, rest)
    } else {
        return Err(HttpError::InvalidUrl(url.to_owned()));
    };

    let (authority, path) = match rest.find('/') {
        Some(index) => (&rest[..index], &rest[index..]),
        None => (rest, "/"),
    };

    if authority.is_empty() {
        return Err(HttpError::InvalidUrl(url.to_owned()));
    }

    let (host, port) = if let Some(stripped) = authority.strip_prefix('[') {
        // IPv6 literal: [::1]:47832
        let Some((host, tail)) = stripped.split_once(']') else {
            return Err(HttpError::InvalidUrl(url.to_owned()));
        };
        let port = tail
            .strip_prefix(':')
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(if secure { 443 } else { 80 });
        (host.to_owned(), port)
    } else if let Some((host, port)) = authority.rsplit_once(':') {
        if port.chars().all(|character| character.is_ascii_digit()) {
            let parsed = port
                .parse::<u16>()
                .map_err(|_| HttpError::InvalidUrl(url.to_owned()))?;
            (host.to_owned(), parsed)
        } else {
            (authority.to_owned(), if secure { 443 } else { 80 })
        }
    } else {
        (authority.to_owned(), if secure { 443 } else { 80 })
    };

    if host.is_empty() {
        return Err(HttpError::InvalidUrl(url.to_owned()));
    }

    Ok(Target {
        secure,
        host,
        port,
        path: path.to_owned(),
        url: url.to_owned(),
    })
}

/// Percent-encodes everything outside the RFC 3986 unreserved set, for a query value.
pub fn percent_encode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// Decodes `%XX` escapes and `+` as a space, as a query value does.
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        match bytes[index] {
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap_or_default();
                match u8::from_str_radix(hex, 16) {
                    Ok(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    Err(_) => {
                        out.push(bytes[index]);
                        index += 1;
                    }
                }
            }
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8_lossy(&out).into_owned()
}

/// Sends a GET request.
pub fn get(url: &str, headers: &[(&str, &str)]) -> Result<HttpResponse, HttpError> {
    request("GET", url, headers, None)
}

/// Sends a POST with a JSON body.
pub fn post_json(
    url: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> Result<HttpResponse, HttpError> {
    request("POST", url, headers, Some(body))
}

fn request(
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse, HttpError> {
    let target = parse_target(url)?;
    if target.secure {
        native_request(&target, method, headers, body)
    } else {
        socket_request(&target, method, headers, body)
    }
}

fn header_block(headers: &[(&str, &str)]) -> String {
    let mut block = String::new();
    for (name, value) in headers {
        block.push_str(name);
        block.push_str(": ");
        block.push_str(value);
        block.push_str("\r\n");
    }
    block
}

fn socket_request(
    target: &Target,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse, HttpError> {
    let address = format!("{}:{}", target.host, target.port);
    let mut last_error = String::new();
    let mut stream = None;

    for socket_address in address
        .to_socket_addrs()
        .map_err(|error| HttpError::Transport {
            url: target.url.clone(),
            message: error.to_string(),
        })?
    {
        match TcpStream::connect_timeout(&socket_address, HTTP_TIMEOUT) {
            Ok(candidate) => {
                stream = Some(candidate);
                break;
            }
            Err(error) => last_error = error.to_string(),
        }
    }

    let Some(mut stream) = stream else {
        return Err(HttpError::Transport {
            url: target.url.clone(),
            message: if last_error.is_empty() {
                "no address answered".to_owned()
            } else {
                last_error
            },
        });
    };

    let transport = |error: std::io::Error| HttpError::Transport {
        url: target.url.clone(),
        message: error.to_string(),
    };

    stream
        .set_read_timeout(Some(HTTP_TIMEOUT))
        .map_err(transport)?;
    stream
        .set_write_timeout(Some(HTTP_TIMEOUT))
        .map_err(transport)?;

    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}:{}\r\nConnection: close\r\n",
        target.path, target.host, target.port
    );
    head.push_str(&header_block(headers));
    if let Some(body) = body {
        head.push_str(&format!(
            "content-length: {}\r\ncontent-type: application/json\r\n",
            body.len()
        ));
    }
    head.push_str("\r\n");

    stream.write_all(head.as_bytes()).map_err(transport)?;
    if let Some(body) = body {
        stream.write_all(body).map_err(transport)?;
    }
    stream.flush().map_err(transport)?;

    let mut response = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => {
                response.extend_from_slice(&chunk[..read]);
                if response.len() > MAX_BODY {
                    return Err(HttpError::Malformed(target.url.clone()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => break,
            Err(error) => return Err(transport(error)),
        }
    }

    parse_response(&target.url, &response)
}

fn parse_response(url: &str, response: &[u8]) -> Result<HttpResponse, HttpError> {
    let separator = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| HttpError::Malformed(url.to_owned()))?;

    let head = String::from_utf8_lossy(&response[..separator]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| HttpError::Malformed(url.to_owned()))?;

    let mut chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("transfer-encoding")
            && value.trim().eq_ignore_ascii_case("chunked")
        {
            chunked = true;
        }
    }

    let body = &response[separator + 4..];
    let body = if chunked {
        decode_chunked(body).ok_or_else(|| HttpError::Malformed(url.to_owned()))?
    } else {
        body.to_vec()
    };

    Ok(HttpResponse { status, body })
}

fn decode_chunked(mut body: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::new();

    loop {
        let end = body.windows(2).position(|window| window == b"\r\n")?;
        let size_text = std::str::from_utf8(&body[..end]).ok()?;
        let size_text = size_text.split(';').next()?.trim();
        let size = usize::from_str_radix(size_text, 16).ok()?;
        body = &body[end + 2..];

        if size == 0 {
            return Some(out);
        }
        if body.len() < size + 2 {
            return None;
        }

        out.extend_from_slice(&body[..size]);
        body = &body[size + 2..];
    }
}

// ---------- WinHTTP, for the production https endpoint ----------

#[cfg(windows)]
mod native {
    use std::ffi::c_void;
    use std::ptr;

    use super::{HTTP_TIMEOUT, HttpError, HttpResponse, MAX_BODY, Target};

    #[link(name = "winhttp")]
    unsafe extern "system" {
        fn WinHttpOpen(
            agent: *const u16,
            access_type: u32,
            proxy: *const u16,
            bypass: *const u16,
            flags: u32,
        ) -> *mut c_void;
        fn WinHttpConnect(
            session: *mut c_void,
            server: *const u16,
            port: u16,
            reserved: u32,
        ) -> *mut c_void;
        fn WinHttpOpenRequest(
            connect: *mut c_void,
            verb: *const u16,
            object: *const u16,
            version: *const u16,
            accept_types: *const *const u16,
            flags: u32,
        ) -> *mut c_void;
        fn WinHttpSendRequest(
            request: *mut c_void,
            headers: *const u16,
            headers_length: u32,
            optional: *const c_void,
            optional_length: u32,
            total_length: u32,
            context: usize,
        ) -> i32;
        fn WinHttpReceiveResponse(request: *mut c_void, reserved: *mut c_void) -> i32;
        fn WinHttpQueryHeaders(
            request: *mut c_void,
            info_level: u32,
            name: *const u16,
            buffer: *mut c_void,
            buffer_length: *mut u32,
            index: *mut u32,
        ) -> i32;
        fn WinHttpReadData(
            request: *mut c_void,
            buffer: *mut c_void,
            to_read: u32,
            read: *mut u32,
        ) -> i32;
        fn WinHttpSetTimeouts(
            handle: *mut c_void,
            resolve: i32,
            connect: i32,
            send: i32,
            receive: i32,
        ) -> i32;
        fn WinHttpCloseHandle(handle: *mut c_void) -> i32;
    }

    const ACCESS_TYPE_AUTOMATIC_PROXY: u32 = 4;
    const FLAG_SECURE: u32 = 0x0080_0000;
    const QUERY_STATUS_CODE: u32 = 19;
    const QUERY_FLAG_NUMBER: u32 = 0x2000_0000;

    /// Owns one WinHTTP handle and closes it on drop.
    struct Handle(*mut c_void);

    impl Drop for Handle {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: the pointer came from WinHttpOpen/Connect/OpenRequest and is closed once.
                unsafe { WinHttpCloseHandle(self.0) };
            }
        }
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    pub(super) fn request(
        target: &Target,
        method: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, HttpError> {
        let transport = |message: String| HttpError::Transport {
            url: target.url.clone(),
            message,
        };

        let agent = wide("ProjectNeb");
        // SAFETY: the agent string is NUL-terminated and outlives the call.
        let session = Handle(unsafe {
            WinHttpOpen(
                agent.as_ptr(),
                ACCESS_TYPE_AUTOMATIC_PROXY,
                ptr::null(),
                ptr::null(),
                0,
            )
        });
        if session.0.is_null() {
            return Err(transport(std::io::Error::last_os_error().to_string()));
        }

        let timeout = HTTP_TIMEOUT.as_millis() as i32;
        // SAFETY: the session handle is live.
        unsafe { WinHttpSetTimeouts(session.0, timeout, timeout, timeout, timeout) };

        let host = wide(&target.host);
        // SAFETY: the host string is NUL-terminated and outlives the call.
        let connect = Handle(unsafe { WinHttpConnect(session.0, host.as_ptr(), target.port, 0) });
        if connect.0.is_null() {
            return Err(transport(std::io::Error::last_os_error().to_string()));
        }

        let verb = wide(method);
        let object = wide(&target.path);
        let flags = if target.secure { FLAG_SECURE } else { 0 };
        // SAFETY: verb and object are NUL-terminated; the accept-type list is optional and null.
        let request = Handle(unsafe {
            WinHttpOpenRequest(
                connect.0,
                verb.as_ptr(),
                object.as_ptr(),
                ptr::null(),
                ptr::null(),
                flags,
            )
        });
        if request.0.is_null() {
            return Err(transport(std::io::Error::last_os_error().to_string()));
        }

        let header_text = super::header_block(headers);
        let header_wide = wide(&header_text);
        let (optional, optional_length) = match body {
            Some(bytes) => (bytes.as_ptr().cast(), bytes.len() as u32),
            None => (ptr::null(), 0),
        };

        // SAFETY: every pointer refers to a live local buffer for the duration of the call.
        let sent = unsafe {
            WinHttpSendRequest(
                request.0,
                header_wide.as_ptr(),
                u32::MAX,
                optional,
                optional_length,
                optional_length,
                0,
            )
        };
        if sent == 0 {
            return Err(transport(std::io::Error::last_os_error().to_string()));
        }

        // SAFETY: the request handle is live.
        if unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) } == 0 {
            return Err(transport(std::io::Error::last_os_error().to_string()));
        }

        let mut status: u32 = 0;
        let mut status_length = std::mem::size_of::<u32>() as u32;
        // SAFETY: `status` is a live u32 and the length names its size.
        let queried = unsafe {
            WinHttpQueryHeaders(
                request.0,
                QUERY_STATUS_CODE | QUERY_FLAG_NUMBER,
                ptr::null(),
                (&mut status as *mut u32).cast(),
                &mut status_length,
                ptr::null_mut(),
            )
        };
        if queried == 0 {
            return Err(transport(std::io::Error::last_os_error().to_string()));
        }

        let mut body = Vec::new();
        let mut buffer = [0u8; 8192];
        loop {
            let mut read: u32 = 0;
            // SAFETY: the buffer is live and its length is declared.
            let ok = unsafe {
                WinHttpReadData(
                    request.0,
                    buffer.as_mut_ptr().cast(),
                    buffer.len() as u32,
                    &mut read,
                )
            };
            if ok == 0 {
                return Err(transport(std::io::Error::last_os_error().to_string()));
            }
            if read == 0 {
                break;
            }
            body.extend_from_slice(&buffer[..read as usize]);
            if body.len() > MAX_BODY {
                return Err(HttpError::Malformed(target.url.clone()));
            }
        }

        Ok(HttpResponse {
            status: status as u16,
            body,
        })
    }
}

#[cfg(not(windows))]
mod native {
    use super::{HttpError, HttpResponse, Target};

    pub(super) fn request(
        target: &Target,
        _method: &str,
        _headers: HeaderMap<'_>,
        _body: Option<&[u8]>,
    ) -> Result<HttpResponse, HttpError> {
        Err(HttpError::Transport {
            url: target.url.clone(),
            message: "https needs Windows; use http:// for a local stub".to_owned(),
        })
    }
}

#[cfg(windows)]
fn native_request(
    target: &Target,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse, HttpError> {
    native::request(target, method, headers, body)
}

#[cfg(not(windows))]
fn native_request(
    target: &Target,
    method: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
) -> Result<HttpResponse, HttpError> {
    native::request(target, method, headers, body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn serve_once(response: &'static [u8]) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();

        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut request = Vec::new();
            let mut buffer = [0u8; 4096];
            let header_end;
            loop {
                let read = stream.read(&mut buffer).unwrap_or(0);
                if read == 0 {
                    header_end = request.len();
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n")
                {
                    header_end = position + 4;
                    break;
                }
            }

            let head = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
            let expected = head
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            while request.len() < header_end + expected {
                let read = stream.read(&mut buffer).unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }

            stream.write_all(response).expect("write");
            String::from_utf8_lossy(&request).into_owned()
        });

        (format!("http://127.0.0.1:{port}/api/me"), handle)
    }

    #[test]
    fn a_get_reads_a_local_response() {
        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: 11\r\n\r\n{\"plan\":\"x\"}",
        );

        let response = get(&url, &[("accept", "application/json")]).expect("get");

        assert_eq!(response.status, 200);
        assert_eq!(response.text(), "{\"plan\":\"x\"}");

        let request = server.join().expect("server");
        assert!(request.starts_with("GET /api/me HTTP/1.1\r\n"), "{request}");
        assert!(request.contains("accept: application/json"), "{request}");
    }

    #[test]
    fn a_post_sends_the_body_and_headers() {
        let (url, server) = serve_once(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nok");

        let response = post_json(
            &url,
            br#"{"deviceId":"d"}"#,
            &[("authorization", "Bearer token")],
        )
        .expect("post");

        assert_eq!(response.text(), "ok");

        let request = server.join().expect("server");
        assert!(request.starts_with("POST /api/me HTTP/1.1"), "{request}");
        assert!(request.contains("authorization: Bearer token"), "{request}");
        assert!(request.contains("{\"deviceId\":\"d\"}"), "{request}");
    }

    #[test]
    fn a_chunked_body_is_decoded() {
        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\ntransfer-encoding: chunked\r\n\r\n5\r\nhello\r\n0\r\n\r\n",
        );

        let response = get(&url, &[]).expect("get");
        assert_eq!(response.text(), "hello");
        server.join().expect("server");
    }

    #[test]
    fn a_non_http_url_is_refused_before_touching_a_socket() {
        assert!(matches!(
            get("ftp://example.invalid/", &[]),
            Err(HttpError::InvalidUrl(_))
        ));
        assert!(matches!(
            get("http:///path", &[]),
            Err(HttpError::InvalidUrl(_))
        ));
    }

    #[test]
    fn a_closed_port_is_a_transport_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        drop(listener);

        let error = get(&format!("http://127.0.0.1:{port}/"), &[]).expect_err("refused");
        assert!(matches!(error, HttpError::Transport { .. }));
    }

    #[test]
    fn a_query_value_is_percent_encoded() {
        assert_eq!(
            percent_encode("http://127.0.0.1:1/callback?state=a b"),
            "http%3A%2F%2F127.0.0.1%3A1%2Fcallback%3Fstate%3Da%20b"
        );
    }
}
