//! Wall-clock helpers: a UTC breakdown of a [`SystemTime`] and an injectable clock.
//!
//! Both the config backup name and the log file name are derived from the wall clock, and both are
//! awkward in tests if the clock cannot move. [`Clock`] is the seam; [`SystemClock`] is what the
//! processes use.

use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

/// Source of "now". Implemented by [`SystemClock`] in production and by test clocks that jump days.
pub trait Clock: fmt::Debug + Send + Sync {
    /// The current instant.
    fn now(&self) -> SystemTime;
}

/// The operating system clock.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> SystemTime {
        SystemTime::now()
    }
}

/// A UTC calendar instant, broken down for file names and log lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcDateTime {
    /// Full year, e.g. `2026`.
    pub year: i32,
    /// Month, 1-12.
    pub month: u32,
    /// Day of month, 1-31.
    pub day: u32,
    /// Hour, 0-23.
    pub hour: u32,
    /// Minute, 0-59.
    pub minute: u32,
    /// Second, 0-59.
    pub second: u32,
    /// Millisecond, 0-999.
    pub millisecond: u32,
}

impl UtcDateTime {
    /// Breaks a [`SystemTime`] down into UTC fields.
    ///
    /// An instant before the epoch (a badly set machine clock) is clamped to the epoch with
    /// millisecond precision dropped: file names and log lines only need a plausible date.
    pub fn from_system_time(time: SystemTime) -> Self {
        let (seconds, millisecond) = match time.duration_since(UNIX_EPOCH) {
            Ok(after) => (
                i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
                after.subsec_millis(),
            ),
            Err(_) => (0, 0),
        };

        let days = seconds.div_euclid(86_400);
        let second_of_day = seconds.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);

        Self {
            year,
            month,
            day,
            hour: u32::try_from(second_of_day / 3_600).unwrap_or(0),
            minute: u32::try_from(second_of_day % 3_600 / 60).unwrap_or(0),
            second: u32::try_from(second_of_day % 60).unwrap_or(0),
            millisecond,
        }
    }

    /// Reads `clock`.
    pub fn now(clock: &dyn Clock) -> Self {
        Self::from_system_time(clock.now())
    }

    /// Reads the system clock.
    pub fn now_utc() -> Self {
        Self::from_system_time(SystemTime::now())
    }

    /// `YYYYMMDD`, the daily log file suffix.
    pub fn date_stamp(&self) -> String {
        format!("{:04}{:02}{:02}", self.year, self.month, self.day)
    }

    /// `YYYYMMDD-HHMMSS`, the suffix a corrupt config backup gets.
    pub fn backup_stamp(&self) -> String {
        format!(
            "{:04}{:02}{:02}-{:02}{:02}{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }

    /// RFC 3339 in UTC with millisecond precision, e.g. `2026-11-17T16:15:30.123Z`.
    pub fn iso8601(&self) -> String {
        format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
            self.year, self.month, self.day, self.hour, self.minute, self.second, self.millisecond
        )
    }

    /// Whether two instants fall on the same UTC day, which is when the log file changes.
    pub fn same_date(&self, other: &Self) -> bool {
        (self.year, self.month, self.day) == (other.year, other.month, other.day)
    }
}

/// Civil date from days since 1970-01-01, using Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i32, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = if month <= 2 { year + 1 } else { year };

    (
        i32::try_from(year).unwrap_or(1970),
        u32::try_from(month).unwrap_or(1),
        u32::try_from(day).unwrap_or(1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    #[derive(Debug)]
    struct FixedClock(Mutex<SystemTime>);

    impl FixedClock {
        fn new(seconds: u64) -> Self {
            Self(Mutex::new(UNIX_EPOCH + Duration::from_secs(seconds)))
        }
    }

    impl Clock for FixedClock {
        fn now(&self) -> SystemTime {
            *self.0.lock().expect("clock lock")
        }
    }

    fn at(seconds: u64) -> UtcDateTime {
        UtcDateTime::from_system_time(UNIX_EPOCH + Duration::from_secs(seconds))
    }

    #[test]
    fn epoch_is_the_first_of_january_1970() {
        let date = at(0);

        assert_eq!((date.year, date.month, date.day), (1970, 1, 1));
        assert_eq!(date.iso8601(), "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn known_instants_break_down_correctly() {
        assert_eq!(at(1_000_000_000).iso8601(), "2001-09-09T01:46:40.000Z");
        assert_eq!(at(1_700_000_000).iso8601(), "2023-11-14T22:13:20.000Z");
    }

    #[test]
    fn leap_day_is_a_leap_day() {
        let date = at(951_782_400);

        assert_eq!((date.year, date.month, date.day), (2000, 2, 29));
        assert_eq!(date.date_stamp(), "20000229");
    }

    #[test]
    fn milliseconds_survive_the_breakdown() {
        let date = UtcDateTime::from_system_time(
            UNIX_EPOCH + Duration::from_secs(1_700_000_000) + Duration::from_millis(123),
        );

        assert_eq!(date.millisecond, 123);
        assert_eq!(date.iso8601(), "2023-11-14T22:13:20.123Z");
        assert_eq!(date.backup_stamp(), "20231114-221320");
    }

    #[test]
    fn same_date_compares_the_calendar_day_only() {
        let start = at(1_700_000_000);
        let later =
            UtcDateTime::from_system_time(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + 3_600));
        let tomorrow =
            UtcDateTime::from_system_time(UNIX_EPOCH + Duration::from_secs(1_700_000_000 + 86_400));

        assert!(start.same_date(&later));
        assert!(!start.same_date(&tomorrow));
    }

    #[test]
    fn the_clock_trait_is_what_now_reads() {
        let clock = FixedClock::new(1_700_000_000);

        assert_eq!(
            UtcDateTime::now(&clock).iso8601(),
            "2023-11-14T22:13:20.000Z"
        );
    }
}
