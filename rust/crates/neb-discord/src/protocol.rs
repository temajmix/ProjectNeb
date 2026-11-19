//! The Discord IPC wire format.
//!
//! A frame is an 8-byte header followed by one UTF-8 JSON document:
//!
//! ```text
//! +----------------+----------------+----------------------+
//! | op   u32 LE    | len  u32 LE    | JSON payload (len)   |
//! +----------------+----------------+----------------------+
//! ```
//!
//! Logically the frame is `{ "op": 0|1|2|3|4, "d": <payload> }`. Discord keeps the opcode in the
//! binary header rather than inside the JSON, so [`Frame`] keeps those two halves together and
//! [`Frame::encode`] writes the pair. The payload of a handshake is `{v, client_id}`; every other
//! frame carries the documented command document (`cmd`, `args`, `nonce`) as its `d`.
//!
//! Every read is bounded by [`MAX_FRAME_BYTES`] before a buffer is allocated, so a broken or hostile
//! peer cannot make this process reserve an arbitrary amount of memory.

use std::io::{self, Read, Write};

use serde_json::{Value, json};

/// Largest payload accepted or produced, in bytes. Discord's own frames are far smaller; the limit
/// exists to stop a malformed length from becoming an allocation.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Bytes in front of every payload: the opcode and the payload length, both little-endian.
pub const HEADER_BYTES: usize = 8;

/// The five opcodes Discord documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u32)]
pub enum Opcode {
    /// First frame on a connection: `{v: 1, client_id}`.
    Handshake = 0,
    /// A command, a dispatch event or an error response.
    Frame = 1,
    /// The peer is closing the connection.
    Close = 2,
    /// Keepalive request; answer with [`Opcode::Pong`] and the same payload.
    Ping = 3,
    /// Keepalive answer.
    Pong = 4,
}

impl Opcode {
    /// The wire code.
    pub fn code(self) -> u32 {
        self as u32
    }

    /// Reads the wire code, refusing anything outside the documented set.
    ///
    /// # Errors
    ///
    /// [`ProtocolError::UnknownOpcode`] for a code this protocol does not define.
    pub fn from_code(code: u32) -> Result<Self, ProtocolError> {
        match code {
            0 => Ok(Self::Handshake),
            1 => Ok(Self::Frame),
            2 => Ok(Self::Close),
            3 => Ok(Self::Ping),
            4 => Ok(Self::Pong),
            other => Err(ProtocolError::UnknownOpcode(other)),
        }
    }
}

impl std::fmt::Display for Opcode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Handshake => "HANDSHAKE",
            Self::Frame => "FRAME",
            Self::Close => "CLOSE",
            Self::Ping => "PING",
            Self::Pong => "PONG",
        };
        formatter.write_str(name)
    }
}

/// One protocol frame: an opcode and the JSON document that travels with it.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    /// What the frame is.
    pub op: Opcode,
    /// The `d` of the logical `{op, d}` frame.
    pub payload: Value,
}

impl Frame {
    /// A bare frame.
    pub fn new(op: Opcode, payload: Value) -> Self {
        Self { op, payload }
    }

    /// The first frame on a connection.
    pub fn handshake(client_id: &str) -> Self {
        Self::new(Opcode::Handshake, json!({ "v": 1, "client_id": client_id }))
    }

    /// `SET_ACTIVITY`; `activity: None` clears the presence.
    pub fn set_activity(pid: u32, activity: Option<&Value>, nonce: &str) -> Self {
        Self::new(
            Opcode::Frame,
            json!({
                "cmd": "SET_ACTIVITY",
                "args": {
                    "pid": pid,
                    "activity": activity.cloned().unwrap_or(Value::Null),
                },
                "nonce": nonce,
            }),
        )
    }

    /// Any other command; kept public so a diagnostic can send `SUBSCRIBE` or a future command.
    pub fn command(command: &str, args: Value, nonce: &str) -> Self {
        Self::new(
            Opcode::Frame,
            json!({ "cmd": command, "args": args, "nonce": nonce }),
        )
    }

    /// The answer to a [`Opcode::Ping`], echoing its payload.
    pub fn pong(payload: Value) -> Self {
        Self::new(Opcode::Pong, payload)
    }

    /// A close notification.
    pub fn close(payload: Value) -> Self {
        Self::new(Opcode::Close, payload)
    }

    /// The command name, when the payload is a command document.
    pub fn command_name(&self) -> Option<&str> {
        self.payload.get("cmd").and_then(Value::as_str)
    }

    /// The event name, when the payload is a dispatch document.
    pub fn event_name(&self) -> Option<&str> {
        self.payload.get("evt").and_then(Value::as_str)
    }

    /// One line for a frame log: the opcode and the compact payload.
    pub fn summary(&self) -> String {
        format!(
            "op={} {} d={}",
            self.op.code(),
            self.op,
            serde_json::to_string(&self.payload).unwrap_or_else(|_| "<unrenderable>".to_owned())
        )
    }

    /// Renders the header and the payload.
    ///
    /// # Errors
    ///
    /// [`ProtocolError::TooLarge`] when the payload exceeds [`MAX_FRAME_BYTES`] and
    /// [`ProtocolError::Json`] when the value cannot be serialised.
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        let body = serde_json::to_vec(&self.payload)?;
        if body.len() > MAX_FRAME_BYTES {
            return Err(ProtocolError::TooLarge {
                size: body.len(),
                limit: MAX_FRAME_BYTES,
            });
        }

        let mut bytes = Vec::with_capacity(HEADER_BYTES + body.len());
        bytes.extend_from_slice(&self.op.code().to_le_bytes());
        bytes.extend_from_slice(&(body.len() as u32).to_le_bytes());
        bytes.extend_from_slice(&body);
        Ok(bytes)
    }

    /// Parses one complete frame (header and payload) from `bytes`.
    ///
    /// # Errors
    ///
    /// [`ProtocolError::Truncated`] when the buffer is shorter than the header promises, plus the
    /// same errors as [`Opcode::from_code`] and [`serde_json::from_slice`].
    pub fn decode(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < HEADER_BYTES {
            return Err(ProtocolError::Truncated {
                expected: HEADER_BYTES,
                actual: bytes.len(),
            });
        }

        let op = Opcode::from_code(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))?;
        let length = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize;
        if length > MAX_FRAME_BYTES {
            return Err(ProtocolError::TooLarge {
                size: length,
                limit: MAX_FRAME_BYTES,
            });
        }

        let expected = HEADER_BYTES + length;
        if bytes.len() < expected {
            return Err(ProtocolError::Truncated {
                expected,
                actual: bytes.len(),
            });
        }

        let payload = serde_json::from_slice(&bytes[HEADER_BYTES..expected])?;
        Ok(Self { op, payload })
    }
}

/// Writes one frame to `writer`.
///
/// # Errors
///
/// [`ProtocolError::Io`] when the pipe refuses the bytes, and the encoding errors of
/// [`Frame::encode`].
pub fn write_frame<W: Write>(writer: &mut W, frame: &Frame) -> Result<(), ProtocolError> {
    writer.write_all(&frame.encode()?)?;
    writer.flush()?;
    Ok(())
}

/// Reads one frame from `reader`, allocating only after the length has been checked.
///
/// # Errors
///
/// [`ProtocolError::Closed`] when the peer closed before the first header byte, [`ProtocolError::Io`]
/// for a short read in the middle of a frame, and the decoding errors of [`Frame::decode`].
pub fn read_frame<R: Read>(reader: &mut R) -> Result<Frame, ProtocolError> {
    let mut header = [0_u8; HEADER_BYTES];
    read_header(reader, &mut header)?;

    let op = Opcode::from_code(u32::from_le_bytes([
        header[0], header[1], header[2], header[3],
    ]))?;
    let length = u32::from_le_bytes([header[4], header[5], header[6], header[7]]) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(ProtocolError::TooLarge {
            size: length,
            limit: MAX_FRAME_BYTES,
        });
    }

    let mut body = vec![0_u8; length];
    reader.read_exact(&mut body)?;
    let payload = serde_json::from_slice(&body)?;

    Ok(Frame { op, payload })
}

/// Reads the fixed header, translating "nothing at all" into [`ProtocolError::Closed`].
fn read_header<R: Read>(
    reader: &mut R,
    header: &mut [u8; HEADER_BYTES],
) -> Result<(), ProtocolError> {
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Err(ProtocolError::Closed),
            Ok(0) => {
                return Err(ProtocolError::Io(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "the discord pipe closed in the middle of a frame header",
                )));
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(ProtocolError::Io(error)),
        }
    }

    Ok(())
}

/// Everything that can go wrong while framing.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    /// The pipe closed between frames.
    #[error("discord closed the ipc connection")]
    Closed,

    /// The pipe failed.
    #[error("discord ipc i/o failed: {0}")]
    Io(#[from] io::Error),

    /// The payload was not JSON.
    #[error("discord frame payload is not json: {0}")]
    Json(#[from] serde_json::Error),

    /// The opcode is not one of the five documented values.
    #[error("unknown discord ipc opcode {0}")]
    UnknownOpcode(u32),

    /// A frame is larger than [`MAX_FRAME_BYTES`].
    #[error("discord frame is {size} bytes, limit is {limit}")]
    TooLarge {
        /// Size the frame claims or needs.
        size: usize,
        /// Configured ceiling.
        limit: usize,
    },

    /// A buffer ended in the middle of a frame.
    #[error("discord frame is truncated: expected {expected} bytes, got {actual}")]
    Truncated {
        /// Bytes the header promises.
        expected: usize,
        /// Bytes actually present.
        actual: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn a_frame_encodes_to_the_little_endian_header_and_json_body() {
        let frame = Frame::handshake("123456789012345678");
        let bytes = frame.encode().expect("encode");
        let body = br#"{"v":1,"client_id":"123456789012345678"}"#;

        assert_eq!(&bytes[0..4], &[0, 0, 0, 0], "handshake opcode");
        assert_eq!(
            u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]) as usize,
            body.len()
        );
        assert_eq!(&bytes[HEADER_BYTES..], body);
    }

    #[test]
    fn every_opcode_round_trips() {
        for op in [
            Opcode::Handshake,
            Opcode::Frame,
            Opcode::Close,
            Opcode::Ping,
            Opcode::Pong,
        ] {
            let frame = Frame::new(op, json!({ "probe": op.code() }));
            let bytes = frame.encode().expect("encode");
            let back = Frame::decode(&bytes).expect("decode");

            assert_eq!(back, frame);
            assert!(matches!(Opcode::from_code(op.code()), Ok(decoded) if decoded == op));
        }
    }

    #[test]
    fn a_set_activity_frame_carries_our_pid_and_the_activity() {
        let activity = json!({ "details": "Working" });
        let frame = Frame::set_activity(4242, Some(&activity), "nonce-1");

        assert_eq!(frame.op, Opcode::Frame);
        assert_eq!(frame.command_name(), Some("SET_ACTIVITY"));
        assert_eq!(frame.payload["args"]["pid"], json!(4242));
        assert_eq!(frame.payload["args"]["activity"], activity);
        assert_eq!(frame.payload["nonce"], json!("nonce-1"));

        let cleared = Frame::set_activity(1, None, "n");
        assert_eq!(cleared.payload["args"]["activity"], Value::Null);
    }

    #[test]
    fn a_bare_zero_is_a_closed_connection_not_an_empty_frame() {
        let mut empty = Cursor::new(Vec::new());

        assert!(matches!(read_frame(&mut empty), Err(ProtocolError::Closed)));
    }

    #[test]
    fn reading_a_pipe_stream_returns_frames_in_order() {
        let first = Frame::handshake("app");
        let second = Frame::set_activity(7, None, "n");
        let mut bytes = first.encode().expect("first");
        bytes.extend(second.encode().expect("second"));

        let mut reader = Cursor::new(bytes);
        assert_eq!(read_frame(&mut reader).expect("first"), first);
        assert_eq!(read_frame(&mut reader).expect("second"), second);
        assert!(matches!(
            read_frame(&mut reader),
            Err(ProtocolError::Closed)
        ));
    }

    #[test]
    fn an_unknown_opcode_is_refused_by_number() {
        let bytes = [9, 0, 0, 0, 2, 0, 0, 0, b'{', b'}'];

        match Frame::decode(&bytes) {
            Err(ProtocolError::UnknownOpcode(9)) => {}
            other => panic!("expected opcode 9 to be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_truncated_header_or_body_is_refused_without_panicking() {
        let frame = Frame::new(Opcode::Frame, json!({ "cmd": "PING" }));
        let bytes = frame.encode().expect("encode");

        assert!(matches!(
            Frame::decode(&bytes[..4]),
            Err(ProtocolError::Truncated { .. })
        ));
        assert!(matches!(
            Frame::decode(&bytes[..bytes.len() - 2]),
            Err(ProtocolError::Truncated { .. })
        ));
    }

    #[test]
    fn a_length_above_the_limit_is_refused_before_allocating() {
        let mut bytes = vec![1, 0, 0, 0];
        bytes.extend_from_slice(&(MAX_FRAME_BYTES as u32 + 1).to_le_bytes());

        match Frame::decode(&bytes) {
            Err(ProtocolError::TooLarge { limit, .. }) => assert_eq!(limit, MAX_FRAME_BYTES),
            other => panic!("expected a size refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_pong_echoes_the_ping_payload() {
        let ping = Frame::new(Opcode::Ping, json!({ "nonce": "keepalive" }));
        let pong = Frame::pong(ping.payload.clone());

        assert_eq!(pong.op, Opcode::Pong);
        assert_eq!(pong.payload, ping.payload);
    }
}
