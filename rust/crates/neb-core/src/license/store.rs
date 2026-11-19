//! The encrypted licence file and the entitlement evaluation on top of it.
//!
//! `license.json` is a JSON envelope around one DPAPI blob. The inner record carries the bearer
//! tokens, the identity the worker returned and the claims the last verified entitlement had. The
//! evaluation never invents a plan: a token past `exp` is only believed for [`GRACE_PERIOD`] after
//! the last successful refresh, and only if that cached plan was `pro`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::dpapi;
use super::jwt::{self, LicenseClaims};

/// How long a cached Pro plan survives an expired entitlement while offline.
pub const GRACE_PERIOD: i64 = 30 * 24 * 60 * 60;

/// Version of the envelope this build writes.
pub const ENVELOPE_VERSION: u32 = 1;

/// A plan the product knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Plan {
    /// No subscription.
    #[default]
    Free,
    /// Active subscription.
    Pro,
}

impl Plan {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Pro => "pro",
        }
    }

    /// Reads a plan name; anything unknown is [`Plan::Free`], never a silent upgrade.
    pub fn parse(value: &str) -> Self {
        if value.eq_ignore_ascii_case("pro") {
            Self::Pro
        } else {
            Self::Free
        }
    }

    /// Whether this is the paid plan.
    pub fn is_pro(self) -> bool {
        matches!(self, Self::Pro)
    }
}

/// The Discord identity the worker described for this account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    /// Discord id (`sub`).
    pub sub: Option<String>,
    /// Display name.
    pub name: Option<String>,
    /// Avatar hash or URL as the worker returned it.
    pub avatar: Option<String>,
}

/// What `license.json` holds after decryption.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StoredLicense {
    /// The entitlement JWT, if the worker issued one.
    pub entitlement: Option<String>,
    /// The session JWT.
    pub session: Option<String>,
    /// Unix seconds of the last successful refresh (or sign-in).
    pub refreshed_at: i64,
    /// Expiry of the entitlement token, Unix seconds.
    pub expires_at: Option<i64>,
    /// Subscription end, Unix seconds.
    pub until: Option<i64>,
    /// Plan the last verified entitlement carried.
    pub plan: Plan,
    /// Identity from `/api/me`.
    pub profile: Profile,
}

impl StoredLicense {
    /// Applies a freshly verified entitlement to the record.
    pub fn apply_entitlement(&mut self, token: &str, claims: &LicenseClaims, now: i64) {
        self.entitlement = Some(token.to_owned());
        self.refreshed_at = now;
        self.expires_at = Some(claims.exp);
        self.until = Some(claims.until.unwrap_or(claims.exp));
        self.plan = Plan::parse(&claims.plan);
    }

    /// Applies a successful sign-in: session, identity and (optionally) entitlement.
    pub fn apply_sign_in(&mut self, session: &str, profile: Profile, now: i64) {
        self.session = Some(session.to_owned());
        self.profile = profile;
        self.refreshed_at = now;
    }

    /// Clears the entitlement but keeps the session, for a signed-in free account.
    pub fn clear_entitlement(&mut self) {
        self.entitlement = None;
        self.expires_at = None;
        self.until = None;
        self.plan = Plan::Free;
    }

    /// Whether the stored session can still be used.
    pub fn signed_in(&self) -> bool {
        self.session
            .as_deref()
            .is_some_and(|token| !token.is_empty())
    }
}

/// What the UI and other modules read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntitlementState {
    /// Effective plan.
    pub plan: Plan,
    /// Subscription end, Unix seconds.
    pub until: Option<i64>,
    /// True when the plan comes from an expired token inside the offline grace window.
    pub stale: bool,
    /// True when a session token is stored.
    pub signed_in: bool,
}

/// Evaluates a stored record at `now` (Unix seconds).
///
/// * A token that has not expired backs its plan exactly, like the server sees it.
/// * An expired token keeps its plan only for [`GRACE_PERIOD`] after `refreshed_at`, and only when
///   that plan was Pro; the result is marked stale.
/// * Anything else is Free. An expired token therefore never claims Pro forever.
pub fn evaluate(record: Option<&StoredLicense>, now: i64) -> EntitlementState {
    let Some(record) = record else {
        return EntitlementState {
            plan: Plan::Free,
            until: None,
            stale: false,
            signed_in: false,
        };
    };

    let signed_in = record.signed_in();
    let fresh = record.expires_at.is_some_and(|expiry| expiry > now);

    if fresh {
        return EntitlementState {
            plan: record.plan,
            until: record.until,
            stale: false,
            signed_in,
        };
    }

    let age = now.saturating_sub(record.refreshed_at);
    // A clock that jumped backwards must not turn a stale cache into a fresh one; a clock that is
    // simply behind the worker's is tolerated by the iat skew check, so age is floored at zero.
    let within_grace = (0..=GRACE_PERIOD).contains(&age);
    if record.plan.is_pro() && within_grace {
        return EntitlementState {
            plan: Plan::Pro,
            until: record.until,
            stale: true,
            signed_in,
        };
    }

    EntitlementState {
        plan: Plan::Free,
        until: None,
        stale: false,
        signed_in,
    }
}

/// What went wrong while reading or writing `license.json`.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The file system refused the operation.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation.
        action: &'static str,
        /// The path involved.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
    /// The envelope or the decrypted record is not JSON in the expected shape.
    #[error("'{}' is not a valid licence file: {message}", path.display())]
    Invalid {
        /// The file.
        path: PathBuf,
        /// What is wrong.
        message: String,
    },
    /// DPAPI refused the blob, usually because another user or machine wrote it.
    #[error("cannot {action} the licence blob: {source}")]
    Dpapi {
        /// `protect` or `unprotect`.
        action: &'static str,
        /// The DPAPI failure.
        #[source]
        source: dpapi::DpapiError,
    },
}

/// JSON envelope written to disk.
#[derive(Debug, Serialize, Deserialize)]
struct Envelope {
    v: u32,
    blob: String,
}

/// The DPAPI-encrypted `license.json`.
#[derive(Debug, Clone)]
pub struct LicenseStore {
    path: PathBuf,
}

impl LicenseStore {
    /// A store for an explicit path, used by tests and portable runs.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// The store for `%LocalAppData%\ProjectNeb\license.json`.
    pub fn at_default_location() -> Self {
        Self::new(crate::paths::license_file())
    }

    /// The file this store owns.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads and decrypts the record; a missing file is `Ok(None)`.
    pub fn load(&self) -> Result<Option<StoredLicense>, StoreError> {
        let bytes = match fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(source) => {
                return Err(StoreError::Io {
                    action: "read",
                    path: self.path.clone(),
                    source,
                });
            }
        };

        let envelope: Envelope =
            serde_json::from_slice(&bytes).map_err(|error| StoreError::Invalid {
                path: self.path.clone(),
                message: error.to_string(),
            })?;

        if envelope.v != ENVELOPE_VERSION {
            return Err(StoreError::Invalid {
                path: self.path.clone(),
                message: format!("unknown envelope version {}", envelope.v),
            });
        }

        let blob = jwt::base64url_decode(&envelope.blob).map_err(|_| StoreError::Invalid {
            path: self.path.clone(),
            message: "the blob is not base64url".to_owned(),
        })?;

        let plaintext = dpapi::unprotect(&blob).map_err(|source| StoreError::Dpapi {
            action: "unprotect",
            source,
        })?;

        let record: StoredLicense =
            serde_json::from_slice(&plaintext).map_err(|error| StoreError::Invalid {
                path: self.path.clone(),
                message: error.to_string(),
            })?;

        Ok(Some(record))
    }

    /// Encrypts `record` and writes it atomically.
    pub fn save(&self, record: &StoredLicense) -> Result<(), StoreError> {
        if let Some(parent) = self.path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| StoreError::Io {
                action: "create",
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let plaintext = serde_json::to_vec(record).map_err(|error| StoreError::Invalid {
            path: self.path.clone(),
            message: error.to_string(),
        })?;

        let protected = dpapi::protect(&plaintext).map_err(|source| StoreError::Dpapi {
            action: "protect",
            source,
        })?;

        let envelope = Envelope {
            v: ENVELOPE_VERSION,
            blob: jwt::base64url_encode(&protected),
        };
        let mut bytes =
            serde_json::to_vec_pretty(&envelope).map_err(|error| StoreError::Invalid {
                path: self.path.clone(),
                message: error.to_string(),
            })?;
        bytes.push(b'\n');

        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, &bytes).map_err(|source| StoreError::Io {
            action: "write",
            path: tmp.clone(),
            source,
        })?;

        if let Err(source) = fs::rename(&tmp, &self.path) {
            let _ = fs::remove_file(&tmp);
            return Err(StoreError::Io {
                action: "replace",
                path: self.path.clone(),
                source,
            });
        }

        Ok(())
    }

    /// Removes the stored tokens. A missing file is already signed out.
    pub fn wipe(&self) -> Result<(), StoreError> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io {
                action: "remove",
                path: self.path.clone(),
                source,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn claims(plan: &str, exp: i64, until: i64) -> LicenseClaims {
        LicenseClaims {
            iss: Some("projectneb".to_owned()),
            aud: Some("projectneb-client".to_owned()),
            sub: "42".to_owned(),
            plan: plan.to_owned(),
            until: Some(until),
            devices: vec!["device-a".to_owned()],
            kind: Some("license".to_owned()),
            iat: 1_000,
            exp,
        }
    }

    fn record(plan: &str, exp: i64, refreshed_at: i64) -> StoredLicense {
        let mut record = StoredLicense {
            session: Some("session.token".to_owned()),
            profile: Profile {
                sub: Some("42".to_owned()),
                name: Some("Tester".to_owned()),
                avatar: Some("avatar".to_owned()),
            },
            ..StoredLicense::default()
        };
        record.apply_entitlement("license.token", &claims(plan, exp, 2_000), refreshed_at);
        record
    }

    #[test]
    fn a_fresh_token_backs_its_plan() {
        let record = record("pro", 2_000, 1_000);
        let state = evaluate(Some(&record), 1_500);

        assert_eq!(state.plan, Plan::Pro);
        assert!(!state.stale);
        assert_eq!(state.until, Some(2_000));
        assert!(state.signed_in);
    }

    #[test]
    fn an_expired_token_is_pro_for_thirty_days_then_free() {
        let record = record("pro", 1_000, 1_000);

        let inside = evaluate(Some(&record), 1_000 + GRACE_PERIOD - 1);
        assert_eq!(inside.plan, Plan::Pro);
        assert!(inside.stale, "the plan is not backed by a live token");

        let after = evaluate(Some(&record), 1_000 + GRACE_PERIOD + 1);
        assert_eq!(after.plan, Plan::Free);
        assert!(!after.stale);
        assert_eq!(after.until, None, "an expired entitlement has no end date");
    }

    #[test]
    fn a_cached_free_plan_never_becomes_pro() {
        let record = record("free", 1_000, 1_000);

        let state = evaluate(Some(&record), 1_100);
        assert_eq!(state.plan, Plan::Free);
        assert!(!state.stale);
    }

    #[test]
    fn no_record_is_signed_out_and_free() {
        let state = evaluate(None, 5_000);

        assert_eq!(state.plan, Plan::Free);
        assert!(!state.signed_in);
        assert!(!state.stale);
        assert_eq!(state.until, None);
    }

    #[test]
    fn a_record_round_trips_encrypted_and_never_writes_the_token() {
        let dir = tempdir().expect("temp dir");
        let store = LicenseStore::new(dir.path().join("license.json"));
        let record = record("pro", 2_000, 1_500);

        store.save(&record).expect("save");
        let text = fs::read_to_string(store.path()).expect("read");

        assert!(
            !text.contains("license.token") && !text.contains("session.token"),
            "raw JWTs must not sit in plaintext: {text}"
        );
        assert!(text.contains("\"v\": 1"), "{text}");

        let loaded = store.load().expect("load").expect("a record");
        assert_eq!(loaded, record);
    }

    #[test]
    fn loading_a_missing_file_is_none_and_wipe_is_idempotent() {
        let dir = tempdir().expect("temp dir");
        let store = LicenseStore::new(dir.path().join("license.json"));

        assert_eq!(store.load().expect("load"), None);
        store.wipe().expect("wipe a missing file");
    }

    #[test]
    fn wiping_removes_the_file() {
        let dir = tempdir().expect("temp dir");
        let store = LicenseStore::new(dir.path().join("licence.json"));
        store.save(&record("pro", 2_000, 1_500)).expect("save");

        store.wipe().expect("wipe");

        assert!(!store.path().exists());
        assert_eq!(store.load().expect("load"), None);
    }

    #[test]
    fn a_broken_envelope_is_an_error_not_a_silent_sign_out() {
        let dir = tempdir().expect("temp dir");
        let store = LicenseStore::new(dir.path().join("license.json"));
        fs::write(store.path(), "{not json").expect("write");

        assert!(matches!(store.load(), Err(StoreError::Invalid { .. })));
    }

    #[test]
    fn a_plan_name_that_is_not_pro_is_free() {
        assert_eq!(Plan::parse("pro"), Plan::Pro);
        assert_eq!(Plan::parse("PRO"), Plan::Pro);
        assert_eq!(Plan::parse("enterprise"), Plan::Free);
        assert_eq!(Plan::parse(""), Plan::Free);
    }
}
