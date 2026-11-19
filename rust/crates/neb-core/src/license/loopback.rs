//! The loopback listener that receives the browser's sign-in callback.
//!
//! `account.signIn` binds `127.0.0.1` on the documented port (or any free port when that one is
//! taken) and opens the worker's `/auth/discord/start` page with
//! `return=http://127.0.0.1:{port}/callback?state=...`. When Discord redirects back, the browser
//! lands here; the listener answers with a small page and never with a redirect chain.
//!
//! The worker puts the session token in the URL *fragment* (`#session=...`), which a browser never
//! sends to the server. The callback page therefore reads `location.hash` and re-requests the same
//! path with `?session=...`; both spellings are accepted. The `state` value is the nonce this process
//! generated, so a stray callback from another tab is refused instead of accepted.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::{Duration, Instant};

/// The documented port, tried first.
pub const PREFERRED_PORT: u16 = 47832;

/// How long one accepted connection may take to send its request line.
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Why the loopback wait ended without a session.
#[derive(Debug, thiserror::Error)]
pub enum LoopbackError {
    /// The listener could not be bound on any port.
    #[error("cannot listen on 127.0.0.1: {0}")]
    Bind(#[source] std::io::Error),
    /// No callback arrived before the deadline.
    #[error("the browser did not return a session within {0:?}")]
    Timeout(Duration),
    /// The worker reported a refusal in the callback.
    #[error("sign-in was refused: {0}")]
    Denied(String),
    /// The socket failed while answering.
    #[error("the loopback listener failed: {0}")]
    Io(#[source] std::io::Error),
}

/// A bound loopback listener.
#[derive(Debug)]
pub struct Loopback {
    listener: TcpListener,
    port: u16,
}

impl Loopback {
    /// Binds [`PREFERRED_PORT`], falling back to any free port on `127.0.0.1`.
    pub fn bind() -> Result<Self, LoopbackError> {
        let listener = TcpListener::bind(("127.0.0.1", PREFERRED_PORT))
            .or_else(|_| TcpListener::bind(("127.0.0.1", 0)))
            .map_err(LoopbackError::Bind)?;
        let port = listener.local_addr().map_err(LoopbackError::Bind)?.port();

        listener.set_nonblocking(true).map_err(LoopbackError::Io)?;

        Ok(Self { listener, port })
    }

    /// The port the browser must reach.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// The callback URL, carrying this attempt's `state` nonce.
    pub fn callback_url(&self, state: &str) -> String {
        format!(
            "http://127.0.0.1:{}/callback?state={}",
            self.port,
            super::http::percent_encode(state)
        )
    }

    /// Waits for the callback that carries `expected_state` and returns its session token.
    ///
    /// Connections with another state, a missing session or an unrelated path are answered (so the
    /// browser shows something useful) and do not end the wait.
    pub fn wait(&self, expected_state: &str, timeout: Duration) -> Result<String, LoopbackError> {
        self.wait_with(expected_state, timeout, Instant::now)
    }

    fn wait_with<F>(
        &self,
        expected_state: &str,
        timeout: Duration,
        now: F,
    ) -> Result<String, LoopbackError>
    where
        F: Fn() -> Instant,
    {
        let deadline = now() + timeout;

        loop {
            if now() >= deadline {
                return Err(LoopbackError::Timeout(timeout));
            }

            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(session) = self.handle(stream, expected_state)? {
                        return Ok(session);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(15));
                }
                Err(error) => return Err(LoopbackError::Io(error)),
            }
        }
    }

    /// Handles one browser request; returns the session when this was the callback.
    fn handle(
        &self,
        mut stream: TcpStream,
        expected_state: &str,
    ) -> Result<Option<String>, LoopbackError> {
        stream.set_nonblocking(false).map_err(LoopbackError::Io)?;
        stream
            .set_read_timeout(Some(READ_TIMEOUT))
            .map_err(LoopbackError::Io)?;
        stream
            .set_write_timeout(Some(READ_TIMEOUT))
            .map_err(LoopbackError::Io)?;

        let mut request = Vec::new();
        let mut buffer = [0u8; 2048];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    request.extend_from_slice(&buffer[..read]);
                    if request.windows(4).any(|window| window == b"\r\n\r\n")
                        || request.len() > 8192
                    {
                        break;
                    }
                }
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    break;
                }
                Err(error) => return Err(LoopbackError::Io(error)),
            }
        }

        let first_line = String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let mut parts = first_line.split_whitespace();
        let _method = parts.next().unwrap_or_default();
        let target = parts.next().unwrap_or("/");

        let Some((path, query)) = target.split_once('?') else {
            respond(&mut stream, 200, LANDING_HTML)?;
            return Ok(None);
        };

        let parameters = parse_query(query);

        if let Some(error) = parameters
            .iter()
            .find(|(name, _)| name == "error_description" || name == "error")
            .map(|(_, value)| value.clone())
        {
            respond(&mut stream, 400, &denial_html(&error))?;
            return Err(LoopbackError::Denied(error));
        }

        let session = parameters
            .iter()
            .find(|(name, _)| name == "session")
            .map(|(_, value)| value.clone());

        let Some(session) = session else {
            // The fragment is only visible to the page; let it ask again with a query value.
            if path == "/callback" {
                respond(&mut stream, 200, FRAGMENT_BRIDGE_HTML)?;
                return Ok(None);
            }
            respond(&mut stream, 404, NOT_FOUND_HTML)?;
            return Ok(None);
        };

        let state = parameters
            .iter()
            .find(|(name, _)| name == "state")
            .map(|(_, value)| value.as_str())
            .unwrap_or_default();

        if state != expected_state {
            respond(&mut stream, 400, BAD_STATE_HTML)?;
            return Ok(None);
        }

        if session.is_empty() {
            respond(&mut stream, 400, BAD_STATE_HTML)?;
            return Ok(None);
        }

        respond(&mut stream, 200, SUCCESS_HTML)?;
        Ok(Some(session))
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &str) -> Result<(), LoopbackError> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "OK",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\ncontent-type: text/html; charset=utf-8\r\n\
         content-length: {}\r\nconnection: close\r\ncache-control: no-store\r\n\r\n{body}",
        body.len()
    );

    stream
        .write_all(response.as_bytes())
        .and_then(|()| stream.flush())
        .map_err(LoopbackError::Io)
}

/// Decodes `a=1&b=2`, percent-decoding names and values. `+` is a space.
fn parse_query(query: &str) -> Vec<(String, String)> {
    query
        .split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (name, value) = part.split_once('=').unwrap_or((part, ""));
            (percent_decode(name), percent_decode(value))
        })
        .collect()
}

fn percent_decode(value: &str) -> String {
    super::http::percent_decode(value)
}

const STYLE: &str = "font-family:Segoe UI,system-ui,sans-serif;background:#0b0a12;color:#f6f4ff;\
display:grid;place-items:center;height:100vh;margin:0";

const LANDING_HTML: &str = "<!doctype html><meta charset=utf-8><title>ProjectNeb</title>\
<body style=\"font-family:Segoe UI,system-ui,sans-serif;background:#0b0a12;color:#f6f4ff;\
display:grid;place-items:center;height:100vh;margin:0\"><p>ProjectNeb</p></body>";

const NOT_FOUND_HTML: &str = "<!doctype html><meta charset=utf-8><title>ProjectNeb</title>\
<body style=\"font-family:Segoe UI,system-ui,sans-serif;background:#0b0a12;color:#f6f4ff;\
display:grid;place-items:center;height:100vh;margin:0\"><p>Not found</p></body>";

const BAD_STATE_HTML: &str = "<!doctype html><meta charset=utf-8><title>ProjectNeb</title>\
<body style=\"font-family:Segoe UI,system-ui,sans-serif;background:#0b0a12;color:#f6f4ff;\
display:grid;place-items:center;height:100vh;margin:0\"><p>This sign-in link does not belong \
to this ProjectNeb window.</p></body>";

const SUCCESS_HTML: &str = "<!doctype html><meta charset=utf-8><title>ProjectNeb</title>\
<body style=\"font-family:Segoe UI,system-ui,sans-serif;background:#0b0a12;color:#f6f4ff;\
display:grid;place-items:center;height:100vh;margin:0;text-align:center\"><div>\
<h1 style=\"font-size:20px;margin:0 0 8px\">Готово / Signed in</h1>\
<p style=\"color:#b7b3c9;margin:0\">Можно закрыть эту вкладку. / You can close this tab.</p>\
</div></body>";

/// The worker redirects with `#session=...`; a fragment never reaches the server, so this page asks
/// again with the same value as a query parameter.
const FRAGMENT_BRIDGE_HTML: &str = "<!doctype html><meta charset=utf-8><title>ProjectNeb</title>\
<body style=\"font-family:Segoe UI,system-ui,sans-serif;background:#0b0a12;color:#f6f4ff;\
display:grid;place-items:center;height:100vh;margin:0\"><p>ProjectNeb…</p><script>\
const hash=new URLSearchParams(location.hash.replace(/^#/,'')),query=new URLSearchParams(location.search);\
const session=hash.get('session')||query.get('session');\
if(session){query.set('session',session);location.replace('/callback?'+query.toString());}\
</script></body>";

fn denial_html(message: &str) -> String {
    format!(
        "<!doctype html><meta charset=utf-8><title>ProjectNeb</title>\
         <body style=\"{STYLE}\"><p>Sign-in was refused: {message}</p></body>"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpStream;
    use std::thread;

    fn hit(url: &str) {
        let rest = url.strip_prefix("http://").expect("http url");
        let (authority, path) = rest.split_once('/').expect("path");
        let mut stream = TcpStream::connect(authority).expect("connect");
        stream
            .write_all(
                format!("GET /{path} HTTP/1.1\r\nHost: {authority}\r\nConnection: close\r\n\r\n")
                    .as_bytes(),
            )
            .expect("write");
        let mut response = String::new();
        stream.read_to_string(&mut response).expect("read");
        assert!(response.starts_with("HTTP/1.1 "), "{response}");
    }

    #[test]
    fn a_callback_with_the_right_state_returns_its_session() {
        let loopback = Loopback::bind().expect("bind");
        let state = "nonce";
        let url = loopback.callback_url(state);
        let callback = format!("{url}&session=session.token");
        let port = loopback.port();

        let caller = thread::spawn(move || hit(&callback));
        let session = loopback
            .wait(state, Duration::from_secs(5))
            .expect("a session");

        caller.join().expect("caller");
        assert_eq!(session, "session.token");
        assert_ne!(port, 0);
    }

    #[test]
    fn a_callback_with_another_state_is_refused_and_the_wait_continues() {
        let loopback = Loopback::bind().expect("bind");
        let state = "expected";
        let wrong = loopback.callback_url("someone-else");
        let right = loopback.callback_url(state);

        let wrong_port = loopback.port();
        let wrong_callback = format!("{wrong}&session=stolen.token");
        let right_callback = format!("{right}&session=real.token");

        let caller = thread::spawn(move || {
            hit(&wrong_callback);
            hit(&right_callback);
        });

        let session = loopback
            .wait(state, Duration::from_secs(5))
            .expect("the right callback still counts");

        caller.join().expect("caller");
        assert_eq!(session, "real.token");
        assert_ne!(wrong_port, 0);
    }

    #[test]
    fn a_timeout_is_reported() {
        let loopback = Loopback::bind().expect("bind");
        let started = Instant::now();

        let error = loopback
            .wait("nobody", Duration::from_millis(80))
            .expect_err("nothing arrives");

        assert!(matches!(error, LoopbackError::Timeout(_)));
        assert!(started.elapsed() >= Duration::from_millis(60));
    }

    #[test]
    fn a_fragment_only_callback_gets_the_bridge_page_and_does_not_finish() {
        let loopback = Loopback::bind().expect("bind");
        let url = loopback.callback_url("state");

        let caller = thread::spawn(move || hit(&url));

        let error = loopback
            .wait("state", Duration::from_millis(150))
            .expect_err("the fragment page is not a session yet");

        caller.join().expect("caller");
        assert!(matches!(error, LoopbackError::Timeout(_)));
    }

    #[test]
    fn an_error_parameter_ends_the_wait_with_the_message() {
        let loopback = Loopback::bind().expect("bind");
        let url = format!("{}&error=access_denied", loopback.callback_url("state"));

        let caller = thread::spawn(move || hit(&url));

        let error = loopback
            .wait("state", Duration::from_secs(5))
            .expect_err("denied");

        caller.join().expect("caller");
        assert!(
            matches!(error, LoopbackError::Denied(ref message) if message.contains("access_denied"))
        );
    }

    #[test]
    fn percent_encoded_values_are_decoded() {
        assert_eq!(
            parse_query("state=a%2Fb%3D&session=x%2By"),
            vec![
                ("state".to_owned(), "a/b=".to_owned()),
                ("session".to_owned(), "x+y".to_owned())
            ]
        );
    }
}
