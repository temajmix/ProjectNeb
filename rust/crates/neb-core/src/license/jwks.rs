//! The JWKS document and its on-disk cache.
//!
//! The worker publishes one or two Ed25519 keys (`/api/keys/jwks`). The client keeps the last good
//! document under `%LocalAppData%\ProjectNeb\cache\jwks.json` for twenty-four hours, and refreshes
//! it early when a token names a `kid` the cached document does not carry.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

use super::jwt::{VerificationKey, base64url_decode};

/// How long a cached JWKS document stays authoritative.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// What went wrong while reading a JWKS document.
#[derive(Debug, thiserror::Error)]
pub enum JwksError {
    /// The document is not the JSON shape the worker publishes.
    #[error("JWKS document is not valid: {0}")]
    Invalid(String),
    /// The cache file could not be read or written.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `read`.
        action: &'static str,
        /// The file involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// Parses `{"keys": [{kty:"OKP", crv:"Ed25519", x:"...", kid:"..."}]}`.
///
/// Keys of another type or curve are skipped: a JWKS document may legitimately carry keys this
/// client cannot use, and one of them must not hide the ones it can.
pub fn parse(document: &Value) -> Result<Vec<VerificationKey>, JwksError> {
    let entries = document
        .get("keys")
        .and_then(Value::as_array)
        .ok_or_else(|| JwksError::Invalid("'keys' is missing or not an array".to_owned()))?;

    let mut keys = Vec::new();
    for entry in entries {
        let Some(object) = entry.as_object() else {
            continue;
        };
        if object.get("kty").and_then(Value::as_str) != Some("OKP") {
            continue;
        }
        if object.get("crv").and_then(Value::as_str) != Some("Ed25519") {
            continue;
        }
        let Some(encoded) = object.get("x").and_then(Value::as_str) else {
            continue;
        };
        let Ok(bytes) = base64url_decode(encoded) else {
            continue;
        };
        let Ok(key) = <[u8; 32]>::try_from(bytes) else {
            continue;
        };
        let kid = object
            .get("kid")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();

        keys.push(VerificationKey { kid, key });
    }

    if keys.is_empty() {
        return Err(JwksError::Invalid(
            "no usable Ed25519 key in the document".to_owned(),
        ));
    }

    Ok(keys)
}

/// Reads every usable key from a JWKS file.
pub fn read_file(path: &Path) -> Result<Vec<VerificationKey>, JwksError> {
    let bytes = fs::read(path).map_err(|source| JwksError::Io {
        action: "read",
        path: path.to_path_buf(),
        source,
    })?;
    let document: Value = serde_json::from_slice(&bytes)
        .map_err(|error| JwksError::Invalid(format!("not json: {error}")))?;
    parse(&document)
}

/// Writes a JWKS document, creating its folder.
pub fn write_file(path: &Path, document: &Value) -> Result<(), JwksError> {
    if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|source| JwksError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut bytes = serde_json::to_vec_pretty(document)
        .map_err(|error| JwksError::Invalid(format!("cannot render: {error}")))?;
    bytes.push(b'\n');

    fs::write(path, bytes).map_err(|source| JwksError::Io {
        action: "write",
        path: path.to_path_buf(),
        source,
    })
}

/// Whether a cache file is younger than [`CACHE_TTL`].
pub fn is_fresh(path: &Path, now: SystemTime) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        return false;
    };

    match now.duration_since(modified) {
        Ok(age) => age < CACHE_TTL,
        // A file stamped in the future is treated as fresh rather than as a reason to hammer the API.
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tempfile::tempdir;

    fn document() -> Value {
        json!({
            "keys": [
                {
                    "kty": "OKP",
                    "crv": "Ed25519",
                    "x": "11qYAYKxCrfVS_7TyWQHOg7hcvPapiMlrwIaaPcHURo",
                    "kid": "test-1",
                    "use": "sig",
                    "alg": "EdDSA"
                },
                { "kty": "RSA", "n": "not-a-key", "kid": "ignored" }
            ]
        })
    }

    #[test]
    fn an_ed25519_key_is_read_and_other_key_types_are_skipped() {
        let keys = parse(&document()).expect("parse");

        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].kid, "test-1");
        assert_eq!(keys[0].key.len(), 32);
    }

    #[test]
    fn a_document_without_a_usable_key_is_an_error() {
        let error = parse(&json!({ "keys": [] })).expect_err("empty");
        assert!(error.to_string().contains("no usable Ed25519 key"));

        assert!(parse(&json!({})).is_err());
        assert!(parse(&json!({ "keys": [{ "kty": "OKP", "crv": "Ed25519", "x": "!" }] })).is_err());
    }

    #[test]
    fn a_written_cache_reads_back() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("cache").join("jwks.json");

        write_file(&path, &document()).expect("write");
        let keys = read_file(&path).expect("read");

        assert_eq!(keys[0].kid, "test-1");
        assert!(is_fresh(&path, SystemTime::now()));
    }

    #[test]
    fn a_missing_cache_is_not_fresh_and_not_panic() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("jwks.json");

        assert!(!is_fresh(&path, SystemTime::now()));
        assert!(read_file(&path).is_err());
    }

    #[test]
    fn an_old_cache_file_is_not_fresh() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("jwks.json");
        write_file(&path, &document()).expect("write");

        let future = SystemTime::now() + CACHE_TTL + Duration::from_secs(60);
        assert!(!is_fresh(&path, future));
    }
}
