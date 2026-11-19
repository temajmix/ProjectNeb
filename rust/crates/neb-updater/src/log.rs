//! One line per step, in the format the rest of ProjectNeb writes.
//!
//! [`neb_core::logging`] owns the application log and names its files `neb-YYYYMMDD.log` inside a
//! directory. The updater needs a *single* file per run, named `updater-<unix>.log` (or given by
//! `--log`), because it runs once and is then gone: pairing it with the resident processes' daily
//! files would bury the one run that explains how an update went. So the file is opened here, while
//! the parts that make the lines look the same — the level labels and the UTC timestamp — come from
//! `neb-core`, exactly like the supervisor's.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use neb_core::logging::Level;
use neb_core::time::UtcDateTime;

/// The log this process writes, or a sink that keeps quiet when no file could be opened.
#[derive(Debug)]
pub struct Log {
    file: Option<Mutex<File>>,
    path: PathBuf,
}

impl Log {
    /// Creates `path` and opens it for appending, creating its directory first.
    ///
    /// Appending is deliberate: a retried update must not erase the evidence of the attempt before
    /// it. The caller adds the run's start line, which is what separates two runs in one file.
    ///
    /// # Errors
    ///
    /// The [`io::Error`] from creating the directory or opening the file.
    pub fn open(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }

        let file = OpenOptions::new().create(true).append(true).open(path)?;

        Ok(Self {
            file: Some(Mutex::new(file)),
            path: path.to_path_buf(),
        })
    }

    /// A log that writes nothing, used when the file cannot be opened: a broken log must not stop an
    /// update, and the failure is reported on stderr by the caller.
    pub fn silent(path: PathBuf) -> Self {
        Self { file: None, path }
    }

    /// The file this log writes into, whether or not it could be opened.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Writes one [`Level::Info`] line.
    pub fn info(&self, module: &str, message: &str) {
        self.write(Level::Info, module, message);
    }

    /// Writes one [`Level::Warn`] line.
    pub fn warn(&self, module: &str, message: &str) {
        self.write(Level::Warn, module, message);
    }

    /// Writes one [`Level::Error`] line.
    pub fn error(&self, module: &str, message: &str) {
        self.write(Level::Error, module, message);
    }

    /// Writes one line: `2026-11-17T16:15:30.123Z INFO module message`.
    ///
    /// A line that cannot be written is dropped, the same choice `neb_core::logging` makes: the
    /// process is already in trouble, and the write failure has nowhere better to go.
    pub fn write(&self, level: Level, module: &str, message: &str) {
        let Some(file) = &self.file else {
            return;
        };

        let mut line = String::with_capacity(48 + module.len() + message.len());
        line.push_str(&UtcDateTime::now_utc().iso8601());
        line.push(' ');
        line.push_str(level.as_str());
        line.push(' ');
        push_sanitised(&mut line, module);
        line.push(' ');
        push_sanitised(&mut line, message);
        line.push('\n');

        let mut file = match file.lock() {
            Ok(file) => file,
            Err(poisoned) => poisoned.into_inner(),
        };
        let _ = file.write_all(line.as_bytes()).and_then(|()| file.flush());
    }
}

/// The default log file: `<app>\..\logs\updater-<unix>.log`.
///
/// The unix stamp makes every run visible instead of letting the newest one overwrite the last
/// explanation of a failed update.
pub fn default_path(app: &Path) -> PathBuf {
    let root = app.parent().filter(|parent| !parent.as_os_str().is_empty());
    let logs = root.map_or_else(|| app.join("logs"), |root| root.join("logs"));
    let unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();

    logs.join(format!("updater-{unix}.log"))
}

/// Copies `value` into `target`, replacing control characters so one message cannot forge a second
/// line, exactly as `neb_core::logging` does.
fn push_sanitised(target: &mut String, value: &str) {
    for character in value.chars() {
        target.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &Path) -> String {
        fs::read_to_string(path).expect("read the log")
    }

    #[test]
    fn a_line_carries_a_utc_timestamp_a_level_and_the_message() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("updater.log");
        let log = Log::open(&path).expect("open the log");

        log.info("copy", "neb-shell.exe <- staging (1024 bytes)");

        let text = read(&path);
        assert_eq!(text.lines().count(), 1, "{text}");
        let line = text.trim_end();
        let (stamp, rest) = line.split_once(' ').expect("a timestamp then a level");
        assert!(stamp.ends_with('Z'), "the stamp is UTC: {stamp}");
        assert_eq!(&stamp[4..5], "-", "{stamp}");
        assert_eq!(&stamp[10..11], "T", "{stamp}");
        assert_eq!(rest, "INFO copy neb-shell.exe <- staging (1024 bytes)");
    }

    #[test]
    fn opening_creates_the_directory_and_appends_instead_of_truncating() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("nested").join("deeper").join("run.log");

        Log::open(&path).expect("open once").info("start", "first");
        Log::open(&path)
            .expect("open twice")
            .info("start", "second");

        let text = read(&path);
        assert_eq!(text.lines().count(), 2, "{text}");
        assert!(text.contains("first"));
        assert!(text.contains("second"));
    }

    #[test]
    fn a_newline_in_a_message_cannot_forge_a_line() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("run.log");
        let log = Log::open(&path).expect("open the log");

        log.warn("copy", "busy\nthen idle");

        let text = read(&path);
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains(" WARN copy busy then idle\n"), "{text}");
    }

    #[test]
    fn a_silent_log_writes_no_file() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("absent.log");
        let log = Log::silent(path.clone());

        log.error("wait", "nothing happened");

        assert!(!path.exists());
        assert_eq!(log.path(), path);
    }

    #[test]
    fn the_default_path_is_beside_the_app_in_its_log_directory() {
        let app = PathBuf::from(r"C:\Portable\ProjectNeb\app");
        let path = default_path(&app);

        assert_eq!(
            path.parent(),
            Some(Path::new(r"C:\Portable\ProjectNeb\logs"))
        );
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        assert!(name.starts_with("updater-"), "{name}");
        assert!(name.ends_with(".log"), "{name}");
        assert!(
            name.trim_start_matches("updater-")
                .trim_end_matches(".log")
                .bytes()
                .all(|byte| byte.is_ascii_digit()),
            "the name carries a unix stamp: {name}"
        );
    }
}
