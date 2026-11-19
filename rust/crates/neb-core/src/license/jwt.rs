//! JWT parsing and EdDSA verification for the licence and session tokens.
//!
//! The worker signs with an Ed25519 key and puts the key id in the header; the client verifies
//! against `/api/keys/jwks`. Nothing here accepts `alg: none`, a symmetric algorithm, a missing
//! expiry or a plan outside the two the product knows.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::ed25519;

/// Tolerated clock skew between the worker and this machine, in seconds.
pub const IAT_SKEW_SECONDS: i64 = 60;

/// Plans a token may name.
pub const PLANS: [&str; 2] = ["free", "pro"];

/// One public key from the JWKS document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationKey {
    /// `kid` the token header refers to.
    pub kid: String,
    /// Raw 32-byte Ed25519 public key.
    pub key: [u8; 32],
}

/// Claims of an entitlement token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LicenseClaims {
    /// Issuer (`projectneb`).
    pub iss: Option<String>,
    /// Audience (`projectneb-client`).
    pub aud: Option<String>,
    /// Discord id the entitlement belongs to.
    pub sub: String,
    /// `free` or `pro`.
    pub plan: String,
    /// End of the subscription, Unix seconds.
    pub until: Option<i64>,
    /// Devices the worker has registered, informational.
    pub devices: Vec<String>,
    /// Token kind; `license` for entitlements.
    pub kind: Option<String>,
    /// Issued at, Unix seconds.
    pub iat: i64,
    /// Expiry, Unix seconds.
    pub exp: i64,
}

/// Claims of a session token.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionClaims {
    /// Discord id.
    pub sub: String,
    /// Server-side session id.
    pub sid: Option<String>,
    /// Token kind; `session`.
    pub kind: Option<String>,
    /// Issued at, Unix seconds.
    pub iat: i64,
    /// Expiry, Unix seconds.
    pub exp: i64,
}

/// A token whose signature and header have been checked.
#[derive(Debug, Clone, PartialEq)]
pub struct DecodedToken {
    /// Decoded payload.
    pub payload: Value,
    /// Whether `exp` is already in the past. The signature is still valid.
    pub expired: bool,
}

/// Why a token was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TokenError {
    /// Not three base64url segments or not JSON.
    #[error("token is malformed")]
    Malformed,
    /// `alg` is anything but EdDSA; `none` lands here too.
    #[error("token algorithm is not EdDSA")]
    UnsupportedAlgorithm,
    /// The header names a key the JWKS document does not carry.
    #[error("token names an unknown key '{0}'")]
    UnknownKid(String),
    /// The signature does not verify.
    #[error("token signature is not valid")]
    BadSignature,
    /// `exp` is missing or not a number.
    #[error("token has no valid expiry")]
    MissingExpiry,
    /// `iat` is further in the future than [`IAT_SKEW_SECONDS`].
    #[error("token is issued in the future")]
    IssuedInFuture,
    /// The token's `exp` is already in the past.
    #[error("token has expired")]
    Expired,
    /// A required claim is missing.
    #[error("token is missing '{0}'")]
    MissingClaim(&'static str),
    /// `plan` is neither `free` nor `pro`.
    #[error("token names an unknown plan '{0}'")]
    InvalidPlan(String),
    /// The token is of the wrong kind for the caller.
    #[error("token kind is '{0}', not '{1}'")]
    WrongKind(String, &'static str),
}

/// Decodes `text` as unpadded base64url.
pub fn base64url_decode(text: &str) -> Result<Vec<u8>, TokenError> {
    let trimmed = text.trim_end_matches('=');
    if trimmed.len() % 4 == 1 {
        return Err(TokenError::Malformed);
    }

    let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
    let mut accumulator = 0u32;
    let mut bits = 0u32;

    for character in trimmed.bytes() {
        let value = match character {
            b'A'..=b'Z' => character - b'A',
            b'a'..=b'z' => character - b'a' + 26,
            b'0'..=b'9' => character - b'0' + 52,
            b'-' => 62,
            b'_' => 63,
            _ => return Err(TokenError::Malformed),
        };

        accumulator = (accumulator << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accumulator >> bits) as u8);
            accumulator &= (1 << bits) - 1;
        }
    }

    Ok(out)
}

/// Encodes bytes as unpadded base64url.
pub fn base64url_encode(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = u32::from(chunk[0]);
        let second = chunk.get(1).copied().map(u32::from).unwrap_or(0);
        let third = chunk.get(2).copied().map(u32::from).unwrap_or(0);
        let combined = (first << 16) | (second << 8) | third;

        out.push(ALPHABET[((combined >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((combined >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((combined >> 6) & 0x3f) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(combined & 0x3f) as usize] as char);
        }
    }

    out
}

fn json_object(bytes: &[u8]) -> Result<Value, TokenError> {
    let value: Value = serde_json::from_slice(bytes).map_err(|_| TokenError::Malformed)?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(TokenError::Malformed)
    }
}

/// Verifies the header and signature of `token` against `keys`.
///
/// Returns the payload even when it has expired, with [`DecodedToken::expired`] set: an expired
/// token is exactly what the offline grace period needs to read.
pub fn decode_verify(
    token: &str,
    keys: &[VerificationKey],
    now_seconds: i64,
) -> Result<DecodedToken, TokenError> {
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err(TokenError::Malformed);
    }
    let (encoded_header, encoded_payload, encoded_signature) = (parts[0], parts[1], parts[2]);

    let header = json_object(&base64url_decode(encoded_header)?)?;
    match header.get("alg").and_then(Value::as_str) {
        Some("EdDSA") => {}
        _ => return Err(TokenError::UnsupportedAlgorithm),
    }

    let kid = header
        .get("kid")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    let key = keys
        .iter()
        .find(|candidate| candidate.kid == kid)
        .or_else(|| {
            // A token without a kid is accepted only when the document has exactly one key.
            if kid.is_empty() && keys.len() == 1 {
                keys.first()
            } else {
                None
            }
        })
        .ok_or_else(|| TokenError::UnknownKid(kid.clone()))?;

    let signature: [u8; 64] = base64url_decode(encoded_signature)?
        .try_into()
        .map_err(|_| TokenError::BadSignature)?;

    let signing_input = format!("{encoded_header}.{encoded_payload}");
    if !ed25519::verify(&key.key, signing_input.as_bytes(), &signature) {
        return Err(TokenError::BadSignature);
    }

    let payload = json_object(&base64url_decode(encoded_payload)?)?;
    let exp = payload
        .get("exp")
        .and_then(Value::as_i64)
        .ok_or(TokenError::MissingExpiry)?;
    if let Some(iat) = payload.get("iat").and_then(Value::as_i64)
        && iat > now_seconds + IAT_SKEW_SECONDS
    {
        return Err(TokenError::IssuedInFuture);
    }

    Ok(DecodedToken {
        payload,
        expired: exp <= now_seconds,
    })
}

fn string_claim(payload: &Value, name: &'static str) -> Result<String, TokenError> {
    payload
        .get(name)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or(TokenError::MissingClaim(name))
}

fn number_claim(payload: &Value, name: &'static str) -> Result<i64, TokenError> {
    payload
        .get(name)
        .and_then(Value::as_i64)
        .ok_or(TokenError::MissingClaim(name))
}

/// Reads entitlement claims, rejecting a plan the product does not have.
pub fn license_claims(payload: &Value) -> Result<LicenseClaims, TokenError> {
    let plan = string_claim(payload, "plan")?.to_lowercase();
    if !PLANS.contains(&plan.as_str()) {
        return Err(TokenError::InvalidPlan(plan));
    }

    let devices = payload
        .get("devices")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    Ok(LicenseClaims {
        iss: payload
            .get("iss")
            .and_then(Value::as_str)
            .map(str::to_owned),
        aud: payload
            .get("aud")
            .and_then(Value::as_str)
            .map(str::to_owned),
        sub: string_claim(payload, "sub")?,
        plan,
        until: payload.get("until").and_then(Value::as_i64),
        devices,
        kind: payload
            .get("kind")
            .and_then(Value::as_str)
            .map(str::to_owned),
        iat: number_claim(payload, "iat")?,
        exp: number_claim(payload, "exp")?,
    })
}

/// Reads session claims, refusing a token of another kind.
pub fn session_claims(payload: &Value) -> Result<SessionClaims, TokenError> {
    let kind = payload
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if kind != "session" {
        return Err(TokenError::WrongKind(kind, "session"));
    }

    Ok(SessionClaims {
        sub: string_claim(payload, "sub")?,
        sid: payload
            .get("sid")
            .and_then(Value::as_str)
            .map(str::to_owned),
        kind: Some(kind),
        iat: number_claim(payload, "iat")?,
        exp: number_claim(payload, "exp")?,
    })
}

/// Full verification of an entitlement token, including the plan claim.
///
/// An expired token is returned with [`DecodedToken::expired`] set rather than refused: the
/// licence store decides whether the cached plan is still inside its grace window.
pub fn verify_license_token(
    token: &str,
    keys: &[VerificationKey],
    now_seconds: i64,
) -> Result<(LicenseClaims, bool), TokenError> {
    let decoded = decode_verify(token, keys, now_seconds)?;
    let claims = license_claims(&decoded.payload)?;
    Ok((claims, decoded.expired))
}

/// Full verification of a session token.
pub fn verify_session_token(
    token: &str,
    keys: &[VerificationKey],
    now_seconds: i64,
) -> Result<SessionClaims, TokenError> {
    let decoded = decode_verify(token, keys, now_seconds)?;
    if decoded.expired {
        return Err(TokenError::Expired);
    }
    session_claims(&decoded.payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_700_000_000;

    fn seed() -> [u8; 32] {
        [42u8; 32]
    }

    fn keys() -> Vec<VerificationKey> {
        vec![VerificationKey {
            kid: "test-1".to_owned(),
            key: ed25519::public_key(&seed()),
        }]
    }

    fn sign(header: Value, payload: Value, seed: [u8; 32]) -> String {
        let header = base64url_encode(header.to_string().as_bytes());
        let payload = base64url_encode(payload.to_string().as_bytes());
        let input = format!("{header}.{payload}");
        let signature = ed25519::sign(&seed, input.as_bytes());
        format!("{input}.{}", base64url_encode(&signature))
    }

    fn license(exp: i64) -> Value {
        json!({
            "iss": "projectneb",
            "aud": "projectneb-client",
            "sub": "123456789",
            "plan": "pro",
            "until": NOW + 86_400,
            "devices": ["device-a"],
            "kind": "license",
            "iat": NOW - 10,
            "exp": exp,
        })
    }

    fn header() -> Value {
        json!({ "alg": "EdDSA", "typ": "JWT", "kid": "test-1" })
    }

    #[test]
    fn a_valid_token_verifies_and_carries_its_claims() {
        let token = sign(header(), license(NOW + 900), seed());

        let (claims, expired) =
            verify_license_token(&token, &keys(), NOW).expect("the token verifies");

        assert!(!expired);
        assert_eq!(claims.plan, "pro");
        assert_eq!(claims.sub, "123456789");
        assert_eq!(claims.devices, vec!["device-a".to_owned()]);
        assert_eq!(claims.until, Some(NOW + 86_400));
    }

    #[test]
    fn an_expired_token_verifies_but_is_flagged() {
        let token = sign(header(), license(NOW - 1), seed());

        let (claims, expired) =
            verify_license_token(&token, &keys(), NOW).expect("the signature is still good");

        assert!(expired, "the caller must know the token is past its exp");
        assert_eq!(claims.plan, "pro");
    }

    #[test]
    fn a_signature_from_another_key_is_refused() {
        let token = sign(header(), license(NOW + 900), [9u8; 32]);

        assert_eq!(
            verify_license_token(&token, &keys(), NOW),
            Err(TokenError::BadSignature)
        );
    }

    #[test]
    fn an_unknown_kid_names_itself() {
        let mut header = header();
        header["kid"] = json!("rotated-away");
        let token = sign(header, license(NOW + 900), seed());

        assert_eq!(
            verify_license_token(&token, &keys(), NOW),
            Err(TokenError::UnknownKid("rotated-away".to_owned()))
        );
    }

    #[test]
    fn alg_none_is_refused_before_any_signature_check() {
        let header = base64url_encode(br#"{"alg":"none","typ":"JWT"}"#);
        let payload = base64url_encode(license(NOW + 900).to_string().as_bytes());
        let token = format!("{header}.{payload}.");

        assert_eq!(
            verify_license_token(&token, &keys(), NOW),
            Err(TokenError::UnsupportedAlgorithm)
        );
    }

    #[test]
    fn hs256_is_refused() {
        let header = base64url_encode(br#"{"alg":"HS256","typ":"JWT","kid":"test-1"}"#);
        let payload = base64url_encode(license(NOW + 900).to_string().as_bytes());
        let token = format!("{header}.{payload}.c2lnbmF0dXJl");

        assert_eq!(
            verify_license_token(&token, &keys(), NOW),
            Err(TokenError::UnsupportedAlgorithm)
        );
    }

    #[test]
    fn a_token_issued_far_in_the_future_is_refused() {
        let mut claims = license(NOW + 900);
        claims["iat"] = json!(NOW + IAT_SKEW_SECONDS + 5);
        let token = sign(header(), claims, seed());

        assert_eq!(
            verify_license_token(&token, &keys(), NOW),
            Err(TokenError::IssuedInFuture)
        );
    }

    #[test]
    fn a_small_clock_skew_is_tolerated() {
        let mut claims = license(NOW + 900);
        claims["iat"] = json!(NOW + IAT_SKEW_SECONDS - 5);
        let token = sign(header(), claims, seed());

        assert!(verify_license_token(&token, &keys(), NOW).is_ok());
    }

    #[test]
    fn a_plan_the_product_does_not_have_is_refused() {
        let mut claims = license(NOW + 900);
        claims["plan"] = json!("enterprise");
        let token = sign(header(), claims, seed());

        assert_eq!(
            verify_license_token(&token, &keys(), NOW),
            Err(TokenError::InvalidPlan("enterprise".to_owned()))
        );
    }

    #[test]
    fn a_token_without_exp_is_refused() {
        let mut claims = license(NOW + 900);
        claims.as_object_mut().expect("object").remove("exp");
        let token = sign(header(), claims, seed());

        assert_eq!(
            verify_license_token(&token, &keys(), NOW),
            Err(TokenError::MissingExpiry)
        );
    }

    #[test]
    fn a_session_token_is_read_with_its_own_claims() {
        let payload = json!({
            "sub": "123456789",
            "sid": "abc",
            "kind": "session",
            "iat": NOW,
            "exp": NOW + 2_592_000,
        });
        let token = sign(header(), payload, seed());

        let claims = verify_session_token(&token, &keys(), NOW).expect("session");
        assert_eq!(claims.sub, "123456789");
        assert_eq!(claims.sid.as_deref(), Some("abc"));
    }

    #[test]
    fn a_license_token_is_not_accepted_as_a_session() {
        let token = sign(header(), license(NOW + 900), seed());

        assert!(matches!(
            verify_session_token(&token, &keys(), NOW),
            Err(TokenError::WrongKind(_, "session"))
        ));
    }

    #[test]
    fn base64url_round_trips_and_rejects_foreign_characters() {
        for bytes in [b"".as_slice(), b"f", b"fo", b"foo", b"\xff\x00\x80"] {
            let encoded = base64url_encode(bytes);
            assert_eq!(base64url_decode(&encoded).expect("decode"), bytes);
        }

        assert!(base64url_decode("a+b/").is_err());
        assert!(base64url_decode("a").is_err());
    }
}
