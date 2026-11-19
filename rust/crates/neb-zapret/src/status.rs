//! The status file the elevated helper writes and the manager reads.
//!
//! The helper has administrator rights and the manager deliberately does not, so the only thing
//! they share is this file: `…\ProjectNeb\zapret\status.json`. It is written atomically (temporary
//! file, then rename), which means a reader never sees half a document even while the helper is
//! updating it.
//!
//! The reader trusts the file only as far as the pid check in [`crate::manager`] allows: a status
//! that says `running` with a pid that is gone is stale, and the manager reports it as stopped.

use std::fs;
use std::io;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Shape version of the status document.
pub const SCHEMA_VERSION: u32 = 1;

/// What the helper knows about the last `winws.exe` it started.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct StatusFile {
    /// Shape version, [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Process id of `winws.exe` while it is alive.
    pub pid: Option<u32>,
    /// Strategy the process was started with.
    pub strategy: String,
    /// Whether the helper believes the process is alive.
    pub running: bool,
    /// Whether the helper had administrator rights.
    pub elevated: bool,
    /// Version marker of the pack the process was started from.
    pub pack_version: String,
    /// Unix time in milliseconds when the process was started.
    pub started_at_ms: u64,
    /// Exit code once `winws.exe` has ended.
    pub exit_code: Option<i32>,
    /// Whether a stop was requested, so a non-zero exit is not reported as a crash.
    pub stop_requested: bool,
    /// Why the last attempt failed, in the helper's words.
    pub last_error: Option<String>,
    /// A short human-readable note about the last transition.
    pub message: String,
}

impl Default for StatusFile {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            pid: None,
            strategy: String::new(),
            running: false,
            elevated: false,
            pack_version: String::new(),
            started_at_ms: 0,
            exit_code: None,
            stop_requested: false,
            last_error: None,
            message: String::new(),
        }
    }
}

impl StatusFile {
    /// A document for a process that has just started.
    pub fn started(pid: u32, strategy: &str, pack_version: &str, elevated: bool) -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            pid: Some(pid),
            strategy: strategy.to_owned(),
            running: true,
            elevated,
            pack_version: pack_version.to_owned(),
            started_at_ms: now_ms(),
            exit_code: None,
            stop_requested: false,
            last_error: None,
            message: format!("winws.exe started (pid {pid})"),
        }
    }
}

/// Reads `path`; `None` means there is no status yet, which is the ordinary first run.
///
/// # Errors
///
/// [`io::ErrorKind::InvalidData`] when the file exists but is not a status document. A caller
/// should report that and still treat the module as stopped, because a corrupt status cannot make a
/// process alive.
pub fn read(path: &Path) -> io::Result<Option<StatusFile>> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };

    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes);
    serde_json::from_slice(bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Writes `status` atomically: a temporary sibling, then a rename over `path`.
///
/// # Errors
///
/// [`io::Error`] from creating the directory, writing the temporary file, or the rename.
pub fn write(path: &Path, status: &StatusFile) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let mut bytes = serde_json::to_vec_pretty(status)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    bytes.push(b'\n');

    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));
    fs::write(&temporary, bytes)?;

    if let Err(error) = fs::rename(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }

    Ok(())
}

/// Unix time in milliseconds, or zero when the clock is before the epoch.
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn a_missing_file_is_not_an_error_and_not_a_status() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("status.json");

        assert_eq!(read(&path).expect("read"), None);
    }

    #[test]
    fn a_status_round_trips_through_the_file() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("zapret").join("status.json");
        let status = StatusFile::started(4242, "general (ALT)", "0123456789abcdef", true);

        write(&path, &status).expect("write");
        let back = read(&path).expect("read").expect("a status");

        assert_eq!(back, status);
        assert_eq!(back.pid, Some(4242));
        assert_eq!(back.strategy, "general (ALT)");
        assert!(back.running);
        assert!(back.elevated);
        assert_eq!(back.schema_version, SCHEMA_VERSION);
    }

    #[test]
    fn a_stopped_status_clears_the_pid_and_keeps_the_message() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("status.json");
        let mut status = StatusFile::started(7, "general", "v", false);
        status.running = false;
        status.pid = None;
        status.stop_requested = true;
        status.message = "остановлено".to_owned();

        write(&path, &status).expect("write");
        let back = read(&path).expect("read").expect("a status");

        assert!(!back.running);
        assert_eq!(back.pid, None);
        assert!(back.stop_requested);
        assert_eq!(back.message, "остановлено");
    }

    #[test]
    fn the_json_uses_the_camel_case_names() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("status.json");
        write(&path, &StatusFile::started(1, "general", "abc", true)).expect("write");
        let text = fs::read_to_string(&path).expect("read");

        for key in [
            "schemaVersion",
            "packVersion",
            "startedAtMs",
            "stopRequested",
            "lastError",
        ] {
            assert!(text.contains(key), "{key} missing from {text}");
        }
        assert!(!text.contains("pack_version"), "{text}");
    }

    #[test]
    fn a_corrupt_status_is_a_typed_error_rather_than_a_missing_one() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("status.json");
        fs::write(&path, b"{not json").expect("write");

        let error = read(&path).expect_err("must fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn writing_leaves_no_temporary_file() {
        let directory = tempdir().expect("temp dir");
        let path = directory.path().join("status.json");

        write(&path, &StatusFile::default()).expect("write");

        let names: Vec<String> = fs::read_dir(directory.path())
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["status.json".to_owned()]);
    }

    #[test]
    fn the_started_document_says_what_started() {
        let status = StatusFile::started(99, "general", "ff", true);

        assert!(status.running);
        assert!(status.elevated);
        assert_eq!(status.exit_code, None);
        assert!(status.last_error.is_none());
        assert!(status.message.contains("99"));
        assert!(status.started_at_ms > 0);
    }
}
