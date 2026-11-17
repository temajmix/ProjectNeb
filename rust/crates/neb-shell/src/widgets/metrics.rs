//! The metrics widget: `ЦП 12%  ОЗУ 38%` (or `CPU 12%  RAM 38%`).
//!
//! A sample arrives on the UI thread as a packed pair of percentages, so the whole widget is a
//! formatting step into a fixed buffer. An unavailable reading shows an em dash, which is what the
//! C# bar does rather than printing a misleading `0%`.

use neb_core::strings::{Language, Text, get};

use super::{BarText, WideBuf};

/// What one sample says, already rounded for display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Reading {
    /// CPU busy share, `0..=100`; `None` when the sampler could not read it.
    pub cpu: Option<u32>,
    /// Physical memory in use, `0..=100`; `None` when the sampler could not read it.
    pub ram: Option<u32>,
}

impl Reading {
    /// Unpacks the two percentages a metrics message carries.
    ///
    /// Each half is `0..=100`, or [`UNKNOWN`] for "the sampler could not read it".
    pub fn from_packed(cpu: u32, ram: u32) -> Self {
        Self {
            cpu: (cpu != UNKNOWN).then_some(cpu.min(100)),
            ram: (ram != UNKNOWN).then_some(ram.min(100)),
        }
    }

    /// Packs the pair the way the metrics thread posts it.
    pub fn pack(self) -> (u32, u32) {
        (self.cpu.unwrap_or(UNKNOWN), self.ram.unwrap_or(UNKNOWN))
    }
}

/// The value that means "no reading", one past the last percentage.
pub const UNKNOWN: u32 = 101;

/// The placeholder drawn for a missing reading.
const DASH: char = '—';

/// The `CPU …  RAM …` line, ready for a [`BarText`].
#[derive(Debug, Clone, Default)]
pub struct MetricsText {
    buffer: WideBuf<32>,
}

impl MetricsText {
    /// Writes `reading` into `text`, and reports whether the line changed.
    pub fn update(&mut self, text: &mut BarText, reading: Reading, language: Language) -> bool {
        let mut buffer = WideBuf::<32>::new();
        compose(&mut buffer, reading, language);

        self.buffer.replace_with(&buffer) && text.metrics.replace_with(&self.buffer)
    }

    /// The line as written, for the log and the tests.
    pub fn line(&self) -> String {
        self.buffer.to_text()
    }
}

/// Formats one line: both labels and both percentages, or a dash for what could not be read.
pub fn compose(target: &mut WideBuf<32>, reading: Reading, language: Language) {
    target.push_str(get(language, Text::BarCpu));
    target.push(' ');
    push_percent(target, reading.cpu);
    target.push_str("  ");
    target.push_str(get(language, Text::BarRam));
    target.push(' ');
    push_percent(target, reading.ram);
}

/// `12%`, or the dash when the reading is missing.
fn push_percent(target: &mut WideBuf<32>, value: Option<u32>) {
    match value {
        Some(percent) => {
            target.push_number(percent.min(100));
            target.push('%');
        }
        None => target.push(DASH),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(reading: Reading, language: Language) -> String {
        let mut buffer = WideBuf::<32>::new();
        compose(&mut buffer, reading, language);

        buffer.to_text()
    }

    #[test]
    fn the_line_reads_like_the_c_sharp_bar() {
        let reading = Reading {
            cpu: Some(12),
            ram: Some(38),
        };

        assert_eq!(line(reading, Language::En), "CPU 12%  RAM 38%");
        assert_eq!(line(reading, Language::Ru), "ЦП 12%  ОЗУ 38%");
    }

    #[test]
    fn a_missing_reading_is_a_dash_rather_than_zero() {
        let reading = Reading {
            cpu: None,
            ram: Some(4),
        };

        assert_eq!(line(reading, Language::En), "CPU —  RAM 4%");
    }

    #[test]
    fn percentages_are_clamped_to_a_hundred() {
        let reading = Reading {
            cpu: Some(250),
            ram: Some(0),
        };

        assert_eq!(line(reading, Language::En), "CPU 100%  RAM 0%");
    }

    #[test]
    fn a_packed_pair_round_trips_and_carries_the_unknown_marker() {
        let reading = Reading {
            cpu: Some(7),
            ram: None,
        };
        let (cpu, ram) = reading.pack();

        assert_eq!(Reading::from_packed(cpu, ram), reading);
        assert_eq!(Reading::from_packed(0, 100).cpu, Some(0));
        assert_eq!(Reading::from_packed(0, 100).ram, Some(100));
        assert_eq!(Reading::from_packed(101, 5).cpu, None);
        assert_eq!(Reading::from_packed(300, 5).cpu, Some(100));
    }

    #[test]
    fn an_update_reports_only_a_real_change() {
        let mut widget = MetricsText::default();
        let mut text = BarText::new(None, Language::En);
        let reading = Reading {
            cpu: Some(1),
            ram: Some(2),
        };

        assert!(widget.update(&mut text, reading, Language::En));
        assert!(!widget.update(&mut text, reading, Language::En));
        assert_eq!(text.metrics.to_text(), "CPU 1%  RAM 2%");
        assert_eq!(widget.line(), "CPU 1%  RAM 2%");
    }
}
