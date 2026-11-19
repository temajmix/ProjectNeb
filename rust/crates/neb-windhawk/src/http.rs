//! One GET over WinHTTP, with the limits this module needs.
//!
//! WinHTTP is the system TLS stack, so no certificate bundle ships with ProjectNeb and the proxy
//! settings the user already configured are honoured. The function is deliberately small: the
//! catalog is one JSON listing and one text file per mod.

use std::ffi::c_void;
use std::mem::size_of;
use std::ptr;
use std::time::Duration;

use windows::Win32::Foundation::GetLastError;
use windows::Win32::Networking::WinHttp::{
    URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
    WINHTTP_ADDREQ_FLAG_ADD, WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTP,
    WINHTTP_INTERNET_SCHEME_HTTPS, WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_QUERY_FLAG_NUMBER,
    WINHTTP_QUERY_STATUS_CODE, WinHttpAddRequestHeaders, WinHttpCloseHandle, WinHttpConnect,
    WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest, WinHttpQueryHeaders, WinHttpReadData,
    WinHttpReceiveResponse, WinHttpSendRequest, WinHttpSetTimeouts,
};
use windows::core::PCWSTR;

/// Largest body accepted for a catalog listing.
pub const CATALOG_LIMIT: u64 = 8 * 1024 * 1024;

/// Largest body accepted for a single `.wh.cpp`.
pub const SOURCE_LIMIT: u64 = 4 * 1024 * 1024;

/// Why a GET failed.
#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    /// The URL is not an absolute HTTP or HTTPS URL WinHTTP can crack.
    #[error("invalid url '{0}'")]
    Url(String),

    /// A WinHTTP call failed.
    #[error("http {action} '{url}' failed (0x{code:08X})")]
    Win32 {
        /// Verbed operation.
        action: &'static str,
        /// URL attempted.
        url: String,
        /// Win32 error code.
        code: u32,
    },

    /// The server answered outside 2xx.
    #[error("http '{url}' answered status {status}")]
    Status {
        /// URL attempted.
        url: String,
        /// HTTP status.
        status: u32,
    },

    /// The body exceeded the caller's limit.
    #[error("http '{url}' answered more than {limit} bytes")]
    TooLarge {
        /// URL attempted.
        url: String,
        /// Caller's limit.
        limit: u64,
    },
}

/// Fetches `url` and returns the body.
///
/// # Errors
///
/// [`HttpError`] for a bad URL, a failed WinHTTP call, a non-2xx answer or a body past `limit`.
pub fn get(url: &str, timeout: Duration, limit: u64) -> Result<Vec<u8>, HttpError> {
    let endpoint = endpoint(url)?;
    let agent: Vec<u16> = format!("ProjectNeb/{}", env!("CARGO_PKG_VERSION"))
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let access = if endpoint.is_local() {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    };

    // SAFETY: the agent string is NUL-terminated and outlives the call; the proxy arguments are
    // ignored for these access types.
    let session = unsafe {
        WinHttpOpen(
            PCWSTR(agent.as_ptr()),
            access,
            PCWSTR::null(),
            PCWSTR::null(),
            0,
        )
    };
    if session.is_null() {
        return Err(win32("open", url));
    }
    let session = Handle(session);

    let millis = i32::try_from(timeout.as_millis())
        .unwrap_or(i32::MAX)
        .max(1);
    // SAFETY: `session` is a live handle; timeouts are plain numbers.
    unsafe {
        let _ = WinHttpSetTimeouts(session.0, millis, millis, millis, millis);
    }

    let host: Vec<u16> = endpoint
        .host
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the host is NUL-terminated and outlives the call.
    let connection = unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), endpoint.port, 0) };
    if connection.is_null() {
        return Err(win32("connect", url));
    }
    let connection = Handle(connection);

    let verb: Vec<u16> = "GET\0".encode_utf16().collect();
    let path: Vec<u16> = endpoint
        .path
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let flags = if endpoint.secure {
        WINHTTP_FLAG_SECURE
    } else {
        WINHTTP_OPEN_REQUEST_FLAGS(0)
    };

    // SAFETY: every string is NUL-terminated and outlives the call; the accept-types pointer is
    // the documented default (null).
    let request = unsafe {
        WinHttpOpenRequest(
            connection.0,
            PCWSTR(verb.as_ptr()),
            PCWSTR(path.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            ptr::null(),
            flags,
        )
    };
    if request.is_null() {
        return Err(win32("open request", url));
    }
    let request = Handle(request);

    // GitHub rejects requests without a User-Agent; the Accept header asks for the documented
    // contents representation.
    let headers: Vec<u16> = concat!(
        "User-Agent: ProjectNeb\r\n",
        "Accept: application/vnd.github+json\r\n"
    )
    .encode_utf16()
    .collect();
    // SAFETY: `request` is a live handle and `headers` is a live UTF-16 buffer of the length given.
    unsafe {
        let _ = WinHttpAddRequestHeaders(request.0, &headers, WINHTTP_ADDREQ_FLAG_ADD);
    }

    // SAFETY: `request` is live; no body and no extra headers are sent.
    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
        .map_err(|_| win32("send", url))?;
    // SAFETY: as above; a null reserved pointer is documented.
    unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) }
        .map_err(|_| win32("receive", url))?;

    let status = status(request.0).map_err(|_| win32("status", url))?;
    if !(200..300).contains(&status) {
        return Err(HttpError::Status {
            url: url.to_owned(),
            status,
        });
    }

    let mut body = Vec::new();
    let mut buffer = vec![0u8; 16 * 1024];

    loop {
        let mut read = 0u32;
        // SAFETY: `buffer` is a live writable array and `read` a live out-parameter.
        unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
        }
        .map_err(|_| win32("read", url))?;

        if read == 0 {
            break;
        }

        body.extend_from_slice(&buffer[..read as usize]);
        if body.len() as u64 > limit {
            return Err(HttpError::TooLarge {
                url: url.to_owned(),
                limit,
            });
        }
    }

    Ok(body)
}

/// One WinHTTP handle, closed exactly once.
struct Handle(*mut c_void);

impl Drop for Handle {
    fn drop(&mut self) {
        // SAFETY: the pointer came from a WinHTTP open call and is closed exactly once here.
        unsafe {
            let _ = WinHttpCloseHandle(self.0);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    host: String,
    path: String,
    port: u16,
    secure: bool,
}

impl Endpoint {
    /// A local test server must bypass the configured proxy, which usually cannot reach it.
    fn is_local(&self) -> bool {
        let host = self.host.to_ascii_lowercase();
        host == "localhost" || host == "::1" || host.starts_with("127.")
    }
}

fn endpoint(url: &str) -> Result<Endpoint, HttpError> {
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let mut parts = URL_COMPONENTS {
        dwStructSize: size_of::<URL_COMPONENTS>() as u32,
        ..URL_COMPONENTS::default()
    };
    parts.dwSchemeLength = u32::MAX;
    parts.dwHostNameLength = u32::MAX;
    parts.dwUrlPathLength = u32::MAX;
    parts.dwExtraInfoLength = u32::MAX;

    // SAFETY: `wide` is NUL-terminated and outlives the call; every pointer/length pair is filled
    // in by the API before it is read below.
    unsafe { WinHttpCrackUrl(&wide, 0, &mut parts) }.map_err(|_| HttpError::Url(url.to_owned()))?;

    let secure = match parts.nScheme {
        WINHTTP_INTERNET_SCHEME_HTTPS => true,
        WINHTTP_INTERNET_SCHEME_HTTP => false,
        _ => return Err(HttpError::Url(url.to_owned())),
    };

    let host = wide_slice(parts.lpszHostName, parts.dwHostNameLength);
    if host.is_empty() {
        return Err(HttpError::Url(url.to_owned()));
    }

    let mut path = wide_slice(parts.lpszUrlPath, parts.dwUrlPathLength);
    path.push_str(&wide_slice(parts.lpszExtraInfo, parts.dwExtraInfoLength));
    if !path.starts_with('/') {
        path.insert(0, '/');
    }

    Ok(Endpoint {
        host,
        path,
        port: parts.nPort,
        secure,
    })
}

fn wide_slice(pointer: windows::core::PWSTR, length: u32) -> String {
    if pointer.is_null() || length == 0 {
        return String::new();
    }

    // SAFETY: on success `WinHttpCrackUrl` points into the caller's URL buffer and reports the
    // number of UTF-16 units there; both outlive this copy.
    let slice = unsafe { std::slice::from_raw_parts(pointer.0, length as usize) };

    String::from_utf16_lossy(slice)
}

fn status(request: *mut c_void) -> Result<u32, windows::core::Error> {
    let mut status = 0u32;
    let mut length = size_of::<u32>() as u32;

    // SAFETY: `status` is a live u32 and `length` its size; the API writes both.
    unsafe {
        WinHttpQueryHeaders(
            request,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut length,
            ptr::null_mut(),
        )
    }?;

    Ok(status)
}

fn win32(action: &'static str, url: &str) -> HttpError {
    // SAFETY: `GetLastError` reads this thread's last error and changes nothing.
    let code = unsafe { GetLastError() }.0;

    HttpError::Win32 {
        action,
        url: url.to_owned(),
        code,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn urls_split_into_host_path_port_and_scheme() {
        let https =
            endpoint("https://api.github.com/repos/x/contents/mods?per_page=1").expect("url");
        assert_eq!(https.host, "api.github.com");
        assert_eq!(https.port, 443);
        assert!(https.secure);
        assert_eq!(https.path, "/repos/x/contents/mods?per_page=1");

        let local = endpoint("http://127.0.0.1:8080/get").expect("url");
        assert_eq!(local.port, 8080);
        assert!(!local.secure);
        assert!(local.is_local());
    }

    #[test]
    fn a_non_http_scheme_is_refused() {
        assert!(matches!(endpoint("file:///C:/x"), Err(HttpError::Url(_))));
        assert!(matches!(endpoint("not a url"), Err(HttpError::Url(_))));
    }

    #[test]
    fn a_local_server_answers_through_winhttp() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request);
            let body = b"[{\"name\":\"accent-color-sync.wh.cpp\"}]";
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).expect("head");
            socket.write_all(body).expect("body");
        });

        let body = get(
            &format!("http://127.0.0.1:{}/mods", address.port()),
            Duration::from_secs(5),
            1024 * 1024,
        )
        .expect("get");

        server.join().expect("server");
        assert_eq!(body, b"[{\"name\":\"accent-color-sync.wh.cpp\"}]");
    }

    #[test]
    fn a_body_past_the_limit_is_refused() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");

        let server = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().expect("accept");
            let mut request = [0u8; 1024];
            let _ = socket.read(&mut request);
            let body = vec![b'x'; 4096];
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).expect("head");
            let _ = socket.write_all(&body);
        });

        let error = get(
            &format!("http://127.0.0.1:{}/big", address.port()),
            Duration::from_secs(5),
            100,
        )
        .expect_err("too large");

        server.join().expect("server");
        assert!(matches!(error, HttpError::TooLarge { limit: 100, .. }));
    }
}
