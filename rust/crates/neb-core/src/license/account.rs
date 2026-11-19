//! The worker-side account API and the documents the UI reads.
//!
//! Every network call lives here so the bridge stays a thin dispatcher: JWKS fetch with the 24-hour
//! cache, `/api/me`, `/api/license/refresh`, and the token checks that go with them. The functions
//! take an API base and a cache path, so a test can point them at a local `TcpListener`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::http::{self, HttpError};
use super::jwks::{self, JwksError};
use super::jwt::{self, LicenseClaims, SessionClaims, TokenError, VerificationKey};
use super::store::{self, LicenseStore, Plan, Profile, StoredLicense};
use crate::time::UtcDateTime;

/// What the worker said about the signed-in account.
#[derive(Debug, Clone, PartialEq)]
pub struct Me {
    /// Identity to show in the UI.
    pub profile: Profile,
    /// Plan from `/api/me`.
    pub plan: Plan,
    /// Subscription end, Unix seconds.
    pub until: Option<i64>,
}

/// Why an account call failed.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// The network call did not produce a response.
    #[error(transparent)]
    Http(#[from] HttpError),
    /// The worker answered with an error document.
    #[error("the server refused '{code}' (HTTP {status}): {message}")]
    Api {
        /// HTTP status.
        status: u16,
        /// Machine code, e.g. `upgrade_required`.
        code: String,
        /// Human message.
        message: String,
    },
    /// A response was not the JSON this client expected.
    #[error("the server answered with an unexpected document: {0}")]
    Json(String),
    /// A token did not verify.
    #[error("token: {0}")]
    Token(#[from] TokenError),
    /// The JWKS document or its cache failed.
    #[error("keys: {0}")]
    Jwks(#[from] JwksError),
    /// The licence file could not be read or written.
    #[error("licence store: {0}")]
    Store(#[from] store::StoreError),
    /// `apiBase` is empty: the build has no service to talk to.
    #[error("account.apiBase is not configured")]
    NotConfigured,
}

/// One worker endpoint pair plus its JWKS cache.
#[derive(Debug, Clone)]
pub struct AccountApi {
    base: String,
    cache: PathBuf,
}

impl AccountApi {
    /// An API for `api_base` (trailing slashes tolerated) with the JWKS cache at `cache`.
    pub fn new(api_base: &str, cache: PathBuf) -> Self {
        Self {
            base: api_base.trim().trim_end_matches('/').to_owned(),
            cache,
        }
    }

    /// The default cache under `%LocalAppData%\ProjectNeb\cache\jwks.json`.
    pub fn at_default_cache(api_base: &str) -> Self {
        Self::new(api_base, crate::paths::jwks_cache_file())
    }

    /// Whether the base is set at all.
    pub fn configured(&self) -> bool {
        !self.base.is_empty()
    }

    /// The browser page that starts the Discord flow, with our loopback as `return`.
    pub fn start_url(&self, return_url: &str) -> String {
        format!(
            "{}/auth/discord/start?return={}",
            self.base,
            http::percent_encode(return_url)
        )
    }

    fn endpoint(&self, path: &str) -> Result<String, AccountError> {
        if !self.configured() {
            return Err(AccountError::NotConfigured);
        }
        Ok(format!("{}{path}", self.base))
    }

    /// The verification keys, from cache when it is younger than 24 hours.
    ///
    /// `force` bypasses the cache; a network failure still falls back to the cached document so an
    /// expired entitlement can be evaluated offline.
    pub fn jwks(&self, force: bool) -> Result<Vec<VerificationKey>, AccountError> {
        if !force && jwks::is_fresh(&self.cache, std::time::SystemTime::now()) {
            if let Ok(keys) = jwks::read_file(&self.cache) {
                return Ok(keys);
            }
        }

        let url = self.endpoint("/api/keys/jwks")?;
        let fetched = http::get(&url, &[("accept", "application/json")]).and_then(|response| {
            if response.ok() {
                Ok(response)
            } else {
                Err(HttpError::Malformed(url.clone()))
            }
        });

        match fetched {
            Ok(response) => {
                let document: Value = serde_json::from_slice(&response.body)
                    .map_err(|error| AccountError::Json(error.to_string()))?;
                let keys = jwks::parse(&document)?;
                // A cache write failure must not take a working key offline.
                let _ = jwks::write_file(&self.cache, &document);
                Ok(keys)
            }
            Err(error) if !force => match jwks::read_file(&self.cache) {
                Ok(keys) => Ok(keys),
                Err(_) => Err(AccountError::Http(error)),
            },
            Err(error) => Err(AccountError::Http(error)),
        }
    }

    /// Verifies an entitlement, refreshing JWKS once when the token names an unknown `kid`.
    pub fn verify_license(
        &self,
        token: &str,
        now: i64,
    ) -> Result<(LicenseClaims, bool), AccountError> {
        let keys = self.jwks(false)?;
        match jwt::verify_license_token(token, &keys, now) {
            Err(TokenError::UnknownKid(_)) => {
                let keys = self.jwks(true)?;
                Ok(jwt::verify_license_token(token, &keys, now)?)
            }
            other => Ok(other?),
        }
    }

    /// Verifies a session token, refreshing JWKS once when needed.
    pub fn verify_session(&self, token: &str, now: i64) -> Result<SessionClaims, AccountError> {
        let keys = self.jwks(false)?;
        match jwt::verify_session_token(token, &keys, now) {
            Err(TokenError::UnknownKid(_)) => {
                let keys = self.jwks(true)?;
                Ok(jwt::verify_session_token(token, &keys, now)?)
            }
            other => Ok(other?),
        }
    }

    /// `GET /api/me`.
    pub fn me(&self, session: &str) -> Result<Me, AccountError> {
        let url = self.endpoint("/api/me")?;
        let response = http::get(
            &url,
            &[
                ("accept", "application/json"),
                ("authorization", &format!("Bearer {session}")),
            ],
        )?;
        let value = expect_ok(&response)?;

        let user = value.get("user").cloned().unwrap_or(Value::Null);
        let profile = Profile {
            sub: user
                .get("discordId")
                .and_then(Value::as_str)
                .map(str::to_owned),
            name: user
                .get("username")
                .and_then(Value::as_str)
                .map(str::to_owned),
            avatar: user
                .get("avatar")
                .and_then(Value::as_str)
                .map(str::to_owned),
        };

        let plan = Plan::parse(value.get("plan").and_then(Value::as_str).unwrap_or("free"));
        let until = value
            .get("until")
            .and_then(Value::as_str)
            .and_then(parse_iso8601);

        Ok(Me {
            profile,
            plan,
            until,
        })
    }

    /// `POST /api/license/refresh` with the device identity.
    pub fn refresh(
        &self,
        session: &str,
        device_id: &str,
        device_name: &str,
    ) -> Result<(String, LicenseClaims), AccountError> {
        let url = self.endpoint("/api/license/refresh")?;
        let body = json!({ "deviceId": device_id, "deviceName": device_name }).to_string();
        let response = http::post_json(
            &url,
            body.as_bytes(),
            &[
                ("accept", "application/json"),
                ("authorization", &format!("Bearer {session}")),
            ],
        )?;
        let value = expect_ok(&response)?;

        let token = value
            .get("token")
            .and_then(Value::as_str)
            .filter(|token| !token.is_empty())
            .ok_or_else(|| AccountError::Json("refresh carries no token".to_owned()))?;

        let now = now_seconds();
        let (claims, _) = self.verify_license(token, now)?;

        Ok((token.to_owned(), claims))
    }
}

/// Reads a response, turning the worker's `{error:{code,message}}` into [`AccountError::Api`].
pub fn expect_ok(response: &http::HttpResponse) -> Result<Value, AccountError> {
    let value: Value = serde_json::from_slice(&response.body)
        .map_err(|error| AccountError::Json(format!("HTTP {}: {error}", response.status)))?;

    if !response.ok() {
        let error = value.get("error").cloned().unwrap_or(Value::Null);
        return Err(AccountError::Api {
            status: response.status,
            code: error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("http_error")
                .to_owned(),
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("the server refused the request")
                .to_owned(),
        });
    }

    Ok(value)
}

/// Current Unix time in seconds, clamped at zero for a clock set before 1970.
pub fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|age| i64::try_from(age.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// RFC 3339 in UTC for a Unix timestamp.
pub fn iso_from_unix(seconds: i64) -> String {
    let instant = std::time::UNIX_EPOCH
        + std::time::Duration::from_secs(u64::try_from(seconds.max(0)).unwrap_or(0));
    UtcDateTime::from_system_time(instant).iso8601()
}

/// Parses the `YYYY-MM-DDTHH:MM:SS(.fff)Z` shape the worker returns.
fn parse_iso8601(text: &str) -> Option<i64> {
    let trimmed = text.trim();
    let date = trimmed.get(0..10)?;
    let time = trimmed.get(11..19)?;
    let mut parts = date.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }

    let mut parts = time.split(':');
    let hour: i64 = parts.next()?.parse().ok()?;
    let minute: i64 = parts.next()?.parse().ok()?;
    let second: i64 = parts.next()?.parse().ok()?;

    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

/// Days since 1970-01-01 for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let shifted_month = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted_month + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// The `account.getState` document.
pub fn state_document(api_base: &str, record: Option<&StoredLicense>, now: i64) -> Value {
    let state = store::evaluate(record, now);
    let profile = record
        .map(|record| record.profile.clone())
        .unwrap_or_default();

    json!({
        "signedIn": state.signed_in,
        "plan": state.plan.as_str(),
        "until": state.until.map(iso_from_unix),
        "name": profile.name,
        "avatar": profile.avatar,
        "stale": state.stale,
        "configured": !api_base.trim().is_empty(),
    })
}

/// The read-only `license.getState` document other modules gate on.
pub fn license_document(record: Option<&StoredLicense>, now: i64) -> Value {
    let state = store::evaluate(record, now);
    let valid = record
        .and_then(|record| record.expires_at)
        .is_some_and(|expiry| expiry > now);

    json!({
        "signedIn": state.signed_in,
        "plan": state.plan.as_str(),
        "until": state.until.map(iso_from_unix),
        "stale": state.stale,
        "valid": valid && state.plan.is_pro(),
    })
}

/// Opens the store next to the config, for callers that already know the data root.
pub fn store_at(root: &Path) -> LicenseStore {
    LicenseStore::new(root.join("license.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unconfigured_base_is_reported_and_refuses_to_build_a_url() {
        let api = AccountApi::new("  ", PathBuf::from("cache.json"));

        assert!(!api.configured());
        assert!(matches!(api.jwks(false), Err(AccountError::NotConfigured)));
    }

    #[test]
    fn the_start_url_carries_the_loopback_as_an_encoded_return() {
        let api = AccountApi::new("https://api.example.invalid/", PathBuf::from("cache.json"));
        let url = api.start_url("http://127.0.0.1:47832/callback?state=abc");

        assert_eq!(
            url,
            "https://api.example.invalid/auth/discord/start?return=http%3A%2F%2F127.0.0.1%3A47832%2Fcallback%3Fstate%3Dabc"
        );
    }

    #[test]
    fn an_error_document_is_read_as_a_typed_refusal() {
        let response = http::HttpResponse {
            status: 403,
            body: br#"{"error":{"code":"upgrade_required","message":"Pro is required"}}"#.to_vec(),
        };

        match expect_ok(&response) {
            Err(AccountError::Api {
                status,
                code,
                message,
            }) => {
                assert_eq!(status, 403);
                assert_eq!(code, "upgrade_required");
                assert_eq!(message, "Pro is required");
            }
            other => panic!("expected an API error, got {other:?}"),
        }
    }

    #[test]
    fn iso_times_round_trip() {
        assert_eq!(iso_from_unix(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_from_unix(1_700_000_000), "2023-11-14T22:13:20.000Z");
        assert_eq!(
            parse_iso8601("2023-11-14T22:13:20.000Z"),
            Some(1_700_000_000)
        );
        assert_eq!(parse_iso8601("not a date"), None);
    }

    #[test]
    fn a_state_document_never_invents_an_account() {
        let document = state_document("https://api.example.invalid", None, 1_000);

        assert_eq!(document["signedIn"], json!(false));
        assert_eq!(document["plan"], json!("free"));
        assert_eq!(document["stale"], json!(false));
        assert_eq!(document["configured"], json!(true));
        assert!(document["until"].is_null());
        assert!(document["name"].is_null());
    }
}
