//! Token signing for the test suites and for the local stub worker.
//!
//! Compiled only under the `test-support` feature. A release build has no signing key and no code
//! that could produce an entitlement.

use serde_json::{Value, json};

use super::ed25519;
use super::jwt::{self, LicenseClaims};

/// Key id every test document uses unless it asks for another.
pub const TEST_KID: &str = "test-key";

/// The deterministic seed the suites sign with.
pub fn test_seed() -> [u8; 32] {
    [42u8; 32]
}

/// A JWKS document carrying the public key for `seed`.
pub fn jwks_document(seed: &[u8; 32], kid: &str) -> Value {
    json!({
        "keys": [{
            "kty": "OKP",
            "crv": "Ed25519",
            "x": jwt::base64url_encode(&ed25519::public_key(seed)),
            "kid": kid,
            "use": "sig",
            "alg": "EdDSA",
        }]
    })
}

/// Signs a payload with the test header (`alg: EdDSA`, `typ: JWT`, `kid`).
pub fn sign_payload(seed: &[u8; 32], kid: &str, payload: &Value) -> String {
    let header = json!({ "alg": "EdDSA", "typ": "JWT", "kid": kid });
    let header = jwt::base64url_encode(header.to_string().as_bytes());
    let payload = jwt::base64url_encode(payload.to_string().as_bytes());
    let signing_input = format!("{header}.{payload}");
    let signature = ed25519::sign(seed, signing_input.as_bytes());

    format!("{signing_input}.{}", jwt::base64url_encode(&signature))
}

/// Signs `claims` as an entitlement token.
pub fn sign_license(seed: &[u8; 32], kid: &str, claims: &LicenseClaims) -> String {
    let payload = serde_json::to_value(claims).expect("claims are serialisable");
    sign_payload(seed, kid, &payload)
}

/// Builds and signs an entitlement with the documented claims.
pub fn license_token(
    seed: &[u8; 32],
    kid: &str,
    sub: &str,
    plan: &str,
    iat: i64,
    exp: i64,
    until: i64,
) -> String {
    let payload = json!({
        "iss": "projectneb",
        "aud": "projectneb-client",
        "sub": sub,
        "plan": plan,
        "until": until,
        "devices": ["test-device"],
        "kind": "license",
        "iat": iat,
        "exp": exp,
    });

    sign_payload(seed, kid, &payload)
}

/// Builds and signs a session token with the documented claims.
pub fn session_token(seed: &[u8; 32], kid: &str, sub: &str, iat: i64, exp: i64) -> String {
    let payload = json!({
        "iss": "projectneb",
        "aud": "projectneb-client",
        "sub": sub,
        "sid": "test-session",
        "kind": "session",
        "iat": iat,
        "exp": exp,
    });

    sign_payload(seed, kid, &payload)
}
