//! The wire contract, byte for byte what the C# build speaks.
//!
//! `src/Neb.Core/Ipc/IpcServer.cs` and `IpcClient.cs` define the format: one UTF-8 JSON object per
//! line, `\n` terminated, a request `{id, method, params?}`, a response `{id, ok, result}` or
//! `{id, ok, error}`. The field names, their order, the `result: null` of a successful call without
//! a value and the `id` tolerances (missing or non-string reads as `""`) all follow that code, so a
//! Rust process and a C# process can talk to each other on the same pipe.

use std::time::Duration;

use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::Value;

use super::IpcError;

/// Pipe name the application listens on when nothing else is specified.
pub const DEFAULT_PIPE_NAME: &str = "ProjectNeb.ipc";

/// Largest request or response line accepted, in bytes. A longer line is answered with an error
/// instead of being buffered, so a hostile or broken peer cannot grow the server without bound.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// How long [`call`](super::call) waits for a connection and a response by default.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// One request line.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    /// Correlation id echoed in the response; `""` when the peer sent none.
    #[serde(default)]
    pub id: String,
    /// Name of the operation the peer wants.
    pub method: String,
    /// Operation arguments; `None` when absent or JSON `null`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<Value>,
}

impl Request {
    /// A request without parameters.
    pub fn new(id: impl Into<String>, method: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            method: method.into(),
            params: None,
        }
    }

    /// A request carrying `params`.
    pub fn with_params(id: impl Into<String>, method: impl Into<String>, params: Value) -> Self {
        Self {
            id: id.into(),
            method: method.into(),
            params: Some(params),
        }
    }

    /// Parses one line the way `IpcServer.DispatchAsync` does.
    ///
    /// A missing or non-string `id` becomes `""`, `params: null` becomes [`None`], and a missing,
    /// empty or non-string `method` is the error the C# build reports.
    pub fn parse(line: &str) -> Result<Self, RequestError> {
        let value: Value =
            serde_json::from_str(line).map_err(|error| RequestError::Json(error.to_string()))?;

        let Value::Object(mut object) = value else {
            return Err(RequestError::NotAnObject);
        };

        let id = match object.remove("id") {
            Some(Value::String(id)) => id,
            _ => String::new(),
        };

        let method = match object.remove("method") {
            Some(Value::String(method)) if !method.is_empty() => method,
            _ => return Err(RequestError::MissingMethod),
        };

        let params = match object.remove("params") {
            Some(Value::Null) | None => None,
            Some(params) => Some(params),
        };

        Ok(Self { id, method, params })
    }
}

/// Why a line could not be read as a [`Request`]. The messages match the C# build's so a client
/// logging the server's answer reads the same text whichever process answered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RequestError {
    /// The line was not a JSON object.
    #[error("ipc request must be a json object")]
    NotAnObject,
    /// The line had no usable `method`.
    #[error("ipc request.method is required")]
    MissingMethod,
    /// The line was not JSON at all.
    #[error("ipc request is not valid json: {0}")]
    Json(String),
}

/// One response line.
///
/// Serialisation is hand-written so the envelope keeps the C# shape exactly: a success always
/// carries `result` (JSON `null` when there is nothing to return), a failure carries `error` and no
/// `result` at all.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Response {
    /// Correlation id copied from the request.
    #[serde(default)]
    pub id: String,
    /// Whether the handler produced a result.
    #[serde(default)]
    pub ok: bool,
    /// The handler's value when `ok`.
    #[serde(default)]
    pub result: Option<Value>,
    /// The handler's message when not `ok`.
    #[serde(default)]
    pub error: Option<String>,
}

impl Response {
    /// A successful response carrying `result`.
    pub fn success(id: impl Into<String>, result: Value) -> Self {
        Self {
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    /// A successful response with no value; serialised as `"result": null`.
    pub fn empty(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ok: true,
            result: None,
            error: None,
        }
    }

    /// A failed response carrying `message`.
    pub fn failure(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ok: false,
            result: None,
            error: Some(message.into()),
        }
    }

    /// Renders the response as one UTF-8 line, newline included.
    pub fn to_line(&self) -> Result<Vec<u8>, IpcError> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }
}

impl Serialize for Response {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut map = serializer.serialize_map(Some(4))?;
        map.serialize_entry("id", &self.id)?;
        map.serialize_entry("ok", &self.ok)?;

        if self.ok {
            map.serialize_entry("result", &self.result.clone().unwrap_or(Value::Null))?;
        } else {
            map.serialize_entry("error", &self.error.as_deref().unwrap_or(FALLBACK_ERROR))?;
        }

        map.end()
    }
}

/// The message a failed response carries when the handler forgot to set one.
pub(crate) const FALLBACK_ERROR: &str = "ipc handler failed";

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope(response: &Response) -> Value {
        serde_json::from_slice(&response.to_line().expect("render")).expect("valid json")
    }

    #[test]
    fn a_request_parses_like_the_c_sharp_server() {
        let request =
            Request::parse(r#"{"id":"7","method":"ping","params":{"depth":2}}"#).expect("parse");

        assert_eq!(request.id, "7");
        assert_eq!(request.method, "ping");
        assert_eq!(request.params, Some(json!({ "depth": 2 })));
    }

    #[test]
    fn missing_id_and_null_params_are_tolerated() {
        let request = Request::parse(r#"{"method":"ping","params":null}"#).expect("parse");

        assert_eq!(request.id, "");
        assert_eq!(request.params, None);
    }

    #[test]
    fn a_non_string_id_reads_as_empty() {
        let request = Request::parse(r#"{"id":42,"method":"ping"}"#).expect("parse");

        assert_eq!(request.id, "");
    }

    #[test]
    fn a_missing_or_empty_method_is_the_c_sharp_error() {
        assert_eq!(
            Request::parse(r#"{"id":"1"}"#),
            Err(RequestError::MissingMethod)
        );
        assert_eq!(
            Request::parse(r#"{"id":"1","method":""}"#),
            Err(RequestError::MissingMethod)
        );
        assert_eq!(
            Request::parse(r#"{"id":"1","method":7}"#),
            Err(RequestError::MissingMethod)
        );
        assert_eq!(
            RequestError::MissingMethod.to_string(),
            "ipc request.method is required"
        );
    }

    #[test]
    fn a_non_object_line_is_rejected_with_the_c_sharp_message() {
        assert_eq!(Request::parse("[1,2,3]"), Err(RequestError::NotAnObject));
        assert_eq!(
            RequestError::NotAnObject.to_string(),
            "ipc request must be a json object"
        );
    }

    #[test]
    fn broken_json_reports_the_parse_error() {
        match Request::parse("{not json") {
            Err(RequestError::Json(message)) => assert!(message.contains("key must be a string")),
            other => panic!("expected a json error, got {other:?}"),
        }
    }

    #[test]
    fn a_success_always_carries_a_result_key() {
        assert_eq!(
            envelope(&Response::success("1", json!("pong"))),
            json!({ "id": "1", "ok": true, "result": "pong" })
        );
        assert_eq!(
            envelope(&Response::empty("2")),
            json!({ "id": "2", "ok": true, "result": null })
        );
    }

    #[test]
    fn a_failure_carries_only_an_error() {
        let value = envelope(&Response::failure("3", "boom"));

        assert_eq!(value, json!({ "id": "3", "ok": false, "error": "boom" }));
        assert!(
            value.get("result").is_none(),
            "the C# envelope has no result key on failure: {value}"
        );
    }

    #[test]
    fn the_field_order_matches_the_c_sharp_writer() {
        let text = String::from_utf8(Response::success("9", json!(1)).to_line().expect("render"))
            .expect("utf-8");

        assert_eq!(text, "{\"id\":\"9\",\"ok\":true,\"result\":1}\n");
    }

    #[test]
    fn a_response_deserialises_from_the_c_sharp_envelope() {
        let response: Response =
            serde_json::from_str(r#"{"id":"5","ok":false,"error":"nope"}"#).expect("parse");

        assert_eq!(response.id, "5");
        assert!(!response.ok);
        assert_eq!(response.result, None);
        assert_eq!(response.error.as_deref(), Some("nope"));
    }
}
