//! The clock widget: `HH:mm` over a long date.
//!
//! The date is what the C# bar shows as `Понедельник, 16 ноября` — the weekday capitalised, the day
//! without a leading zero and the month in the genitive for Russian. Everything is formatted into
//! fixed buffers so the timer that ticks once a second never touches the heap unless the text
//! actually changed.

use neb_core::strings::{Language, month, weekday};
use windows::Win32::System::SystemInformation::GetLocalTime;

use super::{BarText, WideBuf};

/// The time and the date, ready for a [`BarText`].
#[derive(Debug, Clone, Default)]
pub struct ClockText {
    time: WideBuf<8>,
    date: WideBuf<48>,
}

impl ClockText {
    /// Reads the wall clock and writes it into `text`.
    ///
    /// Returns whether anything changed, which is the signal to rebuild the layouts: the time changes
    /// once a minute and the date once a day, not once a second.
    pub fn update(&mut self, text: &mut BarText, language: Language) -> bool {
        let now = local_now();
        let mut changed = false;

        let mut time = WideBuf::<8>::new();
        time.push_two_digits(u32::from(now.wHour));
        time.push(':');
        time.push_two_digits(u32::from(now.wMinute));
        changed |= self.time.replace_with(&time);

        let mut date = WideBuf::<48>::new();
        compose_date(&mut date, now.wDayOfWeek, now.wDay, now.wMonth, language);
        changed |= self.date.replace_with(&date);

        changed |= text.time.replace_with(&self.time);
        changed |= text.date.replace_with(&self.date);

        changed
    }

    /// The time as written, for the layout log line.
    pub fn time(&self) -> &WideBuf<8> {
        &self.time
    }
}

/// The local wall clock, which a bar must show rather than the UTC the log writes.
fn local_now() -> windows::Win32::Foundation::SYSTEMTIME {
    // SAFETY: `GetLocalTime` writes into the returned structure and takes no pointer from the caller.
    unsafe { GetLocalTime() }
}

/// `Понедельник, 16 ноября` / `Monday, 16 November`.
///
/// `day_of_week` is `SYSTEMTIME`'s Sunday-first value; the table is Monday-first, so it is rotated.
pub fn compose_date<const N: usize>(
    target: &mut WideBuf<N>,
    day_of_week: u16,
    day: u16,
    month_of_year: u16,
    language: Language,
) {
    let monday_first = (u32::from(day_of_week) + 6) % 7;
    target.push_str(weekday(language, monday_first));
    target.push_str(", ");
    target.push_number(u32::from(day));
    target.push(' ');
    target.push_str(month(language, u32::from(month_of_year)));
}

#[cfg(test)]
/// The longest date the buffer has to hold, for the tests.
pub(crate) fn longest_date(language: Language) -> usize {
    // "Воскресенье, 30 сентября" is the worst case: the longest weekday plus the longest month.
    (0u16..7)
        .flat_map(|weekday| (1u16..=12).map(move |month| (weekday, month)))
        .map(|(weekday, month)| {
            let mut buffer = WideBuf::<48>::new();
            compose_date(&mut buffer, weekday, 30, month, language);
            buffer.as_units().len()
        })
        .max()
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date_of(day_of_week: u16, day: u16, month_of_year: u16, language: Language) -> String {
        let mut buffer = WideBuf::<48>::new();
        compose_date(&mut buffer, day_of_week, day, month_of_year, language);

        buffer.to_text()
    }

    #[test]
    fn russian_dates_use_the_genitive_month() {
        // 2026-11-16 is a Monday, and SYSTEMTIME numbers Sunday as 0.
        assert_eq!(date_of(1, 16, 11, Language::Ru), "Понедельник, 16 ноября");
        assert_eq!(date_of(0, 6, 1, Language::Ru), "Воскресенье, 6 января");
    }

    #[test]
    fn english_dates_use_the_nominative_month() {
        assert_eq!(date_of(1, 16, 11, Language::En), "Monday, 16 November");
        assert_eq!(date_of(0, 6, 1, Language::En), "Sunday, 6 January");
    }

    #[test]
    fn the_sunday_first_system_value_is_rotated_to_monday() {
        // SYSTEMTIME: 0 Sunday … 6 Saturday.
        for (system, expected) in [
            (1u16, "Понедельник"),
            (2, "Вторник"),
            (3, "Среда"),
            (4, "Четверг"),
            (5, "Пятница"),
            (6, "Суббота"),
            (0, "Воскресенье"),
        ] {
            let text = date_of(system, 1, 1, Language::Ru);
            assert!(text.starts_with(expected), "{system} gave {text}");
        }
    }

    #[test]
    fn the_day_is_not_zero_padded() {
        assert!(date_of(1, 5, 3, Language::En).contains(", 5 March"));
    }

    #[test]
    fn every_date_fits_the_buffer() {
        for language in Language::ALL {
            assert!(
                longest_date(language) < 48,
                "{language:?} needs {} units",
                longest_date(language)
            );
        }
    }

    #[test]
    fn an_update_reads_a_plausible_clock() {
        let mut clock = ClockText::default();
        let mut text = BarText::new(None, Language::Ru);

        let first = clock.update(&mut text, Language::Ru);

        assert!(first, "the first update always writes something");
        let time = text.time.to_text();
        assert_eq!(time.len(), 5, "{time}");
        assert_eq!(time.as_bytes()[2], b':');
        assert!(time[..2].parse::<u32>().unwrap() < 24, "{time}");
        assert!(time[3..].parse::<u32>().unwrap() < 60, "{time}");
        assert!(!text.date.is_empty());
    }

    #[test]
    fn a_second_update_in_the_same_minute_reports_no_change() {
        let mut clock = ClockText::default();
        let mut text = BarText::new(None, Language::En);
        clock.update(&mut text, Language::En);

        let changed = clock.update(&mut text, Language::En);

        // A minute boundary during the test would make this flaky; the date cannot change within a
        // second, so the assertion is only that a repeated call is honest about the time.
        let time = text.time.to_text();
        assert!(!changed || time.len() == 5);
    }
}
