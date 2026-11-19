//! Over-the-air updates for ProjectNeb.
//!
//! The supervisor does not patch itself. It checks a small JSON manifest over WinHTTP, compares the
//! version it was built with, and — when the user allows it — downloads the portable archive, verifies
//! its SHA-256 and hands the staging directory to `neb-updater.exe`, which waits for this process to
//! exit and then copies the files over the application directory.
//!
//! Everything here is synchronous. The network runs on one worker thread; the tray, the dialogs and
//! the process loop stay on the thread that owns the windows. No TLS crate is involved: WinHTTP is the
//! TLS stack, which is the same one Windows Update and every other resident program uses.

use std::ffi::c_void;
use std::fmt;
use std::fs::{self, File};
use std::io::{self, Write};
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use windows::Win32::Foundation::GetLastError;
use windows::Win32::Networking::WinHttp::{
    URL_COMPONENTS, WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY, WINHTTP_ACCESS_TYPE_NO_PROXY,
    WINHTTP_FLAG_SECURE, WINHTTP_INTERNET_SCHEME_HTTP, WINHTTP_INTERNET_SCHEME_HTTPS,
    WINHTTP_OPEN_REQUEST_FLAGS, WINHTTP_QUERY_FLAG_NUMBER, WINHTTP_QUERY_STATUS_CODE,
    WinHttpCloseHandle, WinHttpConnect, WinHttpCrackUrl, WinHttpOpen, WinHttpOpenRequest,
    WinHttpQueryHeaders, WinHttpReadData, WinHttpReceiveResponse, WinHttpSendRequest,
    WinHttpSetTimeouts,
};
use windows::core::PCWSTR;
use zip::ZipArchive;

use crate::supervisor::Reporter;

/// Where the live manifest is published.
pub const DEFAULT_MANIFEST_URL: &str =
    "https://raw.githubusercontent.com/temajmix/ProjectNeb/main/latest.json";

/// The changelog page the tray opens when nothing more specific is known.
pub const DEFAULT_CHANGELOG_URL: &str =
    "https://github.com/temajmix/ProjectNeb/blob/main/CHANGELOG.md";

/// Default hours between two automatic checks.
pub const DEFAULT_CHECK_HOURS: u64 = 6;

/// Shortest interval the settings allow.
pub const MIN_CHECK_HOURS: u64 = 1;

/// Longest interval the settings allow.
pub const MAX_CHECK_HOURS: u64 = 168;

/// The first check waits this long, so the bar and the tray are up before any network traffic.
pub const FIRST_CHECK_DELAY: Duration = Duration::from_secs(10);

/// Budget for one request; a dead network must not keep the worker for minutes.
const HTTP_TIMEOUT: Duration = Duration::from_secs(20);

/// A manifest larger than this is not a manifest.
const MANIFEST_LIMIT: u64 = 1024 * 1024;

/// Directory under the ProjectNeb data root that holds every update artefact.
pub const UPDATES_DIR: &str = "updates";

/// The notes of the last downloaded version.
pub const WHATS_NEW_FILE: &str = "whats-new.json";

/// The portable archive inside `<updates>/<version>/`.
pub const PORTABLE_ZIP: &str = "portable.zip";

/// Extracted files inside `<updates>/<version>/`.
pub const STAGING_DIR: &str = "staging";

/// Marker left when an update was downloaded but not applied.
pub const PENDING_FILE: &str = "pending.json";

/// The hand-over program, looked up next to `ProjectNeb.exe`.
pub const UPDATER_EXE: &str = "neb-updater.exe";

/// How long `neb-updater.exe` may wait for the supervisor to exit.
pub const SPAWN_TIMEOUT_SECS: u64 = 60;

/// Top-level folder the portable archive wraps its files in.
const ARCHIVE_ROOT: &str = "ProjectNeb";

/// A portable archive larger than this is not something this build will unpack.
const DOWNLOAD_LIMIT: u64 = 512 * 1024 * 1024;

/// Budget for the portable download, which is much larger than the manifest.
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

/// Where the tray reads notes from, and where an update writes them once it is downloaded.
pub fn updates_dir(root: &Path) -> PathBuf {
    root.join(UPDATES_DIR)
}

/// The notes file under `root`.
pub fn whats_new_file(root: &Path) -> PathBuf {
    updates_dir(root).join(WHATS_NEW_FILE)
}

/// Every path one downloaded version owns, under `%LocalAppData%\ProjectNeb\updates\<version>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdatePaths {
    /// `updates\<version>`.
    pub version_dir: PathBuf,
    /// `<version_dir>\portable.zip`.
    pub portable_zip: PathBuf,
    /// `<version_dir>\staging`, the directory handed to `neb-updater.exe`.
    pub staging: PathBuf,
}

impl UpdatePaths {
    /// The layout for `version`.
    pub fn for_version(root: &Path, version: &Version) -> Self {
        let version_dir = updates_dir(root).join(path_component(version));
        Self {
            portable_zip: version_dir.join(PORTABLE_ZIP),
            staging: version_dir.join(STAGING_DIR),
            version_dir,
        }
    }
}

/// A version is text from a document, so it must not be allowed to invent path separators.
fn path_component(version: &Version) -> String {
    version
        .to_string()
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

/// The document written when an update waits for the user.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Pending {
    /// Version that is staged.
    pub version: String,
    /// The staging directory the updater will copy.
    pub staging: String,
    /// The archive the staging directory came from.
    pub portable: String,
    /// Notes, repeated here so the tray can answer without a manifest.
    pub notes: String,
    /// Changelog of the pending version.
    pub changelog_url: String,
}

/// Reads a pending marker; a missing or broken file means "nothing pending".
pub fn read_pending(root: &Path) -> Option<Pending> {
    let bytes = fs::read(updates_dir(root).join(PENDING_FILE)).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    serde_json::from_str(text).ok()
}

/// Writes the pending marker for `version`.
fn write_pending(root: &Path, pending: &Pending) -> Result<PathBuf, UpdateError> {
    let path = updates_dir(root).join(PENDING_FILE);
    write_json(&path, pending)?;

    Ok(path)
}

/// Writes a JSON document through a temporary file, so a crash cannot leave half a marker.
fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), UpdateError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let temporary = path.with_extension("tmp");
    let text = serde_json::to_string_pretty(value)?;
    fs::write(&temporary, text).map_err(|source| UpdateError::Io {
        action: "write",
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, path).map_err(|source| UpdateError::Io {
        action: "replace",
        path: path.to_path_buf(),
        source,
    })
}

/// A callback the worker uses to tell the user something without owning a window.
pub type Notify = Arc<dyn Fn(&str, &str) + Send + Sync>;

/// A callback that asks the resident loop to stop after an update was handed over.
pub type StopFn = Arc<dyn Fn() + Send + Sync>;

/// `updates` section of `config.json`, read straight from the JSON so the C# build ignores it and a
/// missing key means "the shipped default", not "whatever the struct happens to zero to".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    /// Whether any check runs at all.
    pub enabled: bool,
    /// Manifest URL.
    pub manifest_url: String,
    /// Hours between checks, clamped to [`MIN_CHECK_HOURS`]..=[`MAX_CHECK_HOURS`].
    pub check_hours: u64,
    /// Whether a verified update is applied without asking.
    pub auto_apply: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            manifest_url: DEFAULT_MANIFEST_URL.to_owned(),
            check_hours: DEFAULT_CHECK_HOURS,
            auto_apply: true,
        }
    }
}

impl Settings {
    /// Reads the `updates` object out of an already parsed `config.json`.
    ///
    /// The performance profile supplies the default interval, `performance.updateCheckHours`
    /// overrides it, and the older explicit `updates.checkHours` still wins so an installation that
    /// already chose an interval keeps it.
    pub fn from_json(config: &Value) -> Self {
        let mut settings = Self::default();

        if let Some(performance) = config.get("performance")
            && let Ok(performance) =
                serde_json::from_value::<neb_core::config::PerformanceSettings>(performance.clone())
        {
            settings.check_hours = performance.resolved().update_check_hours;
        }

        let Some(section) = config.get("updates") else {
            return settings;
        };

        if let Some(enabled) = section.get("enabled").and_then(Value::as_bool) {
            settings.enabled = enabled;
        }

        if let Some(url) = section.get("manifestUrl").and_then(Value::as_str) {
            let url = url.trim();
            if !url.is_empty() {
                settings.manifest_url = url.to_owned();
            }
        }

        if let Some(hours) = section.get("checkHours").and_then(Value::as_i64) {
            settings.check_hours =
                hours.clamp(MIN_CHECK_HOURS as i64, MAX_CHECK_HOURS as i64) as u64;
        }

        if let Some(auto_apply) = section.get("autoApply").and_then(Value::as_bool) {
            settings.auto_apply = auto_apply;
        }

        settings
    }

    /// Reads the settings out of `config.json`; a missing or broken file is the default profile.
    pub fn from_file(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };

        Self::from_text(&String::from_utf8_lossy(&bytes))
    }

    /// Parses the text of `config.json`, tolerating the BOM the C# build writes.
    pub fn from_text(text: &str) -> Self {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);

        match serde_json::from_str::<Value>(text) {
            Ok(value) => Self::from_json(&value),
            Err(_) => Self::default(),
        }
    }

    /// The pause between two checks.
    pub fn interval(&self) -> Duration {
        Duration::from_secs(self.check_hours * 60 * 60)
    }
}

/// A parsed semantic version. Build metadata is accepted and ignored; pre-release identifiers order
/// the way the SemVer specification says, which matters because beta builds are published from the
/// same repository as the stable ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Major number.
    pub major: u64,
    /// Minor number.
    pub minor: u64,
    /// Patch number.
    pub patch: u64,
    pre: Vec<Prerelease>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Prerelease {
    Numeric(u64),
    Text(String),
}

impl Version {
    /// Parses `MAJOR.MINOR.PATCH[-PRE][+BUILD]`.
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let text = text.trim();
        let without_build = text.split('+').next().unwrap_or(text);
        let (numbers, pre) = match without_build.split_once('-') {
            Some((numbers, pre)) => (numbers, Some(pre)),
            None => (without_build, None),
        };

        let mut pieces = numbers.split('.');
        let major = number(pieces.next(), text)?;
        let minor = number(pieces.next(), text)?;
        let patch = number(pieces.next(), text)?;

        if pieces.next().is_some() {
            return Err(UpdateError::Version(text.to_owned()));
        }

        let pre = match pre {
            None => Vec::new(),
            Some("") => return Err(UpdateError::Version(text.to_owned())),
            Some(pre) => pre
                .split('.')
                .map(|identifier| {
                    if identifier.is_empty() {
                        return Err(UpdateError::Version(text.to_owned()));
                    }

                    Ok(match identifier.parse::<u64>() {
                        Ok(number) => Prerelease::Numeric(number),
                        Err(_) => Prerelease::Text(identifier.to_owned()),
                    })
                })
                .collect::<Result<Vec<_>, _>>()?,
        };

        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }

    /// Whether this version is a pre-release.
    #[cfg(test)]
    pub fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }
}

fn number(piece: Option<&str>, whole: &str) -> Result<u64, UpdateError> {
    piece
        .filter(|piece| !piece.is_empty())
        .and_then(|piece| piece.parse::<u64>().ok())
        .ok_or_else(|| UpdateError::Version(whole.to_owned()))
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;

        if self.pre.is_empty() {
            return Ok(());
        }

        formatter.write_str("-")?;
        for (index, identifier) in self.pre.iter().enumerate() {
            if index > 0 {
                formatter.write_str(".")?;
            }
            match identifier {
                Prerelease::Numeric(number) => write!(formatter, "{number}")?,
                Prerelease::Text(text) => formatter.write_str(text)?,
            }
        }

        Ok(())
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.major
            .cmp(&other.major)
            .then_with(|| self.minor.cmp(&other.minor))
            .then_with(|| self.patch.cmp(&other.patch))
            .then_with(|| compare_prerelease(&self.pre, &other.pre))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A release sorts after any of its pre-releases, which is the one rule people trip over.
fn compare_prerelease(left: &[Prerelease], right: &[Prerelease]) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }

    for (a, b) in left.iter().zip(right) {
        let ordering = match (a, b) {
            (Prerelease::Numeric(a), Prerelease::Numeric(b)) => a.cmp(b),
            (Prerelease::Numeric(_), Prerelease::Text(_)) => Ordering::Less,
            (Prerelease::Text(_), Prerelease::Numeric(_)) => Ordering::Greater,
            (Prerelease::Text(a), Prerelease::Text(b)) => a.cmp(b),
        };

        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    left.len().cmp(&right.len())
}

/// One file offered by the manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// Download URL.
    pub url: String,
    /// Lower-case or upper-case hex SHA-256 of the whole file.
    pub sha256: String,
}

/// The update manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Version the release advertises.
    pub version: Version,
    /// Channel the release was published on; `beta` or `stable`.
    pub channel: String,
    /// Release notes, shown after the update.
    pub notes: String,
    /// Where the full changelog lives.
    pub changelog_url: String,
    /// The portable archive, when the release has one.
    pub portable: Option<Asset>,
}

impl Manifest {
    /// Parses the manifest document.
    ///
    /// `notes`, `changelogUrl` and every asset are optional: a release that forgot them should still
    /// install. `version` is not, and neither is a channel this build understands.
    ///
    /// # Errors
    ///
    /// [`UpdateError::Json`] for malformed JSON, [`UpdateError::NoVersion`] when the key is absent,
    /// [`UpdateError::Version`] for a version that is not SemVer, [`UpdateError::Channel`] for a
    /// channel other than `beta` or `stable`.
    pub fn parse(text: &str) -> Result<Self, UpdateError> {
        let value: Value = serde_json::from_str(text)?;

        let version_text = value
            .get("version")
            .and_then(Value::as_str)
            .ok_or(UpdateError::NoVersion)?;
        let version = Version::parse(version_text)?;

        let channel = value
            .get("channel")
            .and_then(Value::as_str)
            .unwrap_or("beta")
            .trim()
            .to_ascii_lowercase();
        if channel != "beta" && channel != "stable" {
            return Err(UpdateError::Channel(channel));
        }

        let notes = value
            .get("notes")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let changelog_url = value
            .get("changelogUrl")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or(DEFAULT_CHANGELOG_URL)
            .to_owned();

        let portable = value
            .pointer("/assets/portable")
            .and_then(asset_from)
            .filter(|asset| !asset.url.is_empty() && !asset.sha256.is_empty());

        Ok(Self {
            version,
            channel,
            notes,
            changelog_url,
            portable,
        })
    }
}

fn asset_from(value: &Value) -> Option<Asset> {
    let url = value.get("url").and_then(Value::as_str)?.trim();
    let sha256 = value.get("sha256").and_then(Value::as_str)?.trim();

    Some(Asset {
        url: url.to_owned(),
        sha256: sha256.to_owned(),
    })
}

/// The notes of a version that was downloaded, written before the update is applied so the next
/// start can say what changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WhatsNew {
    /// Version the notes belong to.
    pub version: String,
    /// Release notes.
    pub notes: String,
    /// Full changelog.
    pub changelog_url: String,
    /// Whether the dialog was already shown.
    pub shown: bool,
}

/// Reads `updates/whats-new.json`; a missing or broken file simply has no notes.
pub fn read_whats_new(root: &Path) -> Option<WhatsNew> {
    let bytes = std::fs::read(whats_new_file(root)).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    serde_json::from_str(text).ok()
}

/// Writes the notes for `version`, replacing an older file.
///
/// # Errors
///
/// [`UpdateError::Io`] when the directory or the file cannot be written.
pub fn write_whats_new(
    root: &Path,
    version: &Version,
    notes: &str,
    changelog_url: &str,
) -> Result<PathBuf, UpdateError> {
    let path = whats_new_file(root);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let notes = WhatsNew {
        version: version.to_string(),
        notes: notes.to_owned(),
        changelog_url: changelog_url.to_owned(),
        shown: false,
    };
    let text = serde_json::to_string_pretty(&notes)?;
    std::fs::write(&path, text).map_err(|source| UpdateError::Io {
        action: "write",
        path: path.clone(),
        source,
    })?;

    Ok(path)
}

/// Marks the current notes as shown.
///
/// # Errors
///
/// [`UpdateError::Io`] when the file exists but cannot be rewritten.
pub fn mark_shown(root: &Path) -> Result<(), UpdateError> {
    let Some(mut notes) = read_whats_new(root) else {
        return Ok(());
    };

    notes.shown = true;
    let path = whats_new_file(root);
    let text = serde_json::to_string_pretty(&notes)?;
    std::fs::write(&path, text).map_err(|source| UpdateError::Io {
        action: "write",
        path,
        source,
    })
}

/// Downloads `url` to `destination`, hashing as it writes, and refuses a mismatch.
///
/// The destination is removed when anything fails, so a half-downloaded archive is never mistaken for
/// a verified one.
///
/// # Errors
///
/// [`UpdateError::Checksum`] when the SHA-256 differs, [`UpdateError::Http`],
/// [`UpdateError::Status`], [`UpdateError::TooLarge`] or [`UpdateError::Io`] for the transfer.
pub fn download(
    url: &str,
    expected_sha256: &str,
    destination: &Path,
    timeout: Duration,
) -> Result<u64, UpdateError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut file = File::create(destination).map_err(|source| UpdateError::Io {
        action: "create",
        path: destination.to_path_buf(),
        source,
    })?;
    let mut hasher = Sha256::new();

    let transferred = http_get(url, timeout, DOWNLOAD_LIMIT, &mut |chunk| {
        file.write_all(chunk).map_err(|source| UpdateError::Io {
            action: "write",
            path: destination.to_path_buf(),
            source,
        })?;
        hasher.update(chunk);

        Ok(())
    });

    let transferred = match transferred {
        Ok(total) => total,
        Err(error) => {
            drop(file);
            let _ = fs::remove_file(destination);
            return Err(error);
        }
    };

    drop(file);
    let actual = hex(hasher.finalize());

    if !actual.eq_ignore_ascii_case(expected_sha256.trim()) {
        let _ = fs::remove_file(destination);
        return Err(UpdateError::Checksum {
            expected: expected_sha256.to_owned(),
            actual,
        });
    }

    Ok(transferred)
}

/// Lower-case hex, the spelling the manifest uses.
fn hex(bytes: impl AsRef<[u8]>) -> String {
    let bytes = bytes.as_ref();
    let mut text = String::with_capacity(bytes.len() * 2);

    for byte in bytes {
        const DIGITS: &[u8; 16] = b"0123456789abcdef";
        text.push(DIGITS[(byte >> 4) as usize] as char);
        text.push(DIGITS[(byte & 0x0f) as usize] as char);
    }

    text
}

/// The path of one archive entry inside `staging`, with the portable archive's top-level folder
/// removed and every traversal rejected.
fn relative_entry(name: &str) -> Result<Option<String>, UpdateError> {
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(UpdateError::UnsafeEntry(name.to_owned()));
    }

    let mut components = Vec::new();
    for component in normalized.split('/') {
        if component.is_empty() || component == "." {
            continue;
        }

        if component == ".." || component.contains(':') {
            return Err(UpdateError::UnsafeEntry(name.to_owned()));
        }

        components.push(component);
    }

    if components
        .first()
        .is_some_and(|first| first.eq_ignore_ascii_case(ARCHIVE_ROOT))
    {
        components.remove(0);
    }

    if components.is_empty() {
        Ok(None)
    } else {
        Ok(Some(components.join("/")))
    }
}

/// Extracts the portable archive into `staging`, stripping the `ProjectNeb/` wrapper.
///
/// Every entry is checked before anything is written, so an archive with one unsafe name stages
/// nothing at all.
///
/// # Errors
///
/// [`UpdateError::UnsafeEntry`] for `..`, absolute or colon-spelled entries,
/// [`UpdateError::Zip`] for an archive that cannot be decoded, [`UpdateError::Io`] for the file
/// system.
pub fn stage_zip(zip_path: &Path, staging: &Path) -> Result<Vec<String>, UpdateError> {
    let file = File::open(zip_path).map_err(|source| UpdateError::Io {
        action: "open",
        path: zip_path.to_path_buf(),
        source,
    })?;
    let mut archive = ZipArchive::new(file).map_err(|error| UpdateError::Zip(error.to_string()))?;

    let mut planned = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index(index)
            .map_err(|error| UpdateError::Zip(error.to_string()))?;

        if entry.is_dir() {
            continue;
        }

        if let Some(relative) = relative_entry(entry.name())? {
            planned.push((index, relative));
        }
    }

    if staging.exists() {
        fs::remove_dir_all(staging).map_err(|source| UpdateError::Io {
            action: "remove",
            path: staging.to_path_buf(),
            source,
        })?;
    }
    fs::create_dir_all(staging).map_err(|source| UpdateError::Io {
        action: "create",
        path: staging.to_path_buf(),
        source,
    })?;

    for (index, relative) in &planned {
        let mut entry = archive
            .by_index(*index)
            .map_err(|error| UpdateError::Zip(error.to_string()))?;
        let destination = staging.join(relative);

        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).map_err(|source| UpdateError::Io {
                action: "create",
                path: parent.to_path_buf(),
                source,
            })?;
        }

        let mut output = File::create(&destination).map_err(|source| UpdateError::Io {
            action: "create",
            path: destination.clone(),
            source,
        })?;
        io::copy(&mut entry, &mut output).map_err(|source| UpdateError::Io {
            action: "write",
            path: destination,
            source,
        })?;
    }

    Ok(planned.into_iter().map(|(_, name)| name).collect())
}

/// Whether `staging` already has files from a previous check.
fn staging_ready(staging: &Path) -> bool {
    fs::read_dir(staging)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false)
}

/// Everything `neb-updater.exe` is told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPlan {
    /// The hand-over program.
    pub updater: PathBuf,
    /// Files to copy.
    pub staging: PathBuf,
    /// Directory the files are copied into.
    pub app: PathBuf,
    /// Executable to start after the copy.
    pub restart: PathBuf,
    /// Where the updater writes its log.
    pub log: PathBuf,
}

impl ApplyPlan {
    /// The plan for `version`, given the data root and the application directory.
    pub fn new(root: &Path, app: &Path, version: &Version) -> Self {
        let paths = UpdatePaths::for_version(root, version);

        Self {
            updater: app.join(UPDATER_EXE),
            staging: paths.staging,
            app: app.to_path_buf(),
            restart: std::env::current_exe().unwrap_or_else(|_| app.join("ProjectNeb.exe")),
            log: updates_dir(root).join("updater.log"),
        }
    }
}

/// The exact command line the other agent's updater documents.
fn updater_arguments(pid: u32, plan: &ApplyPlan) -> Vec<String> {
    vec![
        "--wait-pid".to_owned(),
        pid.to_string(),
        "--staging".to_owned(),
        plan.staging.display().to_string(),
        "--app".to_owned(),
        plan.app.display().to_string(),
        "--restart".to_owned(),
        plan.restart.display().to_string(),
        "--timeout-secs".to_owned(),
        SPAWN_TIMEOUT_SECS.to_string(),
        "--log".to_owned(),
        plan.log.display().to_string(),
    ]
}

/// Starts `neb-updater.exe` and returns its pid.
///
/// # Errors
///
/// [`UpdateError::UpdaterMissing`] when the program is not next to the supervisor;
/// [`UpdateError::Process`] when Windows refuses to start it.
pub fn spawn_updater(pid: u32, plan: &ApplyPlan) -> Result<u32, UpdateError> {
    if !plan.updater.is_file() {
        return Err(UpdateError::UpdaterMissing {
            path: plan.updater.clone(),
        });
    }

    if let Some(parent) = plan.log.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let child = Command::new(&plan.updater)
        .args(updater_arguments(pid, plan))
        .spawn()
        .map_err(|source| UpdateError::Process {
            path: plan.updater.clone(),
            source,
        })?;

    Ok(child.id())
}
/// Why an update could not be checked, downloaded or applied.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The URL is not an absolute HTTP or HTTPS URL WinHTTP can crack.
    #[error("invalid update url '{0}'")]
    Url(String),

    /// WinHTTP refused an operation.
    #[error("http {action} '{url}' failed (0x{code:08X})")]
    Http {
        /// Verbed operation, e.g. `open` or `read`.
        action: &'static str,
        /// URL that was attempted.
        url: String,
        /// Win32 error code.
        code: u32,
    },

    /// The server answered, but not with a body this build accepts.
    #[error("http '{url}' answered status {status}")]
    Status {
        /// URL that was attempted.
        url: String,
        /// HTTP status code.
        status: u32,
    },

    /// A response was bigger than the caller allows.
    #[error("'{url}' is larger than {limit} bytes")]
    TooLarge {
        /// URL that was attempted.
        url: String,
        /// Limit in bytes.
        limit: u64,
    },

    /// The manifest is not UTF-8.
    #[error("the manifest is not utf-8: {0}")]
    Utf8(#[from] std::str::Utf8Error),

    /// The manifest is not JSON.
    #[error("the manifest is not valid json: {0}")]
    Json(#[from] serde_json::Error),

    /// The manifest has no version key.
    #[error("the manifest has no version")]
    NoVersion,

    /// The version is not SemVer.
    #[error("'{0}' is not a semantic version")]
    Version(String),

    /// The release is on a channel this build does not install from.
    #[error("the manifest channel '{0}' is not beta or stable")]
    Channel(String),

    /// The downloaded bytes are not the file the manifest describes.
    #[error("checksum mismatch: expected {expected}, got {actual}")]
    Checksum {
        /// The hash from the manifest.
        expected: String,
        /// The hash of what arrived.
        actual: String,
    },

    /// An archive entry points outside the staging directory.
    #[error("unsafe archive entry '{0}'")]
    UnsafeEntry(String),

    /// The archive could not be opened or read.
    #[error("zip archive: {0}")]
    Zip(String),

    /// The hand-over program is not installed next to the supervisor.
    #[error("'{}' is missing, the update was not applied", path.display())]
    UpdaterMissing {
        /// The program that was expected.
        path: PathBuf,
    },

    /// The hand-over program could not be started.
    #[error("cannot start '{}': {source}", path.display())]
    Process {
        /// Program that failed.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// A file operation failed.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `write`.
        action: &'static str,
        /// File or directory the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}

/// A response body delivered to a sink, one chunk at a time.
type Sink<'a> = &'a mut dyn FnMut(&[u8]) -> Result<(), UpdateError>;

/// One WinHTTP session/handle, closed exactly once.
struct Internet(*mut c_void);

impl Drop for Internet {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from a WinHTTP open call and is closed exactly once here.
            unsafe {
                let _ = WinHttpCloseHandle(self.0);
            }
        }
    }
}

/// The parts of a URL WinHTTP needs.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Endpoint {
    host: String,
    path: String,
    port: u16,
    secure: bool,
}

impl Endpoint {
    /// Whether the request goes to this machine, which must bypass a configured proxy: a proxy
    /// usually cannot reach the test server that a local run points the manifest at.
    fn is_local(&self) -> bool {
        let host = self.host.to_ascii_lowercase();
        host == "localhost"
            || host == "::1"
            || host == "[::1]"
            || host == "127.0.0.1"
            || host.starts_with("127.")
    }
}

/// Splits an absolute URL into the pieces `WinHttpConnect` and `WinHttpOpenRequest` want.
fn endpoint(url: &str) -> Result<Endpoint, UpdateError> {
    let wide: Vec<u16> = url.encode_utf16().chain(std::iter::once(0)).collect();
    let mut parts = URL_COMPONENTS {
        dwStructSize: size_of::<URL_COMPONENTS>() as u32,
        ..URL_COMPONENTS::default()
    };
    // `u32::MAX` asks WinHttpCrackUrl to point into `wide` instead of copying into caller buffers.
    parts.dwSchemeLength = u32::MAX;
    parts.dwHostNameLength = u32::MAX;
    parts.dwUrlPathLength = u32::MAX;
    parts.dwExtraInfoLength = u32::MAX;

    // SAFETY: `wide` is NUL-terminated and outlives the call, and every pointer/length pair below is
    // filled in by the API before it is read.
    unsafe { WinHttpCrackUrl(&wide, 0, &mut parts) }
        .map_err(|_| UpdateError::Url(url.to_owned()))?;

    let secure = match parts.nScheme {
        WINHTTP_INTERNET_SCHEME_HTTPS => true,
        WINHTTP_INTERNET_SCHEME_HTTP => false,
        _ => return Err(UpdateError::Url(url.to_owned())),
    };

    let host = wide_slice(parts.lpszHostName, parts.dwHostNameLength);
    if host.is_empty() {
        return Err(UpdateError::Url(url.to_owned()));
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

/// Copies a string WinHTTP pointed into the URL buffer.
fn wide_slice(pointer: windows::core::PWSTR, length: u32) -> String {
    if pointer.is_null() || length == 0 {
        return String::new();
    }

    // SAFETY: on success `WinHttpCrackUrl` sets the pointer into the caller's URL buffer and the
    // length to the number of UTF-16 units there; both outlive this copy.
    let slice = unsafe { std::slice::from_raw_parts(pointer.0, length as usize) };

    String::from_utf16_lossy(slice)
}

/// Fetches `url` and hands the body to `sink` in chunks.
///
/// # Errors
///
/// [`UpdateError::Url`] for a URL WinHTTP cannot crack, [`UpdateError::Http`] for a failed WinHTTP
/// call, [`UpdateError::Status`] for a non-2xx answer, [`UpdateError::TooLarge`] past `limit`, and
/// whatever the sink returns.
pub fn http_get(
    url: &str,
    timeout: Duration,
    limit: u64,
    sink: Sink<'_>,
) -> Result<u64, UpdateError> {
    let endpoint = endpoint(url)?;
    let agent: Vec<u16> = format!("neb-supervisor/{}", env!("CARGO_PKG_VERSION"))
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let access = if endpoint.is_local() {
        WINHTTP_ACCESS_TYPE_NO_PROXY
    } else {
        WINHTTP_ACCESS_TYPE_AUTOMATIC_PROXY
    };

    // SAFETY: the agent string is NUL-terminated and outlives the call; the proxy arguments are
    // ignored for the access types used here.
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
        return Err(http_error("open", url));
    }
    let session = Internet(session);

    let millis = i32::try_from(timeout.as_millis())
        .unwrap_or(i32::MAX)
        .max(1);
    // SAFETY: `session` is a live WinHTTP handle; timeouts are plain numbers.
    unsafe {
        let _ = WinHttpSetTimeouts(session.0, millis, millis, millis, millis);
    }

    let host: Vec<u16> = endpoint
        .host
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: the host string is NUL-terminated and outlives the call.
    let connection = unsafe { WinHttpConnect(session.0, PCWSTR(host.as_ptr()), endpoint.port, 0) };
    if connection.is_null() {
        return Err(http_error("connect", url));
    }
    let connection = Internet(connection);

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
    // SAFETY: every string is NUL-terminated and outlives the call; a null accept-types pointer is
    // the documented default and a null referrer is what `WINHTTP_NO_REFERER` is.
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
        return Err(http_error("open request", url));
    }
    let request = Internet(request);

    // SAFETY: `request` is a live handle; no body and no extra headers are sent.
    unsafe { WinHttpSendRequest(request.0, None, None, 0, 0, 0) }
        .map_err(|_| http_error("send", url))?;
    // SAFETY: as above; the reserved argument is documented as null.
    unsafe { WinHttpReceiveResponse(request.0, ptr::null_mut()) }
        .map_err(|_| http_error("receive", url))?;

    let mut status = 0u32;
    let mut length = size_of::<u32>() as u32;
    // SAFETY: `status` is a `u32` and `length` its size; the API writes both. A null index pointer
    // asks for the first header value.
    unsafe {
        WinHttpQueryHeaders(
            request.0,
            WINHTTP_QUERY_STATUS_CODE | WINHTTP_QUERY_FLAG_NUMBER,
            PCWSTR::null(),
            Some((&mut status as *mut u32).cast()),
            &mut length,
            ptr::null_mut(),
        )
    }
    .map_err(|_| http_error("status", url))?;
    if !(200..300).contains(&status) {
        return Err(UpdateError::Status {
            url: url.to_owned(),
            status,
        });
    }

    let mut buffer = vec![0u8; 16 * 1024];
    let mut total = 0u64;
    loop {
        let mut read = 0u32;
        // SAFETY: `buffer` is writable for its whole length and `read` is a valid out-parameter.
        unsafe {
            WinHttpReadData(
                request.0,
                buffer.as_mut_ptr().cast(),
                buffer.len() as u32,
                &mut read,
            )
        }
        .map_err(|_| http_error("read", url))?;

        if read == 0 {
            break;
        }

        total = total.saturating_add(u64::from(read));
        if total > limit {
            return Err(UpdateError::TooLarge {
                url: url.to_owned(),
                limit,
            });
        }

        sink(&buffer[..read as usize])?;
    }

    Ok(total)
}

/// Turns the thread's last Win32 error into an [`UpdateError::Http`].
fn http_error(action: &'static str, url: &str) -> UpdateError {
    // SAFETY: `GetLastError` reads this thread's last error and changes nothing.
    let code = unsafe { GetLastError() }.0;

    UpdateError::Http {
        action,
        url: url.to_owned(),
        code,
    }
}

/// Downloads and parses the manifest.
///
/// # Errors
///
/// Everything [`http_get`] can return plus everything [`Manifest::parse`] can return.
pub fn fetch_manifest(url: &str, timeout: Duration) -> Result<Manifest, UpdateError> {
    // `raw.githubusercontent.com` caches a branch file for a few minutes, so a fresh release
    // would look invisible to the first checks after it. A query keeps each check a new URL.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let fresh = format!(
        "{url}{}t={stamp}",
        if url.contains('?') { '&' } else { '?' }
    );
    let mut body = Vec::new();
    http_get(&fresh, timeout, MANIFEST_LIMIT, &mut |chunk| {
        body.extend_from_slice(chunk);
        Ok(())
    })?;

    let text = std::str::from_utf8(&body)?;
    // A release pipeline that writes with a BOM must not make the manifest unreadable.
    Manifest::parse(text.strip_prefix('\u{feff}').unwrap_or(text))
}

/// The version this executable was built as.
pub fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("CARGO_PKG_VERSION is always SemVer")
}

/// What one check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckOutcome {
    /// No newer release.
    UpToDate,
    /// A newer release exists.
    Available,
    /// The update was handed to `neb-updater.exe`; the caller should stop.
    Applied,
    /// The update was downloaded but left for the user.
    Pending,
    /// The check could not complete.
    Failed,
}

/// The message the tray sends to the worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    /// Check now instead of waiting for the next interval.
    CheckNow,
    /// Finish the current request and exit the thread.
    Stop,
}

/// The handle the supervisor keeps so it can ask for a check and stop the worker on shutdown.
#[derive(Debug)]
pub struct UpdatesHandle {
    sender: Sender<Request>,
    cancel: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl UpdatesHandle {
    /// A sender the tray can clone; a request to a stopped worker is simply ignored.
    pub fn sender(&self) -> Sender<Request> {
        self.sender.clone()
    }

    /// Asks the worker to stop and detaches it.
    ///
    /// The join is deliberately omitted: the worker may be inside a WinHTTP read whose timeout is
    /// measured in minutes, and the supervisor is on its way out. The cancellation flag stops it at
    /// the next checkpoint, and the process exit reaps the thread either way.
    pub fn stop(&mut self) {
        self.cancel.store(true, Ordering::SeqCst);
        let _ = self.sender.send(Request::Stop);
        let _ = self.thread.take();
    }
}

impl Drop for UpdatesHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Starts the update worker.
///
/// The thread waits [`FIRST_CHECK_DELAY`] before its first request, then checks every
/// [`Settings::interval`] until [`UpdatesHandle::stop`] or an applied update.
pub fn start(
    settings: Settings,
    root: PathBuf,
    exe_dir: PathBuf,
    reporter: Reporter,
    notify: Notify,
    stop: StopFn,
) -> UpdatesHandle {
    let (sender, receiver) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel);

    let thread = thread::Builder::new()
        .name("neb-updates".to_owned())
        .spawn(move || {
            run(
                settings,
                WorkerContext {
                    root,
                    exe_dir,
                    cancel: worker_cancel,
                },
                reporter,
                notify,
                stop,
                receiver,
            )
        })
        .ok();

    UpdatesHandle {
        sender,
        cancel,
        thread,
    }
}

fn run(
    settings: Settings,
    context: WorkerContext,
    reporter: Reporter,
    notify: Notify,
    stop: StopFn,
    receiver: Receiver<Request>,
) {
    let mut first = true;

    loop {
        if context.cancel.load(Ordering::SeqCst) {
            return;
        }

        let wait = if first {
            FIRST_CHECK_DELAY
        } else {
            settings.interval()
        };

        match receiver.recv_timeout(wait) {
            Ok(Request::Stop) | Err(RecvTimeoutError::Disconnected) => return,
            Ok(Request::CheckNow) | Err(RecvTimeoutError::Timeout) => {}
        }
        first = false;

        if check_once(
            &settings,
            &context.root,
            &context.exe_dir,
            &reporter,
            &notify,
            &context.cancel,
        ) == CheckOutcome::Applied
        {
            stop();
            return;
        }
    }
}

/// The paths and the cancellation flag the worker needs on every request.
#[derive(Debug)]
struct WorkerContext {
    root: PathBuf,
    exe_dir: PathBuf,
    cancel: Arc<AtomicBool>,
}

/// Runs one check and, when the policy allows it, downloads, verifies, stages and hands over.
///
/// Failures are logged and returned as [`CheckOutcome::Failed`]; a background check has no caller to
/// propagate an error to.
pub fn check_once(
    settings: &Settings,
    root: &Path,
    exe_dir: &Path,
    reporter: &Reporter,
    notify: &Notify,
    cancel: &AtomicBool,
) -> CheckOutcome {
    if cancel.load(Ordering::SeqCst) {
        reporter.info("update check: cancelled");
        return CheckOutcome::Failed;
    }

    let manifest = match fetch_manifest(&settings.manifest_url, HTTP_TIMEOUT) {
        Ok(manifest) => manifest,
        Err(error) => {
            reporter.warn(&format!("update check: {error}"));
            return CheckOutcome::Failed;
        }
    };

    if cancel.load(Ordering::SeqCst) {
        reporter.info("update check: cancelled");
        return CheckOutcome::Failed;
    }

    if check_manifest(&manifest, reporter) != CheckOutcome::Available {
        return CheckOutcome::UpToDate;
    }

    let Some(asset) = manifest.portable.as_ref() else {
        reporter.warn("update: the manifest has no portable asset");
        return CheckOutcome::Failed;
    };

    let version = manifest.version.to_string();
    let paths = UpdatePaths::for_version(root, &manifest.version);

    // A version that was already downloaded stays downloaded: the next check must not spend the
    // user's connection on the same bytes.
    if let Some(pending) = read_pending(root) {
        if pending.version == version
            && paths.portable_zip.is_file()
            && staging_ready(&paths.staging)
        {
            reporter.info(&format!("update pending: {version} (already downloaded)"));

            return if settings.auto_apply {
                hand_over(root, exe_dir, &manifest.version, reporter)
            } else {
                CheckOutcome::Pending
            };
        }
    }

    match download(
        &asset.url,
        &asset.sha256,
        &paths.portable_zip,
        DOWNLOAD_TIMEOUT,
    ) {
        Ok(bytes) => reporter.info(&format!(
            "update download: {version} ({bytes} bytes, checksum ok)"
        )),
        Err(UpdateError::Checksum { .. }) => {
            reporter.error("update rejected: checksum");
            return CheckOutcome::Failed;
        }
        Err(error) => {
            reporter.error(&format!("update download: {error}"));
            return CheckOutcome::Failed;
        }
    }

    if cancel.load(Ordering::SeqCst) {
        reporter.info("update check: cancelled after the download");
        return CheckOutcome::Failed;
    }

    let files = match stage_zip(&paths.portable_zip, &paths.staging) {
        Ok(files) => files,
        Err(error) => {
            reporter.error(&format!("update stage: {error}"));
            return CheckOutcome::Failed;
        }
    };
    reporter.info(&format!(
        "update staged: {} files in '{}'",
        files.len(),
        paths.staging.display()
    ));

    match write_whats_new(
        root,
        &manifest.version,
        &manifest.notes,
        &manifest.changelog_url,
    ) {
        Ok(path) => reporter.info(&format!("update notes: '{}'", path.display())),
        Err(error) => reporter.warn(&format!("update notes: {error}")),
    }

    let pending = Pending {
        version: version.clone(),
        staging: paths.staging.display().to_string(),
        portable: paths.portable_zip.display().to_string(),
        notes: manifest.notes.clone(),
        changelog_url: manifest.changelog_url.clone(),
    };
    if let Err(error) = write_pending(root, &pending) {
        reporter.warn(&format!("update pending: cannot write the marker: {error}"));
    }

    if cancel.load(Ordering::SeqCst) {
        reporter.info("update check: cancelled before applying");
        return CheckOutcome::Failed;
    }

    if settings.auto_apply {
        return hand_over(root, exe_dir, &manifest.version, reporter);
    }

    reporter.info(&format!("update pending: {version} (autoApply is off)"));
    let body = manifest
        .notes
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Обновление скачано и готово к установке.");
    notify(&format!("ProjectNeb {version}"), body);

    CheckOutcome::Pending
}

/// Starts the hand-over program for an already staged version.
fn hand_over(root: &Path, exe_dir: &Path, version: &Version, reporter: &Reporter) -> CheckOutcome {
    let plan = ApplyPlan::new(root, exe_dir, version);

    match spawn_updater(std::process::id(), &plan) {
        Ok(pid) => {
            reporter.info(&format!(
                "update apply: '{}' spawned (pid {pid}), stopping to let it replace the files",
                plan.updater.display()
            ));
            CheckOutcome::Applied
        }
        Err(UpdateError::UpdaterMissing { path }) => {
            reporter.error(&format!(
                "update apply: '{}' is missing, the update was not applied",
                path.display()
            ));
            CheckOutcome::Pending
        }
        Err(error) => {
            reporter.error(&format!("update apply: {error}"));
            CheckOutcome::Failed
        }
    }
}

/// Compares a parsed manifest with the running version and logs the verdict.
pub fn check_manifest(manifest: &Manifest, reporter: &Reporter) -> CheckOutcome {
    let current = current_version();

    if manifest.version <= current {
        reporter.info(&format!(
            "update check: {} (current) \u{2014} up to date",
            manifest.version
        ));
        CheckOutcome::UpToDate
    } else {
        reporter.info(&format!(
            "update check: {} \u{2014} available (current {current})",
            manifest.version
        ));
        CheckOutcome::Available
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;

    #[test]
    fn versions_compare_by_number_not_by_text() {
        let older = Version::parse("0.9.9").expect("parse");
        let newer = Version::parse("0.10.0").expect("parse");

        assert!(older < newer, "0.9.9 must sort before 0.10.0");
        assert!(newer > older);
        assert_eq!(newer.to_string(), "0.10.0");
        assert!(matches!(
            Version::parse("v1.2.3"),
            Err(UpdateError::Version(version)) if version == "v1.2.3"
        ));
    }

    #[test]
    fn a_release_sorts_after_its_pre_releases() {
        let alpha = Version::parse("1.0.0-alpha").expect("parse");
        let alpha_one = Version::parse("1.0.0-alpha.1").expect("parse");
        let beta = Version::parse("1.0.0-beta").expect("parse");
        let release = Version::parse("1.0.0+build.7").expect("parse");

        assert!(alpha < alpha_one);
        assert!(alpha_one < beta);
        assert!(beta < release);
        assert!(alpha.is_prerelease());
        assert!(!release.is_prerelease());
        let with_build = Version::parse("1.0.0+build.7").expect("parse");
        assert_eq!(with_build, release);
    }

    #[test]
    fn missing_or_malformed_versions_are_refused() {
        for text in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "1.2.x",
            "1.2.3-",
            "1.2.3-alpha..1",
        ] {
            assert!(
                Version::parse(text).is_err(),
                "'{text}' must not parse as a version"
            );
        }
    }

    #[test]
    fn the_manifest_keeps_the_fields_the_update_needs() {
        let manifest = Manifest::parse(
            r#"{
                "version": "0.2.0",
                "channel": "beta",
                "notes": "Fixes the dock",
                "changelogUrl": "https://example.invalid/CHANGELOG.md",
                "assets": {
                    "portable": { "url": "https://example.invalid/p.zip", "sha256": "ABC" }
                }
            }"#,
        )
        .expect("parse");

        assert_eq!(manifest.version.to_string(), "0.2.0");
        assert_eq!(manifest.channel, "beta");
        assert_eq!(manifest.notes, "Fixes the dock");
        assert_eq!(
            manifest.changelog_url,
            "https://example.invalid/CHANGELOG.md"
        );
        assert_eq!(
            manifest.portable,
            Some(Asset {
                url: "https://example.invalid/p.zip".to_owned(),
                sha256: "ABC".to_owned()
            })
        );
    }

    #[test]
    fn a_manifest_without_optional_fields_still_parses() {
        let manifest = Manifest::parse(r#"{"version":"0.2.0"}"#).expect("parse");

        assert_eq!(manifest.channel, "beta");
        assert_eq!(manifest.notes, "");
        assert_eq!(manifest.changelog_url, DEFAULT_CHANGELOG_URL);
        assert_eq!(manifest.portable, None);
    }

    #[test]
    fn a_manifest_without_a_version_or_on_an_unknown_channel_is_refused() {
        assert!(matches!(
            Manifest::parse(r#"{"channel":"beta"}"#),
            Err(UpdateError::NoVersion)
        ));
        assert!(matches!(
            Manifest::parse(r#"{"version":"not-a-version"}"#),
            Err(UpdateError::Version(_))
        ));
        assert!(matches!(
            Manifest::parse(r#"{"version":"1.0.0","channel":"nightly"}"#),
            Err(UpdateError::Channel(channel)) if channel == "nightly"
        ));
        assert!(matches!(
            Manifest::parse("not json"),
            Err(UpdateError::Json(_))
        ));
    }

    #[test]
    fn both_known_channels_are_accepted() {
        for channel in ["beta", "stable", " Stable "] {
            let text = format!(r#"{{"version":"1.0.0","channel":"{channel}"}}"#);
            let manifest = Manifest::parse(&text).expect("parse");

            assert!(
                manifest.channel == "beta" || manifest.channel == "stable",
                "{channel}"
            );
        }
    }

    #[test]
    fn update_settings_default_to_a_six_hour_automatic_check() {
        let settings = Settings::default();

        assert!(settings.enabled);
        assert_eq!(settings.manifest_url, DEFAULT_MANIFEST_URL);
        assert_eq!(settings.check_hours, 6);
        assert!(settings.auto_apply);
        assert_eq!(settings.interval(), Duration::from_secs(6 * 60 * 60));
    }

    #[test]
    fn the_performance_profile_sets_the_check_interval() {
        for (profile, hours) in [("performance", 24), ("balanced", 6), ("quality", 6)] {
            let text = format!(r#"{{"performance":{{"profile":"{profile}"}}}}"#);
            let settings = Settings::from_text(&text);

            assert_eq!(settings.check_hours, hours, "{profile}");
            assert_eq!(
                settings.interval(),
                Duration::from_secs(hours * 60 * 60),
                "{profile}"
            );
        }

        assert_eq!(
            Settings::from_text(r#"{"performance":{"profile":"turbo"}}"#).check_hours,
            DEFAULT_CHECK_HOURS,
            "an unknown profile falls back to balanced"
        );
    }

    #[test]
    fn the_check_interval_override_beats_the_profile_and_a_legacy_key_beats_both() {
        let profile_only = Settings::from_text(
            r#"{"performance":{"profile":"performance","updateCheckHours":12}}"#,
        );
        assert_eq!(profile_only.check_hours, 12);

        let legacy = Settings::from_text(
            r#"{"performance":{"profile":"performance","updateCheckHours":12},
                "updates":{"checkHours":2}}"#,
        );
        assert_eq!(
            legacy.check_hours, 2,
            "an explicit updates.checkHours is kept"
        );

        let clamped =
            Settings::from_text(r#"{"performance":{"profile":"quality","updateCheckHours":0}}"#);
        assert_eq!(clamped.check_hours, MIN_CHECK_HOURS);
    }

    #[test]
    fn missing_settings_keys_keep_the_defaults() {
        let settings = Settings::from_text(r#"{"language":"ru","updates":{"enabled":false}}"#);

        assert!(!settings.enabled);
        assert_eq!(settings.manifest_url, DEFAULT_MANIFEST_URL);
        assert_eq!(settings.check_hours, DEFAULT_CHECK_HOURS);
        assert!(settings.auto_apply);
    }

    #[test]
    fn check_hours_is_clamped_and_a_broken_file_is_the_default_profile() {
        for (value, expected) in [(0, 1), (-9, 1), (6, 6), (999, 168)] {
            let text = format!(r#"{{"updates":{{"checkHours":{value}}}}}"#);
            assert_eq!(
                Settings::from_text(&text).check_hours,
                expected,
                "checkHours {value}"
            );
        }

        assert_eq!(Settings::from_text("{ not json").check_hours, 6);
        assert_eq!(
            Settings::from_text(r#"{"updates":{"manifestUrl":"  "}}"#).manifest_url,
            DEFAULT_MANIFEST_URL
        );
    }

    #[test]
    fn the_bom_the_c_sharp_build_writes_does_not_hide_the_settings() {
        let text = "\u{feff}{\"updates\":{\"checkHours\":2,\"autoApply\":false}}";
        let settings = Settings::from_text(text);

        assert_eq!(settings.check_hours, 2);
        assert!(!settings.auto_apply);
    }

    #[test]
    fn whats_new_round_trips_and_is_marked_shown() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        let version = Version::parse("0.2.0").expect("parse");

        let path =
            write_whats_new(root, &version, "Fixes", "https://example.invalid/log").expect("write");
        assert!(
            path.ends_with("updates\\whats-new.json") || path.ends_with("updates/whats-new.json")
        );

        let notes = read_whats_new(root).expect("notes");
        assert_eq!(notes.version, "0.2.0");
        assert_eq!(notes.notes, "Fixes");
        assert_eq!(notes.changelog_url, "https://example.invalid/log");
        assert!(!notes.shown);

        mark_shown(root).expect("mark");
        assert!(read_whats_new(root).expect("notes").shown);
    }

    /// Serves one HTTP response and closes the connection.
    fn serve_once(body: Vec<u8>) -> (String, JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");

        let thread = thread::spawn(move || {
            let Ok((mut socket, _)) = listener.accept() else {
                return;
            };
            let mut request = [0u8; 4096];
            let _ = socket.read(&mut request);
            let _ = write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = socket.write_all(&body);
        });

        (format!("http://{address}/latest.json"), thread)
    }

    #[test]
    fn winhttp_fetches_a_manifest_from_a_local_server() {
        let body = b"\xEF\xBB\xBF{\"version\":\"9.9.9\",\"channel\":\"beta\",\"notes\":\"local\"}"
            .to_vec();
        let (url, server) = serve_once(body);

        let manifest = fetch_manifest(&url, Duration::from_secs(5)).expect("fetch");

        server.join().expect("server");
        assert_eq!(manifest.version.to_string(), "9.9.9");
        assert_eq!(manifest.notes, "local");
    }

    #[test]
    fn a_non_two_hundred_answer_is_an_error() {
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");
        let server = thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request);
                let _ = socket.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let url = format!("http://{address}/missing.json");
        let error = fetch_manifest(&url, Duration::from_secs(5)).expect_err("404");

        server.join().expect("server");
        assert!(
            matches!(error, UpdateError::Status { status: 404, .. }),
            "{error:?}"
        );
    }

    #[test]
    fn the_verdict_of_a_check_is_the_running_version() {
        let manifest = Manifest::parse(
            r#"{"version":"0.1.0","channel":"beta","assets":{"portable":{"url":"x","sha256":"y"}}}"#,
        )
        .expect("parse");

        assert_eq!(
            check_manifest(&manifest, &Reporter::new(false)),
            CheckOutcome::UpToDate
        );
    }

    fn write_zip(path: &Path, entries: &[(&str, &[u8])]) {
        let file = File::create(path).expect("create the archive");
        let mut writer = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();

        for (name, body) in entries {
            writer.start_file(*name, options).expect("entry");
            writer.write_all(body).expect("body");
        }
        writer.finish().expect("finish");
    }

    #[test]
    fn a_download_is_hashed_and_a_mismatch_is_deleted() {
        let directory = tempfile::tempdir().expect("temp dir");
        let destination = directory.path().join("portable.zip");
        let body = b"portable-archive".to_vec();
        let digest = hex(Sha256::digest(&body));

        let (url, server) = serve_once(body.clone());
        let bytes = download(
            &url,
            &digest.to_ascii_uppercase(),
            &destination,
            Duration::from_secs(5),
        )
        .expect("download");
        server.join().expect("server");

        assert_eq!(bytes, body.len() as u64);
        assert_eq!(std::fs::read(&destination).expect("archive"), body);

        let (url, server) = serve_once(body);
        let error = download(
            &url,
            "0000000000000000000000000000000000000000000000000000000000000000",
            &destination,
            Duration::from_secs(5),
        )
        .expect_err("the checksum does not match");
        server.join().expect("server");

        assert!(matches!(error, UpdateError::Checksum { .. }), "{error:?}");
        assert!(
            !destination.exists(),
            "a rejected download must not be left behind"
        );
    }

    #[test]
    fn a_failed_download_leaves_no_partial_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let destination = directory.path().join("updates").join("portable.zip");
        let listener = TcpListener::bind("127.0.0.1:0").expect("listen");
        let address = listener.local_addr().expect("address");
        let server = thread::spawn(move || {
            if let Ok((mut socket, _)) = listener.accept() {
                let mut request = [0u8; 4096];
                let _ = socket.read(&mut request);
                let _ = socket.write_all(b"HTTP/1.1 500 Oops\r\nContent-Length: 0\r\n\r\n");
            }
        });

        let url = format!("http://{address}/portable.zip");
        let error = download(&url, "abc", &destination, Duration::from_secs(5)).expect_err("500");
        server.join().expect("server");

        assert!(matches!(error, UpdateError::Status { status: 500, .. }));
        assert!(!destination.exists(), "a failed download is cleaned up");
    }

    #[test]
    fn staging_strips_the_portable_root_and_lands_files_flat() {
        let directory = tempfile::tempdir().expect("temp dir");
        let archive = directory.path().join("portable.zip");
        let staging = directory.path().join("staging");
        write_zip(
            &archive,
            &[
                ("ProjectNeb/neb-shell.exe", b"shell"),
                ("ProjectNeb/sub/README.md", b"readme"),
                ("start-portable.cmd", b"@echo off"),
            ],
        );

        let files = stage_zip(&archive, &staging).expect("stage");

        assert_eq!(
            files,
            vec![
                "neb-shell.exe".to_owned(),
                "sub/README.md".to_owned(),
                "start-portable.cmd".to_owned()
            ]
        );
        assert_eq!(
            std::fs::read(staging.join("neb-shell.exe")).expect("exe"),
            b"shell"
        );
        assert_eq!(
            std::fs::read(staging.join("sub").join("README.md")).expect("readme"),
            b"readme"
        );
        assert!(
            !staging.join("ProjectNeb").exists(),
            "the wrapper folder must not survive"
        );
    }

    #[test]
    fn an_entry_that_escapes_the_staging_directory_is_refused() {
        for name in [
            "../evil.txt",
            "ProjectNeb/../../evil.txt",
            "/absolute/evil.txt",
            "C:/Windows/evil.txt",
            "ProjectNeb/file.txt:stream",
        ] {
            assert!(
                matches!(relative_entry(name), Err(UpdateError::UnsafeEntry(_))),
                "'{name}' must be refused"
            );
        }

        assert_eq!(
            relative_entry("ProjectNeb/neb-shell.exe").expect("safe"),
            Some("neb-shell.exe".to_owned())
        );
        assert_eq!(relative_entry("ProjectNeb/").expect("safe"), None);
        assert_eq!(
            relative_entry("other/thing.txt").expect("safe"),
            Some("other/thing.txt".to_owned()),
            "a file without the wrapper keeps its path"
        );
    }

    #[test]
    fn an_archive_with_an_escaping_entry_stages_nothing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let archive = directory.path().join("hostile.zip");
        let staging = directory.path().join("staging");
        write_zip(&archive, &[("../evil.txt", b"no")]);

        let error = stage_zip(&archive, &staging).expect_err("escaping entry");

        assert!(matches!(error, UpdateError::UnsafeEntry(_)), "{error:?}");
        assert!(!staging.join("evil.txt").exists());
        assert!(!directory.path().join("evil.txt").exists());
    }

    #[test]
    fn the_updater_command_line_matches_the_documented_contract() {
        let plan = ApplyPlan {
            updater: PathBuf::from(r"C:\app\neb-updater.exe"),
            staging: PathBuf::from(r"C:\data\updates\1.2.3\staging"),
            app: PathBuf::from(r"C:\app"),
            restart: PathBuf::from(r"C:\app\ProjectNeb.exe"),
            log: PathBuf::from(r"C:\data\updates\updater.log"),
        };

        let arguments = updater_arguments(4242, &plan);

        assert_eq!(
            arguments,
            vec![
                "--wait-pid",
                "4242",
                "--staging",
                r"C:\data\updates\1.2.3\staging",
                "--app",
                r"C:\app",
                "--restart",
                r"C:\app\ProjectNeb.exe",
                "--timeout-secs",
                "60",
                "--log",
                r"C:\data\updates\updater.log",
            ]
        );
    }

    #[test]
    fn a_missing_updater_is_an_error_and_starts_nothing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let plan = ApplyPlan {
            updater: directory.path().join("neb-updater.exe"),
            staging: directory.path().join("staging"),
            app: directory.path().to_path_buf(),
            restart: directory.path().join("ProjectNeb.exe"),
            log: directory.path().join("updater.log"),
        };

        let error = spawn_updater(1, &plan).expect_err("the updater is absent");

        assert!(
            matches!(error, UpdateError::UpdaterMissing { .. }),
            "{error:?}"
        );
    }

    #[test]
    fn a_pending_marker_round_trips_and_is_replaced() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path();
        let pending = Pending {
            version: "0.2.0".to_owned(),
            staging: r"C:\staging".to_owned(),
            portable: r"C:\portable.zip".to_owned(),
            notes: "Fixes".to_owned(),
            changelog_url: "https://example.invalid".to_owned(),
        };

        write_pending(root, &pending).expect("write");
        assert_eq!(read_pending(root), Some(pending.clone()));

        let next = Pending {
            version: "0.3.0".to_owned(),
            ..pending
        };
        write_pending(root, &next).expect("replace");
        assert_eq!(read_pending(root), Some(next));
    }

    #[test]
    fn a_version_becomes_one_safe_path_component() {
        let version = Version::parse("1.2.3-beta+build/7").expect("parse");

        assert_eq!(path_component(&version), "1.2.3-beta");
        assert!(
            UpdatePaths::for_version(Path::new(r"C:\data"), &version)
                .portable_zip
                .ends_with(r"updates\1.2.3-beta\portable.zip")
        );
    }
}
