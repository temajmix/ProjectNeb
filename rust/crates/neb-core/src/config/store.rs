//! Reading and writing `config.json`.
//!
//! `save` is atomic and durable: the bytes go to `config.json.tmp-<pid>`, that file is flushed to the
//! physical disk, and only then is it renamed over the real file. A power cut therefore leaves either
//! the old file or the new one, never a half-written one.
//!
//! Saves from two processes are serialised by a lock file next to the config (`config.json.lock`)
//! instead of by chance: the loser gets [`ConfigError::Locked`] and its file is untouched. A lock left
//! behind by a crashed process is taken over once it is [`LOCK_STALE_AFTER`] old, so a crash cannot
//! wedge every later save.
//!
//! `load_checked` distinguishes the three cases that a single "deserialize or defaults" call used to
//! blur: a missing file is the first run, a file that cannot be decoded is backed up, and a file that
//! exists but cannot be *read* (a scanner or another process holding it) is an error the caller must
//! see. [`ConfigStore::load`] keeps the old infallible shape for read-only callers, but a store that
//! saw an unreadable file refuses to [`save`](ConfigStore::save) until a load succeeds, so a transient
//! sharing violation can never turn into an overwrite of a good config.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, SystemTime};

use super::model::Config;
use crate::time::UtcDateTime;

/// Age at which a lock file is assumed to belong to a process that died mid-save.
const LOCK_STALE_AFTER: Duration = Duration::from_secs(60);

/// Suffix of the lock file that serialises saves.
const LOCK_SUFFIX: &str = ".lock";

/// How many times a failed read is retried before it is reported as unreadable.
const READ_ATTEMPTS: usize = 3;

/// Pause between read attempts, long enough for a scanner that holds the file to let go.
const READ_RETRY_DELAY: Duration = Duration::from_millis(50);

/// What went wrong while loading or saving the config.
#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    /// The file system refused the operation.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `write` or `replace`.
        action: &'static str,
        /// File the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The config could not be rendered as JSON. Only reachable from a value that cannot be
    /// serialised, since every in-memory [`Config`] is marshalable.
    #[error("config is not valid json: {0}")]
    Json(#[from] serde_json::Error),

    /// Another process holds the save lock.
    #[error("another process is saving '{}'; try again", path.display())]
    Locked {
        /// The config file the caller tried to write.
        path: PathBuf,
    },

    /// The file exists but could not be read, even after retrying. The file is intact; this process
    /// simply is not allowed to look at it right now.
    #[error("'{}' exists but cannot be read: {source}", path.display())]
    Unreadable {
        /// The config file that could not be read.
        path: PathBuf,
        /// The last error the file system gave.
        #[source]
        source: io::Error,
    },

    /// A save was refused because the last load could not read the existing file, so the config in
    /// memory is defaults rather than the user's settings.
    #[error(
        "refusing to overwrite '{}': the last load could not read it, load again first",
        path.display()
    )]
    NotLoaded {
        /// The config file the caller tried to write.
        path: PathBuf,
    },
}

/// Loads and saves one `config.json`.
///
/// Clones share the "the last load could not read the file" flag, because they address the same file
/// and the same danger.
#[derive(Debug, Clone)]
pub struct ConfigStore {
    path: PathBuf,
    unreadable: Arc<AtomicBool>,
}

impl ConfigStore {
    /// A store for an explicit file, used by tests and by tools that take a path.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            unreadable: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The store for the real `%LocalAppData%\ProjectNeb\config.json`.
    pub fn at_default_location() -> Self {
        Self::new(crate::paths::config_file())
    }

    /// The file this store owns.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads the config, falling back to defaults and preserving a broken file.
    ///
    /// A missing file is the ordinary first run; a file that cannot be decoded is copied aside before
    /// defaults are returned, so the user's original bytes are never silently overwritten.
    ///
    /// A file that exists but cannot be read yields defaults *and* marks this store, so a following
    /// [`save`](Self::save) is refused instead of replacing a config this process never managed to
    /// look at. Callers that can tell the user what happened should use [`load_checked`](Self::load_checked).
    pub fn load(&self) -> Config {
        self.load_checked().unwrap_or_default()
    }

    /// Reads the config and reports a file that exists but cannot be read.
    ///
    /// Unlike [`load`](Self::load) this separates "nothing there yet" (defaults) and "unreadable"
    /// (a retryable error), so a caller that is about to write can decide to wait or to tell the user,
    /// rather than starting from defaults and overwriting a good file.
    ///
    /// # Errors
    ///
    /// [`ConfigError::Unreadable`] when the file is present but could not be read after
    /// [`READ_ATTEMPTS`] tries.
    pub fn load_checked(&self) -> Result<Config, ConfigError> {
        let result = self.read_config();
        self.unreadable.store(result.is_err(), Ordering::SeqCst);
        result
    }

    /// The shared read path: missing → defaults, undecodable → backup + defaults, unreadable → error.
    fn read_config(&self) -> Result<Config, ConfigError> {
        let bytes = match read_with_retries(&self.path) {
            ReadOutcome::Bytes(bytes) => bytes,
            ReadOutcome::Missing => return Ok(Config::default()),
            ReadOutcome::Unreadable(source) => {
                return Err(ConfigError::Unreadable {
                    path: self.path.clone(),
                    source,
                });
            }
        };

        let Ok(text) = std::str::from_utf8(strip_bom(&bytes)) else {
            self.back_up_corrupt_file();
            return Ok(Config::default());
        };

        match serde_json::from_str(text) {
            Ok(config) => Ok(config),
            Err(_) => {
                self.back_up_corrupt_file();
                Ok(Config::default())
            }
        }
    }

    /// Writes `config` atomically and durably.
    ///
    /// # Errors
    ///
    /// [`ConfigError::NotLoaded`] when the last read failed, [`ConfigError::Locked`] when another
    /// process is mid-save, [`ConfigError::Io`] when the directory or file cannot be written, and
    /// [`ConfigError::Json`] if serialisation fails.
    pub fn save(&self, config: &Config) -> Result<(), ConfigError> {
        if self.unreadable.load(Ordering::SeqCst) {
            // The config in memory is defaults, not the user's settings: writing it would destroy a
            // file this process could not read.
            return Err(ConfigError::NotLoaded {
                path: self.path.clone(),
            });
        }

        if let Some(parent) = self.path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| io_error("create", parent, source))?;
        }

        let _lock = self.acquire_lock()?;

        // Render through a value so whole numbers can be written the way `System.Text.Json` writes a
        // `double`: `1440`, not `1440.0`. The file is the compatibility surface with the C# build.
        let mut document = serde_json::to_value(config)?;
        whole_numbers_as_integers(&mut document);
        let mut bytes = serde_json::to_vec_pretty(&document)?;
        bytes.push(b'\n');

        let tmp = self.tmp_path();
        write_and_sync(&tmp, &bytes)?;

        if let Err(source) = fs::rename(&tmp, &self.path) {
            // Leaving a half-written tmp file behind would confuse the next save on platforms where
            // rename cannot replace; the bytes in it are already worthless.
            let _ = fs::remove_file(&tmp);
            return Err(io_error("replace", &self.path, source));
        }

        Ok(())
    }

    /// Path of the lock file that serialises saves; exposed for the stale-lock test.
    fn lock_path(&self) -> PathBuf {
        let mut path = self.path.clone().into_os_string();
        path.push(LOCK_SUFFIX);
        PathBuf::from(path)
    }

    /// Path of this process's temporary file.
    fn tmp_path(&self) -> PathBuf {
        let name = self
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config.json".to_owned());
        self.path
            .with_file_name(format!("{name}.tmp-{}", process::id()))
    }

    /// Takes the save lock, or explains why not.
    fn acquire_lock(&self) -> Result<LockGuard, ConfigError> {
        let path = self.lock_path();

        if let Some(parent) = path.parent().filter(|dir| !dir.as_os_str().is_empty()) {
            fs::create_dir_all(parent).map_err(|source| io_error("create", parent, source))?;
        }

        match create_lock_file(&path) {
            Ok(()) => Ok(LockGuard { path }),
            Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                if !lock_is_stale(&path) {
                    return Err(ConfigError::Locked {
                        path: self.path.clone(),
                    });
                }

                // A process died mid-save. Removing and re-creating is racy by nature: if two savers
                // do it at once, exactly one wins the create and the other still reports Locked.
                let _ = fs::remove_file(&path);
                match create_lock_file(&path) {
                    Ok(()) => Ok(LockGuard { path }),
                    Err(source) if source.kind() == io::ErrorKind::AlreadyExists => {
                        Err(ConfigError::Locked {
                            path: self.path.clone(),
                        })
                    }
                    Err(source) => Err(io_error("create", &path, source)),
                }
            }
            Err(source) => Err(io_error("create", &path, source)),
        }
    }

    /// Copies the unreadable file aside so a later save cannot destroy the evidence.
    fn back_up_corrupt_file(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };

        let stem = self
            .path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_else(|| "config".to_owned());
        let extension = self
            .path
            .extension()
            .map(|extension| extension.to_string_lossy().into_owned())
            .unwrap_or_else(|| "json".to_owned());
        let stamp = UtcDateTime::now_utc().backup_stamp();

        let backup = parent.join(format!("{stem}.broken-{stamp}.{extension}"));
        let _ = fs::copy(&self.path, backup);
    }
}

/// Holds the save lock and drops it by deleting the file.
#[derive(Debug)]
struct LockGuard {
    path: PathBuf,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Creates the lock file, failing with `AlreadyExists` when someone else holds it.
fn create_lock_file(path: &Path) -> io::Result<()> {
    OpenOptions::new().write(true).create_new(true).open(path)?;
    Ok(())
}

/// Whether a lock file is old enough to belong to a process that will never release it.
fn lock_is_stale(path: &Path) -> bool {
    let Ok(modified) = fs::metadata(path).and_then(|metadata| metadata.modified()) else {
        // The lock vanished between the create attempt and now: let the caller try again.
        return true;
    };

    match SystemTime::now().duration_since(modified) {
        Ok(age) => age > LOCK_STALE_AFTER,
        // The timestamp is in the future, so the clock moved; treat the lock as live.
        Err(_) => false,
    }
}

/// Writes `bytes` to `path` and flushes them to the physical disk.
fn write_and_sync(path: &Path, bytes: &[u8]) -> Result<(), ConfigError> {
    let mut file = File::create(path).map_err(|source| io_error("create", path, source))?;
    file.write_all(bytes)
        .map_err(|source| io_error("write", path, source))?;
    file.sync_all()
        .map_err(|source| io_error("flush", path, source))?;
    Ok(())
}

/// What one read attempt produced.
enum ReadOutcome {
    /// The bytes on disk.
    Bytes(Vec<u8>),
    /// There is no file, which is the ordinary first run.
    Missing,
    /// The file is there but the file system refused to hand it over.
    Unreadable(io::Error),
}

/// Reads the config, retrying briefly so a scanner or an antivirus that opened it for a moment does
/// not look like a corrupt file.
fn read_with_retries(path: &Path) -> ReadOutcome {
    let mut last = None;

    for attempt in 0..READ_ATTEMPTS {
        match fs::read(path) {
            Ok(bytes) => return ReadOutcome::Bytes(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => return ReadOutcome::Missing,
            Err(error) => {
                last = Some(error);
                if attempt + 1 < READ_ATTEMPTS {
                    thread::sleep(READ_RETRY_DELAY);
                }
            }
        }
    }

    ReadOutcome::Unreadable(
        last.unwrap_or_else(|| io::Error::other("the config could not be read")),
    )
}

/// Rewrites every whole `f64` in the document as an integer, recursively.
///
/// `System.Text.Json` writes a `double` holding 1280 as `1280`, so a file this crate writes has to do
/// the same or every save produces a diff on the other side. Numbers with a fraction, and values too
/// large to be exact as `i64`, are left alone.
fn whole_numbers_as_integers(value: &mut serde_json::Value) {
    use serde_json::Value;

    match value {
        Value::Number(number) => {
            let Some(float) = number.as_f64() else {
                return;
            };
            let exact = float.fract() == 0.0 && float.abs() < 9_007_199_254_740_992.0;
            if exact {
                *number = serde_json::Number::from(float as i64);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(whole_numbers_as_integers),
        Value::Object(fields) => fields.values_mut().for_each(whole_numbers_as_integers),
        _ => {}
    }
}

/// Drops a UTF-8 byte-order mark, which `File.ReadAllText` on the other side also tolerates.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Builds a [`ConfigError::Io`] for `path`.
fn io_error(action: &'static str, path: &Path, source: io::Error) -> ConfigError {
    ConfigError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{SceneKind, WallpaperSettings, WindowSettings};
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;
    use std::time::{Duration, SystemTime};
    use tempfile::{TempDir, tempdir};

    fn temp_store() -> (TempDir, ConfigStore) {
        let dir = tempdir().expect("temp dir");
        let store = ConfigStore::new(dir.path().join("config.json"));
        (dir, store)
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(dir)
            .expect("read dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        names.sort();
        names
    }

    fn backup_names(dir: &Path) -> Vec<String> {
        names(dir)
            .into_iter()
            .filter(|name| name.starts_with("config.broken-"))
            .collect()
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).expect("read config")).expect("valid json")
    }

    /// Opens `path` without sharing, which is what a scanner holding the file looks like to Rust.
    #[cfg(windows)]
    fn hold_exclusively(path: &Path) -> fs::File {
        use std::os::windows::fs::OpenOptionsExt;

        fs::OpenOptions::new()
            .read(true)
            .share_mode(0)
            .open(path)
            .expect("hold the file exclusively")
    }

    #[test]
    fn missing_file_loads_defaults() {
        let (_dir, store) = temp_store();

        assert_eq!(store.load_checked().expect("load"), Config::default());
        assert!(!store.path().exists(), "load must not create the file");
    }

    #[test]
    fn empty_file_is_backed_up_and_defaults_returned() {
        let (dir, store) = temp_store();
        fs::write(store.path(), "").expect("write empty config");

        assert_eq!(store.load_checked().expect("load"), Config::default());
        assert_eq!(backup_names(dir.path()).len(), 1);

        // A file that was *decoded* as broken is safe to replace: its bytes are in the backup.
        store.save(&Config::default()).expect("save after a backup");
    }

    #[test]
    fn truncated_json_is_backed_up_with_its_bytes() {
        let (dir, store) = temp_store();
        let corrupt = r#"{"schemaVersion": 1, "language": "en", broken"#;
        fs::write(store.path(), corrupt).expect("write corrupt config");

        assert_eq!(store.load_checked().expect("load"), Config::default());

        let backups = backup_names(dir.path());
        assert_eq!(backups.len(), 1, "one backup per broken file: {backups:?}");
        let backup = dir.path().join(&backups[0]);
        assert!(backup.to_string_lossy().contains("config.broken-"));
        assert_eq!(fs::read_to_string(backup).expect("read backup"), corrupt);
    }

    #[test]
    fn load_accepts_a_utf8_bom() {
        let (_dir, store) = temp_store();
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(br#"{"schemaVersion": 1, "language": "de"}"#);
        fs::write(store.path(), bytes).expect("write bom config");

        assert_eq!(store.load_checked().expect("load").language, "de");
    }

    #[test]
    fn save_writes_the_c_sharp_shape() {
        let (_dir, store) = temp_store();

        store.save(&Config::default()).expect("save");
        let value = read_json(store.path());

        assert_eq!(value["schemaVersion"].as_u64(), Some(1));
        assert_eq!(value["language"].as_str(), Some("ru"));
        assert_eq!(value["theme"]["name"].as_str(), Some("Dark"));
        for module in ["wallpapers", "zapret", "windhawk", "discordRp"] {
            assert_eq!(value["modules"][module].as_bool(), Some(false), "{module}");
        }
        assert!(value["window"]["left"].is_null());
        assert!(value["window"]["top"].is_null());
        assert_eq!(value["window"]["width"].as_f64(), Some(1280.0));
        assert_eq!(value["window"]["height"].as_f64(), Some(800.0));
        assert_eq!(value["window"]["maximized"].as_bool(), Some(false));
        assert_eq!(value["wallpapers"]["enabled"].as_bool(), Some(false));
        assert_eq!(value["wallpapers"]["kind"].as_str(), Some("Color"));
        assert!(value["wallpapers"]["path"].is_null());
        assert_eq!(value["wallpapers"]["color"].as_str(), Some("#141021"));
        assert_eq!(
            value["wallpapers"]["playlist"].as_array().map(Vec::len),
            Some(0)
        );
        assert_eq!(value["wallpapers"]["rotateMinutes"].as_i64(), Some(30));
        assert_eq!(
            value["wallpapers"]["pauseOnFullscreen"].as_bool(),
            Some(true)
        );
        assert_eq!(value["wallpapers"]["pauseOnBattery"].as_bool(), Some(true));
    }

    #[test]
    fn unknown_keys_survive_a_round_trip() {
        let (_dir, store) = temp_store();
        fs::write(
            store.path(),
            r##"{
  "schemaVersion": 1,
  "language": "en",
  "theme": { "name": "Dark" },
  "modules": { "wallpapers": true, "zapret": false, "windhawk": true, "discordRp": false },
  "window": { "left": 100.5, "top": -40, "width": 1600, "height": 900, "maximized": true },
  "wallpapers": {
    "enabled": true,
    "kind": "Image",
    "path": "C:\\Users\\tester\\Pictures\\a.png",
    "color": "#AABBCC",
    "playlist": ["C:\\one.png"],
    "rotateMinutes": 15,
    "pauseOnFullscreen": false,
    "pauseOnBattery": true
  },
  "futureFlag": true,
  "futureObject": { "nested": [1, 2, 3] }
}"##,
        )
        .expect("write config");

        let config = store.load_checked().expect("load");
        assert_eq!(config.language, "en");
        assert!(config.modules.wallpapers);
        assert!(config.modules.windhawk);
        assert_eq!(config.window.left, Some(100.5));
        assert_eq!(config.window.top, Some(-40.0));
        assert_eq!(config.window.width, 1600.0);
        assert!(config.window.maximized);
        assert_eq!(config.wallpapers.kind, SceneKind::Image);
        assert_eq!(config.wallpapers.rotate_minutes, 15);
        assert!(!config.wallpapers.pause_on_fullscreen);
        assert_eq!(config.extra.len(), 2);

        store.save(&config).expect("save");
        let value = read_json(store.path());

        assert_eq!(value["futureFlag"], json!(true));
        assert_eq!(value["futureObject"]["nested"], json!([1, 2, 3]));
        assert_eq!(value["window"]["width"].as_f64(), Some(1600.0));
        assert_eq!(value["wallpapers"]["kind"].as_str(), Some("Image"));
        assert_eq!(
            value["wallpapers"]["path"].as_str(),
            Some(r"C:\Users\tester\Pictures\a.png")
        );
        assert_eq!(
            config,
            store.load_checked().expect("reload"),
            "save/load round trip must be lossless"
        );
    }

    #[test]
    fn cyrillic_survives_a_round_trip() {
        let (_dir, store) = temp_store();
        let wallpaper = r"C:\Users\tester\Рисунки\обои ночь.png";
        let mut extra = serde_json::Map::new();
        extra.insert("profileName".to_owned(), json!("Кириллица — тест"));

        let config = Config {
            language: "ru".to_owned(),
            wallpapers: WallpaperSettings {
                enabled: true,
                path: Some(wallpaper.to_owned()),
                ..WallpaperSettings::default()
            },
            extra,
            ..Config::default()
        };

        store.save(&config).expect("save");
        let text = fs::read_to_string(store.path()).expect("config is valid utf-8");

        assert!(text.contains("Кириллица — тест"), "{text}");
        assert!(text.contains("обои ночь.png"), "{text}");
        assert!(
            !text.contains(r"\u04"),
            "non-ascii must be written as utf-8, not escapes"
        );

        let reloaded = store.load_checked().expect("reload");
        assert_eq!(reloaded, config);
        assert_eq!(
            reloaded.extra.get("profileName").and_then(Value::as_str),
            Some("Кириллица — тест")
        );
    }

    #[test]
    fn save_replaces_an_existing_file() {
        let (_dir, store) = temp_store();
        fs::write(store.path(), "x".repeat(4096)).expect("write old config");

        let config = Config {
            language: "ja".to_owned(),
            ..Config::default()
        };
        store.save(&config).expect("save over an existing file");

        assert_eq!(read_json(store.path())["language"].as_str(), Some("ja"));
    }

    #[test]
    fn save_leaves_no_temporary_or_lock_file() {
        let (dir, store) = temp_store();

        store.save(&Config::default()).expect("save");

        assert_eq!(names(dir.path()), vec!["config.json".to_owned()]);
    }

    #[test]
    fn save_creates_missing_directories() {
        let dir = tempdir().expect("temp dir");
        let store = ConfigStore::new(dir.path().join("a").join("b").join("config.json"));

        store.save(&Config::default()).expect("save");

        assert!(store.path().exists());
    }

    #[test]
    fn a_second_save_is_refused_while_the_lock_is_held() {
        let (_dir, store) = temp_store();

        let lock = store.acquire_lock().expect("take the lock");
        let result = store.save(&Config::default());
        match result {
            Err(ConfigError::Locked { .. }) => {}
            other => panic!("expected a typed lock error, got {other:?}"),
        }
        assert!(
            !store.path().exists(),
            "a refused save must not touch the file"
        );

        drop(lock);
        store
            .save(&Config::default())
            .expect("the lock is released");
    }

    #[test]
    fn a_lock_left_behind_by_a_crash_is_taken_over() {
        let (_dir, store) = temp_store();
        let lock_path = store.lock_path();
        fs::create_dir_all(lock_path.parent().expect("parent")).expect("create dir");
        fs::write(&lock_path, b"").expect("write stale lock");

        let long_ago = SystemTime::now() - LOCK_STALE_AFTER - Duration::from_secs(5);
        fs::File::options()
            .write(true)
            .open(&lock_path)
            .expect("open stale lock")
            .set_modified(long_ago)
            .expect("backdate stale lock");

        store
            .save(&Config::default())
            .expect("stale lock must not block saving");

        assert!(!lock_path.exists(), "the taken-over lock is removed again");
    }

    #[test]
    fn concurrent_saves_leave_a_valid_file() {
        let (_dir, store) = temp_store();
        let rounds = 4;

        // Each round holds the lock here, so the contender that runs first must fail, and then one
        // save succeeds once the lock is released. That makes "exactly one winner, exactly one
        // typed loser" a fact rather than a race that usually comes out that way.
        for round in 0..rounds {
            let guard = store.acquire_lock().expect("hold the save lock");

            let loser = std::thread::scope(|scope| {
                scope
                    .spawn(|| store.save(&Config::default()))
                    .join()
                    .expect("thread must not panic")
            });
            assert!(
                matches!(loser, Err(ConfigError::Locked { .. })),
                "round {round}: the contended save must report Locked, got {loser:?}"
            );

            drop(guard);

            let winner = std::thread::scope(|scope| {
                scope
                    .spawn(|| {
                        store.save(&Config {
                            language: format!("round-{round}"),
                            ..Config::default()
                        })
                    })
                    .join()
                    .expect("thread must not panic")
            });
            assert!(winner.is_ok(), "round {round}: the released save must win");

            let config = store.load_checked().expect("the file stays readable");
            assert_eq!(
                config.language,
                format!("round-{round}"),
                "round {round}: the winner's value must be intact"
            );
        }
    }

    #[test]
    fn a_held_config_file_is_reported_instead_of_overwritten() {
        let (_dir, store) = temp_store();
        let original = Config {
            language: "fr".to_owned(),
            ..Config::default()
        };
        store.save(&original).expect("seed a good config");
        let bytes = fs::read(store.path()).expect("read the seeded file");

        let held = hold_exclusively(store.path());

        // The typed read fails and says why, instead of pretending the file is empty.
        let error = store.load_checked().expect_err("a held file is unreadable");
        assert!(
            matches!(error, ConfigError::Unreadable { .. }),
            "expected Unreadable, got {error:?}"
        );
        assert!(
            error.to_string().contains("cannot be read"),
            "the message must be actionable: {error}"
        );

        // The convenience shortcut still hands out defaults, but it poisons the store: a save is now
        // refused, so the good file cannot be replaced by those defaults.
        assert_eq!(store.load(), Config::default());
        let refused = store.save(&Config::default());
        assert!(
            matches!(refused, Err(ConfigError::NotLoaded { .. })),
            "expected NotLoaded, got {refused:?}"
        );

        drop(held);

        assert_eq!(
            fs::read(store.path()).expect("read after the refused save"),
            bytes,
            "the user's file must be byte-for-byte untouched"
        );

        // Once the scanner lets go, the same store recovers and may write again.
        assert_eq!(store.load_checked().expect("load after release"), original);
        store.save(&Config::default()).expect("saving works again");
    }

    #[test]
    fn whole_numbers_are_written_the_way_the_c_sharp_writer_writes_them() {
        let (_dir, store) = temp_store();
        let config = Config {
            window: WindowSettings {
                left: Some(100.0),
                top: None,
                width: 1440.0,
                height: 1440.5,
                maximized: false,
            },
            ..Config::default()
        };

        store.save(&config).expect("save");
        let text = fs::read_to_string(store.path()).expect("read the file");

        assert!(text.contains(r#""width": 1440,"#), "{text}");
        assert!(text.contains(r#""height": 1440.5,"#), "{text}");
        assert!(text.contains(r#""left": 100,"#), "{text}");
        assert!(text.contains(r#""top": null"#), "{text}");

        let reloaded = store.load_checked().expect("load");
        assert_eq!(reloaded.window.width, 1440.0);
        assert_eq!(reloaded.window.height, 1440.5);
        assert_eq!(reloaded.window.left, Some(100.0));
        assert_eq!(reloaded, config);
    }

    #[test]
    fn integer_dimensions_load_as_floats() {
        let (_dir, store) = temp_store();
        fs::write(
            store.path(),
            r#"{"window": {"width": 1280, "height": 800}}"#,
        )
        .expect("write");

        let config = store.load_checked().expect("load");

        assert_eq!(config.window.width, 1280.0);
        assert_eq!(config.window.height, 800.0);
    }
}
