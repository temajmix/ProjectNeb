//! The application log: one file per day, capped, trimmed, written by hand.
//!
//! No logging framework is involved — the whole writer is below — because a resident shell cannot
//! afford a dependency that allocates per event or spawns a worker thread. The shape follows the C#
//! build (`Neb.Core.Logging.NebLog`) so both versions write the same directory:
//!
//! - `neb-YYYYMMDD.log` (UTC), one file per day;
//! - a file that reaches [`MAX_FILE_BYTES`] is renamed to `neb-YYYYMMDD_001.log` and started over
//!   (the Serilog shape the C# build writes, so both builds' rolls share one retention window);
//! - only the newest [`MAX_FILES`] files are kept;
//! - a line reads `ts LEVEL module message`;
//! - [`Level::Debug`] is hidden unless `PROJECTNEB_LOG_LEVEL` (or [`LogHandle::set_level`]) says
//!   otherwise.
//!
//! Call sites that format arguments should use the `log_info!`-style macros: they test the level
//! before `format!` runs, so a filtered line allocates nothing.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use crate::time::{Clock, SystemClock, UtcDateTime};

/// Size at which the current file is rolled to `.1`, in bytes.
pub const MAX_FILE_BYTES: u64 = 5_000_000;

/// How many files a log directory keeps.
pub const MAX_FILES: usize = 7;

/// Environment variable that lowers the level for a debugging session, as in the C# build.
pub const LEVEL_VARIABLE: &str = "PROJECTNEB_LOG_LEVEL";

/// Prefix and suffix of a log file, `neb-YYYYMMDD[.N].log`.
const FILE_PREFIX: &str = "neb-";
const FILE_SUFFIX: &str = ".log";

/// Severity of a line. Declaration order is the filter order: a level is written when it is at least
/// the configured minimum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Per-keystroke detail, hidden by default.
    Debug,
    /// What happened, in order: the default minimum.
    Info,
    /// Something was recovered from, or is about to degrade.
    Warn,
    /// Something failed.
    Error,
}

impl Level {
    /// The label written into the line.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "DEBUG",
            Self::Info => "INFO",
            Self::Warn => "WARN",
            Self::Error => "ERROR",
        }
    }

    /// Reads a level name; accepts the C# names (`Information`, `Warning`, `Verbose`) as well as the
    /// short ones, case-insensitively.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "debug" | "verbose" => Some(Self::Debug),
            "info" | "information" => Some(Self::Info),
            "warn" | "warning" => Some(Self::Warn),
            "error" | "fatal" => Some(Self::Error),
            _ => None,
        }
    }

    /// The level named by [`LEVEL_VARIABLE`], or [`Level::Info`].
    fn from_env() -> Self {
        std::env::var(LEVEL_VARIABLE)
            .ok()
            .as_deref()
            .and_then(Self::parse)
            .unwrap_or(Self::Info)
    }

    fn code(self) -> u8 {
        self as u8
    }

    fn from_code(code: u8) -> Self {
        match code {
            0 => Self::Debug,
            1 => Self::Info,
            3 => Self::Error,
            _ => Self::Warn,
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// What went wrong while starting the log.
#[derive(Debug, thiserror::Error)]
pub enum LogError {
    /// The directory or the current file could not be opened.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `open` or `roll`.
        action: &'static str,
        /// File or directory the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}

/// A handle to the log. Cloning shares the same state and file, so it can be handed to every module
/// without a global.
#[derive(Debug, Clone)]
pub struct LogHandle {
    inner: Arc<LogInner>,
}

#[derive(Debug)]
struct LogInner {
    dir: PathBuf,
    /// [`Level::code`] of the current minimum.
    level: AtomicU8,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

/// The open file and where in the day it sits.
#[derive(Debug, Default)]
struct State {
    file: Option<File>,
    date: Option<UtcDateTime>,
    bytes: u64,
}

/// Starts logging into `dir` at the level named by [`LEVEL_VARIABLE`].
///
/// # Errors
///
/// [`LogError::Io`] when the directory cannot be created or today's file cannot be opened.
pub fn init(dir: &Path) -> Result<LogHandle, LogError> {
    init_with(dir, Level::from_env(), Arc::new(SystemClock))
}

/// Starts logging with an explicit level and clock; the clock is the test seam for day changes.
///
/// # Errors
///
/// [`LogError::Io`] when the directory cannot be created or today's file cannot be opened.
pub fn init_with(dir: &Path, level: Level, clock: Arc<dyn Clock>) -> Result<LogHandle, LogError> {
    fs::create_dir_all(dir).map_err(|source| io_error("create", dir, source))?;

    let handle = LogHandle {
        inner: Arc::new(LogInner {
            dir: dir.to_path_buf(),
            level: AtomicU8::new(level.code()),
            clock,
            state: Mutex::new(State::default()),
        }),
    };

    let now = UtcDateTime::now(handle.inner.clock.as_ref());
    handle.lock_state().open_day(dir, now)?;
    // Trim after today's file exists, so the file being written is never a candidate for removal.
    trim(dir);

    Ok(handle)
}

impl LogHandle {
    /// Directory the log writes into.
    pub fn dir(&self) -> &Path {
        &self.inner.dir
    }

    /// The current minimum level.
    pub fn level(&self) -> Level {
        Level::from_code(self.inner.level.load(Ordering::Relaxed))
    }

    /// Changes the minimum level, e.g. after reading a debug switch.
    pub fn set_level(&self, level: Level) {
        self.inner.level.store(level.code(), Ordering::Relaxed);
    }

    /// Whether a line at `level` would be written. Lets a caller skip building a message.
    pub fn enabled(&self, level: Level) -> bool {
        level.code() >= self.inner.level.load(Ordering::Relaxed)
    }

    /// Writes a [`Level::Debug`] line.
    pub fn debug(&self, module: &str, message: &str) {
        self.log(Level::Debug, module, message);
    }

    /// Writes a [`Level::Info`] line.
    pub fn info(&self, module: &str, message: &str) {
        self.log(Level::Info, module, message);
    }

    /// Writes a [`Level::Warn`] line.
    pub fn warn(&self, module: &str, message: &str) {
        self.log(Level::Warn, module, message);
    }

    /// Writes a [`Level::Error`] line.
    pub fn error(&self, module: &str, message: &str) {
        self.log(Level::Error, module, message);
    }

    /// Writes one line, if `level` passes the filter.
    ///
    /// A line that cannot be written is dropped: the process is already in trouble and the log is
    /// where it would have been reported, so failing here would only lose the original problem too.
    pub fn log(&self, level: Level, module: &str, message: &str) {
        if !self.enabled(level) {
            return;
        }

        let now = UtcDateTime::now(self.inner.clock.as_ref());
        let mut line = String::with_capacity(48 + module.len() + message.len());
        line.push_str(&now.iso8601());
        line.push(' ');
        line.push_str(level.as_str());
        line.push(' ');
        push_sanitised(&mut line, module);
        line.push(' ');
        push_sanitised(&mut line, message);
        line.push('\n');

        let mut state = self.lock_state();
        let _ = state.write_line(&self.inner.dir, now, &line);
    }

    /// Locks the state, recovering from a poison: a panic while logging must not silence the log.
    fn lock_state(&self) -> MutexGuard<'_, State> {
        self.inner
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl State {
    /// Appends one line, switching file when the day changed or the current one is full.
    fn write_line(&mut self, dir: &Path, now: UtcDateTime, line: &str) -> Result<(), LogError> {
        let new_day = self.date.is_none_or(|date| !date.same_date(&now));
        if new_day {
            self.open_day(dir, now)?;
            // A new day changes the set of retained files, not just the one being written.
            trim(dir);
        } else if self.bytes + line.len() as u64 > MAX_FILE_BYTES {
            // A failed roll reopens the current file and reports itself on stderr, so this line still
            // lands somewhere; `roll` resets the byte count, which is what stops every later line from
            // retrying the same rename.
            if self.roll(dir, now).is_err() && self.file.is_none() {
                self.open_day(dir, now)?;
            }
        }

        if self.file.is_none() {
            // Recovery after an open that failed earlier, and the guard that keeps a failed roll from
            // silencing the log: try again on the next line rather than giving up until midnight.
            self.open_day(dir, now)?;
        }

        let Some(file) = self.file.as_mut() else {
            // open_day above either opened a file or returned an error, so this is unreachable
            // without a logic change; dropping the line is still better than a panic in logging.
            return Ok(());
        };

        file.write_all(line.as_bytes())
            .and_then(|()| file.flush())
            .map_err(|source| io_error("write", dir, source))?;
        self.bytes += line.len() as u64;
        Ok(())
    }

    /// Opens (or re-opens) today's file for appending.
    fn open_day(&mut self, dir: &Path, now: UtcDateTime) -> Result<(), LogError> {
        self.file = None;
        let path = dir.join(file_name(now, None));

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|source| io_error("open", &path, source))?;
        self.bytes = file.metadata().map(|meta| meta.len()).unwrap_or(0);
        self.file = Some(file);
        self.date = Some(now);

        // A file that survived a restart may already be past the cap. A failed roll keeps this file
        // open, so reaching the cap is not a reason to refuse to log.
        if self.bytes >= MAX_FILE_BYTES {
            let _ = self.roll(dir, now);
        }

        Ok(())
    }

    /// Moves the current file to `_001` and starts a fresh one.
    ///
    /// On failure the current file is reopened in append mode and the byte count is reset, so logging
    /// continues past the cap instead of dropping every line until the date changes. The failure is
    /// reported once on stderr — the log itself is what is failing, so it cannot report this.
    fn roll(&mut self, dir: &Path, now: UtcDateTime) -> Result<(), LogError> {
        self.file = None;

        let current = dir.join(file_name(now, None));
        let rolled = dir.join(file_name(now, Some(1)));

        if let Err(source) = fs::rename(&current, &rolled) {
            eprintln!(
                "neb-core: log roll failed ({source}); continuing in {}",
                current.display()
            );
            if let Ok(file) = OpenOptions::new().append(true).open(&current) {
                self.file = Some(file);
                // Without this the next line would try the same rename again, and the line after that,
                // for the rest of the day.
                self.bytes = 0;
            }
            return Err(io_error("roll", &current, source));
        }

        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&current)
            .map_err(|source| io_error("open", &current, source))?;
        self.bytes = 0;
        self.file = Some(file);

        Ok(())
    }
}

/// Deletes the oldest log files once there are more than [`MAX_FILES`].
///
/// Best effort by design: a file another process still holds open must not stop this process from
/// starting, and the next start trims again.
fn trim(dir: &Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    let mut files: Vec<(String, u32, String)> = entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            log_file_order(&name).map(|(date, roll)| (date, roll, name))
        })
        .collect();

    if files.len() <= MAX_FILES {
        return;
    }

    // Newest day first, and within a day the plain file before its rolls: the file being written is
    // the last one a trim should take.
    files.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.cmp(&right.1))
            .then_with(|| left.2.cmp(&right.2))
    });
    for (_, _, stale) in &files[MAX_FILES..] {
        let _ = fs::remove_file(dir.join(stale));
    }
}

/// `(date, roll)` for a log file name, used as the retention order.
///
/// Accepts both the Serilog shape this writer uses (`neb-YYYYMMDD_001.log`) and the older
/// `neb-YYYYMMDD.1.log`, so files written by the C# build count towards the same retention window and
/// are trimmed by the same rule.
fn log_file_order(name: &str) -> Option<(String, u32)> {
    let rest = name.strip_prefix(FILE_PREFIX)?.strip_suffix(FILE_SUFFIX)?;

    let (date, roll) = match rest.split_once('_') {
        Some((date, roll)) => (date, Some(roll)),
        // Legacy shape, kept readable so old files are still trimmed.
        None => match rest.split_once('.') {
            Some((date, roll)) => (date, Some(roll)),
            None => (rest, None),
        },
    };

    let date_is_valid = date.len() == 8 && date.bytes().all(|byte| byte.is_ascii_digit());
    if !date_is_valid {
        return None;
    }

    match roll {
        None => Some((date.to_owned(), 0)),
        Some(roll) if !roll.is_empty() && roll.bytes().all(|byte| byte.is_ascii_digit()) => {
            Some((date.to_owned(), roll.parse().ok()?))
        }
        Some(_) => None,
    }
}

/// `neb-YYYYMMDD.log`, or `neb-YYYYMMDD_001.log` for a rolled file.
fn file_name(date: UtcDateTime, roll: Option<u32>) -> String {
    let stamp = date.date_stamp();
    match roll {
        Some(roll) => format!("{FILE_PREFIX}{stamp}_{roll:03}{FILE_SUFFIX}"),
        None => format!("{FILE_PREFIX}{stamp}{FILE_SUFFIX}"),
    }
}

/// Copies `value` into `target`, replacing control characters so one message cannot forge a second
/// log line and cannot smuggle terminal escapes into a console reader.
fn push_sanitised(target: &mut String, value: &str) {
    for character in value.chars() {
        target.push(if character.is_control() {
            ' '
        } else {
            character
        });
    }
}

/// Builds a [`LogError::Io`] for `path`.
fn io_error(action: &'static str, path: &Path, source: io::Error) -> LogError {
    LogError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

/// Writes a debug line, formatting the message only when the level is enabled.
#[macro_export]
macro_rules! log_debug {
    ($handle:expr, $module:expr, $($arg:tt)*) => {{
        let handle = &$handle;
        if handle.enabled($crate::logging::Level::Debug) {
            handle.debug($module, &format!($($arg)*));
        }
    }};
}

/// Writes an info line, formatting the message only when the level is enabled.
#[macro_export]
macro_rules! log_info {
    ($handle:expr, $module:expr, $($arg:tt)*) => {{
        let handle = &$handle;
        if handle.enabled($crate::logging::Level::Info) {
            handle.info($module, &format!($($arg)*));
        }
    }};
}

/// Writes a warning line, formatting the message only when the level is enabled.
#[macro_export]
macro_rules! log_warn {
    ($handle:expr, $module:expr, $($arg:tt)*) => {{
        let handle = &$handle;
        if handle.enabled($crate::logging::Level::Warn) {
            handle.warn($module, &format!($($arg)*));
        }
    }};
}

/// Writes an error line, formatting the message only when the level is enabled.
#[macro_export]
macro_rules! log_error {
    ($handle:expr, $module:expr, $($arg:tt)*) => {{
        let handle = &$handle;
        if handle.enabled($crate::logging::Level::Error) {
            handle.error($module, &format!($($arg)*));
        }
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tempfile::{TempDir, tempdir};

    /// 2023-11-14T22:13:20Z.
    const START: u64 = 1_700_000_000;

    #[derive(Debug)]
    struct FixedClock(AtomicU64);

    impl FixedClock {
        fn at(seconds: u64) -> Arc<Self> {
            Arc::new(Self(AtomicU64::new(seconds)))
        }

        fn advance(&self, seconds: u64) {
            self.0.fetch_add(seconds, Ordering::SeqCst);
        }
    }

    impl Clock for FixedClock {
        fn now(&self) -> SystemTime {
            UNIX_EPOCH + Duration::from_secs(self.0.load(Ordering::SeqCst))
        }
    }

    fn temp_log(clock: Arc<FixedClock>) -> (TempDir, LogHandle) {
        let dir = tempdir().expect("temp dir");
        let log = init_with(dir.path(), Level::Info, clock).expect("init logging");
        (dir, log)
    }

    fn read_log(dir: &Path, date: &str) -> String {
        fs::read_to_string(dir.join(format!("neb-{date}.log"))).expect("read log file")
    }

    fn log_files(dir: &Path) -> Vec<String> {
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

    #[test]
    fn a_line_is_written_and_read_back() {
        let (dir, log) = temp_log(FixedClock::at(START));

        log.info("config", "saved");

        assert_eq!(
            read_log(dir.path(), "20231114"),
            "2023-11-14T22:13:20.000Z INFO config saved\n"
        );
    }

    #[test]
    fn every_level_has_its_own_label() {
        let (dir, log) = temp_log(FixedClock::at(START));

        log.debug("m", "d");
        log.info("m", "i");
        log.warn("m", "w");
        log.error("m", "e");

        let text = read_log(dir.path(), "20231114");
        assert_eq!(text.lines().count(), 3, "debug is filtered at info: {text}");
        assert!(text.contains(" INFO m i\n"), "{text}");
        assert!(text.contains(" WARN m w\n"), "{text}");
        assert!(text.contains(" ERROR m e\n"), "{text}");
    }

    #[test]
    fn cyrillic_survives_the_round_trip() {
        let (dir, log) = temp_log(FixedClock::at(START));

        log.warn("профили", "не удалось прочитать «Ночь»");

        let text = read_log(dir.path(), "20231114");
        assert!(
            text.contains("WARN профили не удалось прочитать «Ночь»"),
            "{text}"
        );
        assert!(!text.contains(r"\u04"), "written as utf-8, not escapes");
    }

    #[test]
    fn a_new_day_starts_a_new_file() {
        let clock = FixedClock::at(START);
        let (dir, log) = temp_log(Arc::clone(&clock));

        log.info("m", "first");
        clock.advance(86_400);
        log.info("m", "second");

        let yesterday = read_log(dir.path(), "20231114");
        assert_eq!(yesterday.lines().count(), 1, "{yesterday}");
        assert!(yesterday.contains("first"));
        let today = read_log(dir.path(), "20231115");
        assert_eq!(today.lines().count(), 1, "{today}");
        assert!(today.contains("second"));
    }

    #[test]
    fn a_full_file_rolls_to_a_numbered_file() {
        let (dir, log) = temp_log(FixedClock::at(START));
        let big = "a".repeat(MAX_FILE_BYTES as usize + 1);

        log.info("m", &big);
        log.info("m", "after the roll");

        let rolled = read_log(dir.path(), "20231114_001");
        assert!(
            !rolled.contains("after the roll"),
            "the roll holds the old lines"
        );
        assert_eq!(
            rolled.len(),
            big.len() + "2023-11-14T22:13:20.000Z INFO m \n".len()
        );

        let current = read_log(dir.path(), "20231114");
        assert_eq!(current, "2023-11-14T22:13:20.000Z INFO m after the roll\n");
    }

    #[cfg(windows)]
    #[test]
    fn a_failed_roll_keeps_writing_into_the_current_file() {
        use std::os::windows::fs::OpenOptionsExt;

        let (dir, log) = temp_log(FixedClock::at(START));

        // Fill the file past the cap, so the next line wants a roll.
        log.info("m", &"a".repeat(MAX_FILE_BYTES as usize));

        // Hold the roll target the way a reader that shares nothing would.
        let rolled = dir.path().join("neb-20231114_001.log");
        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&rolled)
            .expect("hold the roll target");
        assert!(rolled.exists(), "the roll target is taken");

        log.info("m", "after the failed roll");
        log.info("m", "and the line after that");

        let current = read_log(dir.path(), "20231114");
        assert!(current.contains("after the failed roll"), "{current}");
        assert!(
            current.contains("and the line after that"),
            "a failed roll must not silence logging: {current}"
        );

        // The byte count was reset by the failure, so the writer does not retry the rename for
        // another 5 MB even though the cap is still exceeded.
        drop(held);
        log.info("m", "still writing after the failure");
        assert!(
            read_log(dir.path(), "20231114").contains("still writing after the failure"),
            "logging continues in the same file"
        );
    }

    #[test]
    fn retention_keeps_the_newest_day_and_drops_rolls_first() {
        let dir = tempdir().expect("temp dir");
        for day in 1..=4 {
            fs::write(dir.path().join(format!("neb-202612{day:02}.log")), "x").expect("seed");
            fs::write(dir.path().join(format!("neb-202612{day:02}_001.log")), "x")
                .expect("seed a roll");
        }

        trim(dir.path());

        let names = log_files(dir.path());
        assert_eq!(names.len(), MAX_FILES, "{names:?}");
        assert!(names.contains(&"neb-20261204.log".to_owned()), "{names:?}");
        assert!(
            names.contains(&"neb-20261204_001.log".to_owned()),
            "rolls count towards retention: {names:?}"
        );
        assert!(
            !names.contains(&"neb-20261201_001.log".to_owned()),
            "the oldest roll goes first, never the file being written: {names:?}"
        );
        assert!(names.contains(&"neb-20261201.log".to_owned()), "{names:?}");
    }

    #[test]
    fn a_day_change_trims_the_directory_too() {
        let clock = FixedClock::at(START);
        let (dir, log) = temp_log(Arc::clone(&clock));
        for day in 1..=6 {
            fs::write(dir.path().join(format!("neb-202311{day:02}.log")), "old").expect("seed");
        }
        fs::write(dir.path().join("neb-20231031.log"), "old").expect("seed the oldest");

        clock.advance(86_400);
        log.info("m", "the next day");

        let names = log_files(dir.path());
        assert_eq!(names.len(), MAX_FILES, "{names:?}");
        assert!(
            !names.contains(&"neb-20231031.log".to_owned()),
            "the day change must trim, not only init: {names:?}"
        );
    }

    #[test]
    fn trimming_keeps_seven_files() {
        let dir = tempdir().expect("temp dir");
        for day in 1..=10 {
            fs::write(dir.path().join(format!("neb-202611{day:02}.log")), "old").expect("seed");
        }

        // A day newer than every seeded file, so trimming has something to prefer.
        let clock: Arc<dyn Clock> = FixedClock::at(1_794_816_000);
        let _log = init_with(dir.path(), Level::Info, clock.clone()).expect("init");

        let names = log_files(dir.path());
        assert_eq!(names.len(), MAX_FILES, "{names:?}");
        assert!(names.contains(&"neb-20261110.log".to_owned()), "{names:?}");
        assert!(!names.contains(&"neb-20261101.log".to_owned()), "{names:?}");
        let today = UtcDateTime::from_system_time(clock.now()).date_stamp();
        assert!(names.contains(&format!("neb-{today}.log")), "{names:?}");
    }

    #[test]
    fn init_creates_the_directory_it_is_pointed_at() {
        let dir = tempdir().expect("temp dir");
        let nested = dir.path().join("a").join("b");

        let _log = init_with(&nested, Level::Info, FixedClock::at(START)).expect("init");

        assert!(nested.is_dir());
    }

    #[test]
    fn a_disabled_level_is_not_written_until_it_is_enabled() {
        let (dir, log) = temp_log(FixedClock::at(START));

        assert!(!log.enabled(Level::Debug));
        log.debug("m", "quiet");
        assert_eq!(
            read_log(dir.path(), "20231114"),
            "",
            "a filtered line writes nothing"
        );

        log.set_level(Level::Debug);
        log.debug("m", "loud");

        assert!(log.enabled(Level::Debug));
        assert!(read_log(dir.path(), "20231114").contains("DEBUG m loud"));
    }

    #[test]
    fn a_newline_in_a_message_cannot_forge_a_line() {
        let (dir, log) = temp_log(FixedClock::at(START));

        log.info("m", "one\ntwo\r\nthree");

        let text = read_log(dir.path(), "20231114");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("one two  three"), "{text}");
    }

    #[test]
    fn level_names_parse_like_the_c_sharp_build() {
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse("Information"), Some(Level::Info));
        assert_eq!(Level::parse("WARNING"), Some(Level::Warn));
        assert_eq!(Level::parse("Error"), Some(Level::Error));
        assert_eq!(Level::parse("verbose"), Some(Level::Debug));
        assert_eq!(Level::parse("chatty"), None);
        assert_eq!(Level::Info.to_string(), "INFO");
    }

    #[test]
    fn the_macros_format_only_when_the_level_passes() {
        let (dir, log) = temp_log(FixedClock::at(START));

        crate::log_debug!(log, "m", "hidden {}", 1);
        crate::log_info!(log, "m", "shown {}", 2);

        let text = read_log(dir.path(), "20231114");
        assert_eq!(text.lines().count(), 1, "{text}");
        assert!(text.contains("shown 2"), "{text}");
    }

    #[test]
    fn only_log_file_names_are_trimmed() {
        let recognised = |name: &str| log_file_order(name).is_some();

        assert!(recognised("neb-20261117.log"));
        assert!(recognised("neb-20261117_001.log"));
        assert!(
            recognised("neb-20261117.1.log"),
            "the older roll shape is still trimmed"
        );
        assert!(!recognised("neb-2026111.log"));
        assert!(!recognised("neb-notes.log"));
        assert!(!recognised("other-20261117.log"));
        assert!(!recognised("neb-20261117.txt"));
    }
}
