//! Day/night theme switching: a pure schedule model the supervisor can evaluate on a timer.
//!
//! The schedule file is `%LocalAppData%\ProjectNeb\theme-schedule.json`:
//!
//! ```json
//! {
//!   "enabled": true,
//!   "day": "nebula-glass",
//!   "night": "minimal-dark",
//!   "dayFrom": "08:00",
//!   "nightFrom": "20:00"
//! }
//! ```
//!
//! [`resolve`] is the whole rule and has no clock, file or allocator of its own: given a
//! [`TimeOfDay`] and a schedule it answers which theme the schedule wants now. [`should_switch`]
//! adds the comparison with the theme the shell already paints, which is the only thing a timer
//! needs to avoid posting `shell.reload` every minute:
//!
//! ```no_run
//! # use neb_core::theming::schedule::{self, ThemeSchedule, TimeOfDay};
//! # fn example(config: &neb_core::config::Config) -> Result<(), schedule::ScheduleError> {
//! let schedule = ThemeSchedule::load(&schedule::schedule_path())?;
//! if let Some(next) = schedule::should_switch(config.shell.theme(), TimeOfDay::now_local(), &schedule) {
//!     // Write `next` into `config.shell.themeId`, save, then post `shell.reload`.
//!     let _ = next;
//! }
//! # Ok(())
//! # }
//! ```
//!
//! The supervisor loop is deliberately not wired here: another slice owns that file. This module
//! only promises the rule, the clock and the file shape.
//!
//! Boundaries are half-open. With `dayFrom = 08:00` and `nightFrom = 20:00`, 08:00 is day, 19:59 is
//! day, 20:00 is night and 07:59 is night. A schedule whose `dayFrom` is later than its `nightFrom`
//! wraps: the day window runs from `dayFrom` through midnight to `nightFrom`. Equal boundaries mean
//! day all day, because a zero-length night has no switch to make.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::{is_valid_id, pretty_json, write_atomic};

/// Time the day theme takes over when the file does not say.
pub const DEFAULT_DAY_FROM: &str = "08:00";

/// Time the night theme takes over when the file does not say.
pub const DEFAULT_NIGHT_FROM: &str = "20:00";

/// Why a schedule could not be read, validated or written.
#[derive(Debug, thiserror::Error)]
pub enum ScheduleError {
    /// The file system refused the operation.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `read` or `write`.
        action: &'static str,
        /// File the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The document is not a usable schedule.
    #[error("invalid theme schedule: {0}")]
    Invalid(String),

    /// The document could not be rendered or parsed as JSON.
    #[error("theme schedule is not valid json: {0}")]
    Json(#[from] serde_json::Error),
}

/// A wall-clock time of day, in minutes since midnight.
///
/// The type exists so the schedule rule cannot accidentally accept 25:00 or compare a time with a
/// date. [`TimeOfDay::now_local`] is the only place that reads the operating system clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimeOfDay {
    minutes: u32,
}

impl TimeOfDay {
    /// Midnight.
    pub const MIDNIGHT: Self = Self { minutes: 0 };

    /// `hour:minute`, or `None` when either part is out of range.
    pub fn new(hour: u32, minute: u32) -> Option<Self> {
        (hour < 24 && minute < 60).then_some(Self {
            minutes: hour * 60 + minute,
        })
    }

    /// A minute of the day, `0..=1439`.
    pub fn from_minutes(minutes: u32) -> Option<Self> {
        (minutes < 24 * 60).then_some(Self { minutes })
    }

    /// Parses `HH:MM` (or `H:MM`); anything else is `None`.
    pub fn parse(text: &str) -> Option<Self> {
        let (hour, minute) = text.trim().split_once(':')?;
        if hour.is_empty() || minute.is_empty() || minute.contains(':') {
            return None;
        }

        Self::new(hour.parse().ok()?, minute.parse().ok()?)
    }

    /// The current local wall-clock time, which is what a day/night schedule is written against.
    #[cfg(windows)]
    pub fn now_local() -> Self {
        // SAFETY: `GetLocalTime` writes a `SYSTEMTIME` into its own return value and has no other
        // precondition; the two fields read below are always in the documented range.
        let local = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        let minutes = u32::from(local.wHour) * 60 + u32::from(local.wMinute);

        Self::from_minutes(minutes).unwrap_or(Self::MIDNIGHT)
    }

    /// Without a Win32 clock the UTC hour is the best available answer.
    #[cfg(not(windows))]
    pub fn now_local() -> Self {
        let utc = crate::time::UtcDateTime::now_utc();

        Self::new(utc.hour, utc.minute).unwrap_or(Self::MIDNIGHT)
    }

    /// Minutes since midnight, `0..=1439`.
    pub fn minutes(self) -> u32 {
        self.minutes
    }

    /// The hour, `0..=23`.
    pub fn hour(self) -> u32 {
        self.minutes / 60
    }

    /// The minute, `0..=59`.
    pub fn minute(self) -> u32 {
        self.minutes % 60
    }
}

impl fmt::Display for TimeOfDay {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:02}:{:02}", self.hour(), self.minute())
    }
}

/// The persisted day/night rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeSchedule {
    /// Whether the schedule does anything at all.
    pub enabled: bool,
    /// Theme id used during the day window.
    pub day: String,
    /// Theme id used during the night window.
    pub night: String,
    /// Start of the day window, `HH:MM`.
    pub day_from: String,
    /// Start of the night window, `HH:MM`.
    pub night_from: String,
}

impl Default for ThemeSchedule {
    fn default() -> Self {
        Self {
            enabled: false,
            day: "nebula-glass".to_owned(),
            night: "minimal-dark".to_owned(),
            day_from: DEFAULT_DAY_FROM.to_owned(),
            night_from: DEFAULT_NIGHT_FROM.to_owned(),
        }
    }
}

impl ThemeSchedule {
    /// Whether this schedule names usable ids and times.
    ///
    /// # Errors
    ///
    /// [`ScheduleError::Invalid`] naming the field that is wrong.
    pub fn validate(&self) -> Result<(), ScheduleError> {
        for (field, id) in [("day", &self.day), ("night", &self.night)] {
            if !is_valid_id(id) {
                return Err(ScheduleError::Invalid(format!(
                    "'{field}' is '{}', which is not a usable theme id",
                    id.trim()
                )));
            }
        }

        for (field, time) in [("dayFrom", &self.day_from), ("nightFrom", &self.night_from)] {
            if TimeOfDay::parse(time).is_none() {
                return Err(ScheduleError::Invalid(format!(
                    "'{field}' is '{time}', which is not HH:MM"
                )));
            }
        }

        Ok(())
    }

    /// The theme id this schedule wants at `now`, or `None` when it is disabled or unusable.
    pub fn resolve(&self, now: TimeOfDay) -> Option<&str> {
        resolve(now, self)
    }

    /// The theme id to switch to, or `None` when the current theme is already right.
    pub fn should_switch(&self, current_theme: &str, now: TimeOfDay) -> Option<&str> {
        should_switch(current_theme, now, self)
    }

    /// Reads a schedule file. A missing file is the ordinary first run and yields the disabled
    /// default; a present but invalid file is an error so the caller can log it and keep painting.
    ///
    /// # Errors
    ///
    /// [`ScheduleError::Io`] when the file exists but cannot be read, [`ScheduleError::Json`] when it
    /// is not JSON, [`ScheduleError::Invalid`] when a field is unusable.
    pub fn load(path: &Path) -> Result<Self, ScheduleError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(source) => return Err(io_error("read", path, source)),
        };
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

        if text.trim().is_empty() {
            return Ok(Self::default());
        }

        let schedule: Self = serde_json::from_str(text)?;
        schedule.validate()?;

        Ok(schedule)
    }

    /// Writes a schedule file atomically. The schedule must validate first.
    ///
    /// # Errors
    ///
    /// [`ScheduleError::Invalid`] when the schedule cannot be resolved, [`ScheduleError::Io`] when
    /// the file cannot be written, [`ScheduleError::Json`] when it cannot be rendered.
    pub fn save(&self, path: &Path) -> Result<(), ScheduleError> {
        self.validate()?;
        let value = serde_json::to_value(self)?;
        let text = pretty_json(&value)?;

        write_atomic(path, text.as_bytes()).map_err(|source| io_error("write", path, source))
    }
}

/// The schedule the supervisor reads.
pub fn schedule_path() -> PathBuf {
    crate::paths::theme_schedule_file()
}

/// The theme id `schedule` wants at `now`.
///
/// Pure: no clock, no disk, no allocation. A disabled schedule, an unparsable boundary or a blank
/// theme id all answer `None`, which means "leave the shell alone" rather than "fall back to a
/// guess".
pub fn resolve(now: TimeOfDay, schedule: &ThemeSchedule) -> Option<&str> {
    if !schedule.enabled {
        return None;
    }

    let day_from = TimeOfDay::parse(&schedule.day_from)?;
    let night_from = TimeOfDay::parse(&schedule.night_from)?;

    let day = if day_from < night_from {
        now >= day_from && now < night_from
    } else if night_from < day_from {
        // The day window wraps midnight, e.g. dayFrom 20:00 and nightFrom 08:00.
        now >= day_from || now < night_from
    } else {
        // A zero-length night: the day theme owns the whole day.
        true
    };

    let id = if day {
        schedule.day.trim()
    } else {
        schedule.night.trim()
    };

    (!id.is_empty()).then_some(id)
}

/// The theme id to switch to, or `None` when the current theme is already the scheduled one.
///
/// This is the helper a supervisor timer calls: it answers `Some(id)` only when a switch is worth a
/// config write and a `shell.reload`.
pub fn should_switch<'a>(
    current_theme: &str,
    now: TimeOfDay,
    schedule: &'a ThemeSchedule,
) -> Option<&'a str> {
    let target = resolve(now, schedule)?;

    (target != current_theme.trim()).then_some(target)
}

/// Builds a typed file-system error.
fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> ScheduleError {
    ScheduleError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn schedule() -> ThemeSchedule {
        ThemeSchedule {
            enabled: true,
            day: "nebula-glass".to_owned(),
            night: "minimal-dark".to_owned(),
            day_from: "08:00".to_owned(),
            night_from: "20:00".to_owned(),
        }
    }

    fn at(hour: u32, minute: u32) -> TimeOfDay {
        TimeOfDay::new(hour, minute).expect("a valid time")
    }

    #[test]
    fn the_day_window_is_half_open() {
        for (hour, minute, expected) in [
            (8, 0, "nebula-glass"),
            (12, 0, "nebula-glass"),
            (19, 59, "nebula-glass"),
            (20, 0, "minimal-dark"),
            (23, 59, "minimal-dark"),
        ] {
            assert_eq!(resolve(at(hour, minute), &schedule()), Some(expected));
        }
    }

    #[test]
    fn before_the_day_starts_the_night_theme_is_used() {
        for (hour, minute) in [(0, 0), (3, 30), (7, 59)] {
            assert_eq!(
                resolve(at(hour, minute), &schedule()),
                Some("minimal-dark"),
                "{hour:02}:{minute:02}"
            );
        }
    }

    #[test]
    fn the_schedule_resolves_across_midnight() {
        let mut late = schedule();
        late.night_from = "22:00".to_owned();

        assert_eq!(resolve(at(23, 59), &late), Some("minimal-dark"));
        assert_eq!(resolve(at(0, 0), &late), Some("minimal-dark"));
        assert_eq!(resolve(at(21, 59), &late), Some("nebula-glass"));
    }

    #[test]
    fn a_wrapped_day_window_is_understood() {
        let wrapped = ThemeSchedule {
            enabled: true,
            day: "minimal-dark".to_owned(),
            night: "nebula-glass".to_owned(),
            day_from: "20:00".to_owned(),
            night_from: "08:00".to_owned(),
        };

        assert_eq!(resolve(at(21, 0), &wrapped), Some("minimal-dark"));
        assert_eq!(resolve(at(2, 0), &wrapped), Some("minimal-dark"));
        assert_eq!(resolve(at(12, 0), &wrapped), Some("nebula-glass"));
    }

    #[test]
    fn equal_boundaries_mean_day_all_day() {
        let mut equal = schedule();
        equal.night_from = "08:00".to_owned();

        assert_eq!(resolve(at(0, 0), &equal), Some("nebula-glass"));
        assert_eq!(resolve(at(23, 59), &equal), Some("nebula-glass"));
    }

    #[test]
    fn a_disabled_schedule_never_switches() {
        let mut disabled = schedule();
        disabled.enabled = false;

        assert_eq!(resolve(TimeOfDay::MIDNIGHT, &disabled), None);
        assert_eq!(resolve(at(12, 0), &disabled), None);
        assert_eq!(should_switch("nebula-glass", at(12, 0), &disabled), None);
    }

    #[test]
    fn an_unusable_schedule_answers_none_instead_of_guessing() {
        for (day_from, night_from) in [("25:00", "20:00"), ("08:00", "nope"), ("", "")] {
            let mut broken = schedule();
            broken.day_from = day_from.to_owned();
            broken.night_from = night_from.to_owned();

            assert!(broken.validate().is_err(), "{day_from}/{night_from}");
            assert_eq!(resolve(at(12, 0), &broken), None);
        }

        let mut blank = schedule();
        blank.day.clear();
        assert_eq!(resolve(at(12, 0), &blank), None);
    }

    #[test]
    fn should_switch_answers_only_when_the_theme_differs() {
        assert_eq!(
            should_switch("nebula-glass", at(12, 0), &schedule()),
            None,
            "already the day theme"
        );
        assert_eq!(
            should_switch("minimal-dark", at(12, 0), &schedule()),
            Some("nebula-glass")
        );
        assert_eq!(
            should_switch("nebula-glass", at(22, 0), &schedule()),
            Some("minimal-dark")
        );
        assert_eq!(
            should_switch(" nebula-glass ", at(12, 0), &schedule()),
            None,
            "surrounding spaces are not a different theme"
        );
    }

    #[test]
    fn time_of_day_parses_formats_and_rejects_nonsense() {
        assert_eq!(
            TimeOfDay::parse("08:00").map(|time| time.minutes()),
            Some(480)
        );
        assert_eq!(
            TimeOfDay::parse("8:05").map(|time| time.minutes()),
            Some(485)
        );
        assert_eq!(
            TimeOfDay::parse("23:59").map(|time| time.to_string()),
            Some("23:59".to_owned())
        );
        assert_eq!(TimeOfDay::parse("24:00"), None);
        assert_eq!(TimeOfDay::parse("08:60"), None);
        assert_eq!(TimeOfDay::parse("08"), None);
        assert_eq!(TimeOfDay::parse("08:00:00"), None);
        assert_eq!(TimeOfDay::from_minutes(1440), None);
        assert!(TimeOfDay::now_local().minutes() < 1440);
    }

    #[test]
    fn the_schedule_round_trips_through_its_json_shape() {
        let text = serde_json::to_string(&schedule()).expect("render");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");

        assert_eq!(value["enabled"], serde_json::json!(true));
        assert_eq!(value["day"], serde_json::json!("nebula-glass"));
        assert_eq!(value["night"], serde_json::json!("minimal-dark"));
        assert_eq!(value["dayFrom"], serde_json::json!("08:00"));
        assert_eq!(value["nightFrom"], serde_json::json!("20:00"));

        let back: ThemeSchedule = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, schedule());
    }

    #[test]
    fn a_missing_file_loads_the_disabled_default() {
        let dir = tempdir().expect("temp dir");

        let loaded = ThemeSchedule::load(&dir.path().join("absent.json")).expect("load");

        assert_eq!(loaded, ThemeSchedule::default());
        assert!(!loaded.enabled);
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("theme-schedule.json");

        schedule().save(&path).expect("save");
        let loaded = ThemeSchedule::load(&path).expect("load");

        assert_eq!(loaded, schedule());
        assert!(loaded.resolve(at(12, 0)).is_some());
    }

    #[test]
    fn saving_an_invalid_schedule_is_refused() {
        let dir = tempdir().expect("temp dir");
        let mut broken = schedule();
        broken.day_from = "tomorrow".to_owned();

        assert!(matches!(
            broken.save(&dir.path().join("theme-schedule.json")),
            Err(ScheduleError::Invalid(_))
        ));
        assert!(!dir.path().join("theme-schedule.json").exists());
    }

    #[test]
    fn a_broken_file_is_reported_not_silently_replaced() {
        let dir = tempdir().expect("temp dir");
        let path = dir.path().join("theme-schedule.json");
        fs::write(&path, "{ nope").expect("write");

        assert!(matches!(
            ThemeSchedule::load(&path),
            Err(ScheduleError::Json(_))
        ));
    }

    #[test]
    fn validation_names_the_bad_field() {
        let mut broken = schedule();
        broken.night = "../escape".to_owned();

        match broken.validate() {
            Err(ScheduleError::Invalid(message)) => {
                assert!(message.contains("night"), "{message}");
            }
            other => panic!("expected an invalid schedule, got {other:?}"),
        }
    }

    #[test]
    fn the_default_path_comes_from_the_data_root() {
        assert_eq!(schedule_path(), crate::paths::theme_schedule_file());
    }
}
