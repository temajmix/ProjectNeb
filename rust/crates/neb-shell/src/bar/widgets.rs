//! Configurable bar widgets: what the user asked for, and the text each one draws.
//!
//! The configuration is a list of loosely typed entries ([`BarWidget`]) because an older build must
//! load a file written by a newer one. [`parse`] turns that list into the typed [`WidgetSpec`] the
//! shell can paint, reporting every entry it had to skip so the caller can log it once per reload.
//!
//! Text is composed here, before a frame, into fixed UTF-16 buffers: [`BarText`] holds the shared
//! clock/metrics/profile readouts, and [`WidgetText`] is one widget's copy with its own format and
//! filters applied. Painting only reads the buffers.

use neb_core::config::BarWidget;
use neb_core::strings::{Language, Text, get};

use crate::widgets::metrics::Reading;
use crate::widgets::{WideBuf, clock};

/// How many widgets one bar will draw. More than this is a broken file, not a layout.
pub const MAX_WIDGETS: usize = 32;

/// Longest `text` widget string kept, in UTF-16 units.
pub const TEXT_UNITS: usize = 96;

/// Longest clock format accepted; anything longer is cut back to `HH:mm`.
pub const MAX_FORMAT: usize = 32;

/// A widget type the shell knows how to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidgetKind {
    /// The four-square launcher glyph.
    Launcher,
    /// The active profile's name.
    Profile,
    /// The time, formatted by the entry.
    Clock,
    /// The long date.
    Date,
    /// The CPU and memory readout from the existing metrics feed.
    Metrics,
    /// What is playing, or the "nothing is playing" line.
    Media,
    /// A string from the configuration.
    Text,
    /// The settings gear.
    Settings,
    /// Flexible space.
    Spacer,
}

impl WidgetKind {
    /// Every type the configuration may name, for the settings window and the tests.
    pub const ALL: [Self; 9] = [
        Self::Launcher,
        Self::Profile,
        Self::Clock,
        Self::Date,
        Self::Metrics,
        Self::Media,
        Self::Text,
        Self::Settings,
        Self::Spacer,
    ];

    /// Reads the `type` field, case-insensitively; `None` for a type this build does not know.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.trim().to_ascii_lowercase().as_str() {
            "launcher" => Some(Self::Launcher),
            "profile" => Some(Self::Profile),
            "clock" => Some(Self::Clock),
            "date" => Some(Self::Date),
            "metrics" => Some(Self::Metrics),
            "media" => Some(Self::Media),
            "text" => Some(Self::Text),
            "settings" => Some(Self::Settings),
            "spacer" => Some(Self::Spacer),
            _ => None,
        }
    }

    /// The spelling the configuration uses.
    pub fn name(self) -> &'static str {
        match self {
            Self::Launcher => "launcher",
            Self::Profile => "profile",
            Self::Clock => "clock",
            Self::Date => "date",
            Self::Metrics => "metrics",
            Self::Media => "media",
            Self::Text => "text",
            Self::Settings => "settings",
            Self::Spacer => "spacer",
        }
    }

    /// Whether the widget draws text, which is what the layout measures.
    pub fn is_text(self) -> bool {
        !matches!(self, Self::Launcher | Self::Settings | Self::Spacer)
    }
}

/// One of the three groups a widget may belong to.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Align {
    /// Anchored to the left edge, in array order.
    #[default]
    Left,
    /// Centred on the bar, in array order.
    Centre,
    /// Anchored to the right edge, in array order.
    Right,
}

impl Align {
    /// Reads the `align` field; `centre` is accepted as a synonym of `center`, and an unknown value
    /// falls back to the left group rather than dropping the widget.
    pub fn from_name(name: &str) -> Self {
        match name.trim().to_ascii_lowercase().as_str() {
            "center" | "centre" | "middle" => Self::Centre,
            "right" => Self::Right,
            _ => Self::Left,
        }
    }
}

/// One validated widget.
#[derive(Debug, Clone, PartialEq)]
pub struct WidgetSpec {
    /// What to draw.
    pub kind: WidgetKind,
    /// Which group it belongs to.
    pub align: Align,
    /// `HH:mm`-style pattern; only the clock reads it.
    pub format: String,
    /// Whether a lone clock carries the date, or whether a date is shown at all.
    pub show_date: bool,
    /// The string a `text` widget draws, already trimmed and bounded.
    pub text: String,
    /// Whether a `metrics` widget includes the CPU share.
    pub cpu: bool,
    /// Whether a `metrics` widget includes the memory share.
    pub ram: bool,
}

impl WidgetSpec {
    fn new(kind: WidgetKind, align: Align) -> Self {
        Self {
            kind,
            align,
            format: "HH:mm".to_owned(),
            show_date: true,
            text: String::new(),
            cpu: true,
            ram: true,
        }
    }
}

/// What [`parse`] had to leave out, for one warning per reload.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Skipped {
    /// Unknown widget types, in file order.
    pub unknown: Vec<String>,
    /// Entries dropped because the bar already had [`MAX_WIDGETS`].
    pub overflow: usize,
}

impl Skipped {
    /// Whether anything was skipped.
    pub fn is_empty(&self) -> bool {
        self.unknown.is_empty() && self.overflow == 0
    }
}

/// Turns the configuration entries into typed specs, in the same order.
pub fn parse(entries: &[BarWidget]) -> (Vec<WidgetSpec>, Skipped) {
    let mut specs = Vec::with_capacity(entries.len().min(MAX_WIDGETS));
    let mut skipped = Skipped::default();

    for entry in entries {
        let Some(kind) = WidgetKind::from_name(&entry.kind) else {
            skipped.unknown.push(entry.kind.clone());
            continue;
        };

        if specs.len() >= MAX_WIDGETS {
            skipped.overflow += 1;
            continue;
        }

        let mut spec = WidgetSpec::new(kind, Align::from_name(&entry.align));
        spec.show_date = entry.show_date.unwrap_or(true);
        spec.cpu = entry.cpu.unwrap_or(true);
        spec.ram = entry.ram.unwrap_or(true);

        if let Some(format) = entry.format.as_deref() {
            spec.format = valid_format(format);
        }

        if let Some(text) = entry.text.as_deref() {
            spec.text = text.chars().take(TEXT_UNITS).collect();
        }

        specs.push(spec);
    }

    (specs, skipped)
}

/// A clock format the formatter can honour: non-empty and no longer than [`MAX_FORMAT`].
fn valid_format(format: &str) -> String {
    let format = format.trim();

    if format.is_empty() || format.chars().count() > MAX_FORMAT {
        "HH:mm".to_owned()
    } else {
        format.to_owned()
    }
}

/// The wall clock digits a widget formatter needs, split out so the format can be tested.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClockReading {
    /// `0..=23`.
    pub hour: u16,
    /// `0..=59`.
    pub minute: u16,
    /// `0..=59`.
    pub second: u16,
    /// `0` is Sunday, as `SYSTEMTIME` numbers it.
    pub day_of_week: u16,
    /// Day of the month, `1..=31`.
    pub day: u16,
    /// Month, `1..=12`.
    pub month: u16,
}

/// Formats `reading` with `format`: `HH`/`H` hours, `mm`/`m` minutes, `ss`/`s` seconds, everything
/// else literal. Fixed-width fields are zero-padded, which is what keeps the clock from shifting.
pub fn format_clock<const N: usize>(target: &mut WideBuf<N>, format: &str, reading: &ClockReading) {
    let mut characters = format.chars().peekable();

    while let Some(character) = characters.next() {
        let repeated = characters.peek() == Some(&character);

        match (character, repeated) {
            ('H', true) => {
                let _ = characters.next();
                target.push_two_digits(u32::from(reading.hour));
            }
            ('H', false) => target.push_number(u32::from(reading.hour)),
            ('m', true) => {
                let _ = characters.next();
                target.push_two_digits(u32::from(reading.minute));
            }
            ('m', false) => target.push_number(u32::from(reading.minute)),
            ('s', true) => {
                let _ = characters.next();
                target.push_two_digits(u32::from(reading.second));
            }
            ('s', false) => target.push_number(u32::from(reading.second)),
            _ => target.push(character),
        }
    }
}

/// One widget's text, ready for the layout step.
#[derive(Debug, Clone, Default)]
pub struct WidgetText {
    /// The line the widget draws.
    pub main: WideBuf<96>,
    /// The date under a clock, when the widget carries one.
    pub sub: WideBuf<64>,
}

impl WidgetText {
    /// Composes the text for `spec`.
    ///
    /// `date_is_separate` is true when the same align group also holds a `date` widget: the clock
    /// then leaves the date to that widget instead of drawing it twice.
    pub fn compose(
        &mut self,
        spec: &WidgetSpec,
        reading: &ClockReading,
        metrics: Reading,
        profile: &WideBuf<64>,
        language: Language,
        date_is_separate: bool,
    ) {
        self.main.clear();
        self.sub.clear();

        match spec.kind {
            WidgetKind::Clock => {
                format_clock(&mut self.main, &spec.format, reading);

                if spec.show_date && !date_is_separate {
                    clock::compose_date(
                        &mut self.sub,
                        reading.day_of_week,
                        reading.day,
                        reading.month,
                        language,
                    );
                }
            }
            WidgetKind::Date if spec.show_date => clock::compose_date(
                &mut self.main,
                reading.day_of_week,
                reading.day,
                reading.month,
                language,
            ),
            WidgetKind::Metrics => {
                compose_metrics(&mut self.main, metrics, spec.cpu, spec.ram, language);
            }
            WidgetKind::Media => self.main.push_str(get(language, Text::BarMedia)),
            WidgetKind::Text => self.main.push_str(&spec.text),
            WidgetKind::Profile => self.main.push_buffer(profile),
            WidgetKind::Launcher | WidgetKind::Settings | WidgetKind::Spacer | WidgetKind::Date => {
            }
        }
    }
}

/// `CPU 12%  RAM 38%`, or the half the entry asked for. Duplicated from the legacy readout only in
/// the sense that it takes the same labels from the shared table.
pub fn compose_metrics(
    target: &mut WideBuf<96>,
    reading: Reading,
    cpu: bool,
    ram: bool,
    language: Language,
) {
    let mut wrote = false;

    if cpu {
        target.push_str(get(language, Text::BarCpu));
        target.push(' ');
        push_percent(target, reading.cpu);
        wrote = true;
    }

    if ram {
        if wrote {
            target.push_str("  ");
        }
        target.push_str(get(language, Text::BarRam));
        target.push(' ');
        push_percent(target, reading.ram);
    }
}

/// `12%`, or an em dash when the sampler could not read it.
fn push_percent(target: &mut WideBuf<96>, value: Option<u32>) {
    match value {
        Some(percent) => {
            target.push_number(percent.min(100));
            target.push('%');
        }
        None => target.push('—'),
    }
}

/// `Понедельник, 16 ноября`, exposed for the layout tests and the widget text.
pub fn compose_date(target: &mut WideBuf<64>, reading: &ClockReading, language: Language) {
    clock::compose_date(
        target,
        reading.day_of_week,
        reading.day,
        reading.month,
        language,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use neb_core::config::default_widgets;

    fn entry(kind: &str, align: &str) -> BarWidget {
        BarWidget {
            kind: kind.to_owned(),
            align: align.to_owned(),
            ..BarWidget::default()
        }
    }

    #[test]
    fn the_default_configuration_parses_every_widget() {
        let (widgets, skipped) = parse(&default_widgets());

        assert!(skipped.is_empty());
        assert_eq!(widgets.len(), 6);
        assert_eq!(widgets[0].kind, WidgetKind::Launcher);
        assert_eq!(widgets[0].align, Align::Left);
        assert_eq!(widgets[2].kind, WidgetKind::Clock);
        assert_eq!(widgets[2].align, Align::Centre);
        assert_eq!(widgets[5].kind, WidgetKind::Settings);
        assert_eq!(widgets[5].align, Align::Right);
    }

    #[test]
    fn an_unknown_type_is_skipped_and_named() {
        let entries = vec![
            entry("clock", "left"),
            entry("hologram", "left"),
            entry("metrics", "right"),
        ];

        let (widgets, skipped) = parse(&entries);

        assert_eq!(widgets.len(), 2);
        assert_eq!(skipped.unknown, vec!["hologram".to_owned()]);
        assert_eq!(widgets[1].kind, WidgetKind::Metrics);
    }

    #[test]
    fn an_unknown_align_lands_in_the_left_group() {
        let (widgets, _) = parse(&[entry("text", "diagonally")]);

        assert_eq!(widgets[0].align, Align::Left);
        assert_eq!(Align::from_name("centre"), Align::Centre);
        assert_eq!(Align::from_name("RIGHT"), Align::Right);
    }

    #[test]
    fn every_documented_type_round_trips_its_name() {
        for kind in WidgetKind::ALL {
            assert_eq!(WidgetKind::from_name(kind.name()), Some(kind));
        }

        assert_eq!(
            WidgetKind::from_name("Launcher"),
            Some(WidgetKind::Launcher)
        );
    }

    #[test]
    fn the_overflowing_tail_is_counted_not_drawn() {
        let entries: Vec<BarWidget> = (0..MAX_WIDGETS + 3)
            .map(|_| entry("text", "left"))
            .collect();

        let (widgets, skipped) = parse(&entries);

        assert_eq!(widgets.len(), MAX_WIDGETS);
        assert_eq!(skipped.overflow, 3);
        assert!(!skipped.is_empty());
    }

    #[test]
    fn a_clock_format_is_validated_and_formatted() {
        let reading = ClockReading {
            hour: 9,
            minute: 5,
            second: 7,
            ..ClockReading::default()
        };

        let mut buffer = WideBuf::<64>::new();
        format_clock(&mut buffer, "HH:mm:ss", &reading);
        assert_eq!(buffer.to_text(), "09:05:07");

        let mut short = WideBuf::<64>::new();
        format_clock(&mut short, "H:m", &reading);
        assert_eq!(short.to_text(), "9:5");

        let mut literal = WideBuf::<64>::new();
        format_clock(&mut literal, "HH ч.", &reading);
        assert_eq!(literal.to_text(), "09 ч.");
    }

    #[test]
    fn a_blank_or_absurd_clock_format_falls_back_to_hours_and_minutes() {
        for format in ["", "   ", &"H".repeat(MAX_FORMAT + 1)] {
            let mut entry = entry("clock", "left");
            entry.format = Some(format.to_owned());
            let (widgets, _) = parse(&[entry]);

            assert_eq!(widgets[0].format, "HH:mm", "{format:?}");
        }
    }

    #[test]
    fn a_lone_clock_carries_the_date_but_a_paired_one_does_not() {
        let (widgets, _) = parse(&[entry("clock", "center")]);
        let clock = &widgets[0];
        let reading = ClockReading {
            day_of_week: 1,
            day: 16,
            month: 11,
            ..ClockReading::default()
        };

        let mut text = WidgetText::default();
        text.compose(
            clock,
            &reading,
            Reading::default(),
            &WideBuf::default(),
            Language::Ru,
            false,
        );
        assert_eq!(text.sub.to_text(), "Понедельник, 16 ноября");

        let mut paired = WidgetText::default();
        paired.compose(
            clock,
            &reading,
            Reading::default(),
            &WideBuf::default(),
            Language::Ru,
            true,
        );
        assert!(paired.sub.is_empty(), "the date widget owns that line");
    }

    #[test]
    fn a_clock_with_show_date_off_never_writes_the_date() {
        let mut entry = entry("clock", "center");
        entry.show_date = Some(false);
        let (widgets, _) = parse(&[entry]);

        let mut text = WidgetText::default();
        text.compose(
            &widgets[0],
            &ClockReading::default(),
            Reading::default(),
            &WideBuf::default(),
            Language::Ru,
            false,
        );

        assert!(text.sub.is_empty());
    }

    #[test]
    fn the_media_widget_says_nothing_is_playing() {
        let (widgets, _) = parse(&[entry("media", "right")]);

        let mut text = WidgetText::default();
        text.compose(
            &widgets[0],
            &ClockReading::default(),
            Reading::default(),
            &WideBuf::default(),
            Language::Ru,
            false,
        );

        assert_eq!(text.main.to_text(), "Ничего не играет");
    }

    #[test]
    fn a_text_widget_draws_its_configured_string() {
        let mut entry = entry("text", "right");
        entry.text = Some("Ночь · тихо".to_owned());
        let (widgets, _) = parse(&[entry]);

        let mut text = WidgetText::default();
        text.compose(
            &widgets[0],
            &ClockReading::default(),
            Reading::default(),
            &WideBuf::default(),
            Language::Ru,
            false,
        );

        assert_eq!(text.main.to_text(), "Ночь · тихо");
    }

    #[test]
    fn metrics_can_show_one_half_or_both() {
        let reading = Reading {
            cpu: Some(12),
            ram: Some(38),
        };

        let mut both = WideBuf::<96>::new();
        compose_metrics(&mut both, reading, true, true, Language::En);
        assert_eq!(both.to_text(), "CPU 12%  RAM 38%");

        let mut memory = WideBuf::<96>::new();
        compose_metrics(&mut memory, reading, false, true, Language::Ru);
        assert_eq!(memory.to_text(), "ОЗУ 38%");

        let mut neither = WideBuf::<96>::new();
        compose_metrics(&mut neither, reading, false, false, Language::En);
        assert!(neither.is_empty());
    }

    #[test]
    fn a_long_text_string_is_cut_to_the_buffer_size() {
        let mut entry = entry("text", "left");
        entry.text = Some("x".repeat(TEXT_UNITS * 2));
        let (widgets, _) = parse(&[entry]);

        assert_eq!(widgets[0].text.chars().count(), TEXT_UNITS);
    }
}
