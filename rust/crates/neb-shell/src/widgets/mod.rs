//! The bar's text: what each widget says, formatted into fixed buffers.
//!
//! Painting must not allocate, so the strings a frame draws are built *before* the frame: the clock
//! once a second (and its layouts once a minute, when the text actually changed), the metrics every
//! time a sample arrives. [`BarText`] is the small, copyable-by-hand result, and [`WideBuf`] is the
//! UTF-16 buffer behind each widget.

mod clock;
pub mod metrics;

pub use clock::ClockText;
pub use metrics::MetricsText;

use neb_core::strings::{Language, Text, get};

/// How long the profile name may be before it is cut with an ellipsis.
const PROFILE_UNITS: usize = 64;

/// A UTF-16 string of fixed capacity.
///
/// Writing past the capacity truncates instead of growing, which is what keeps every widget's text
/// off the heap: the longest realistic profile name is far shorter than this, and a name that is not
/// simply loses its tail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WideBuf<const N: usize> {
    units: [u16; N],
    len: usize,
}

impl<const N: usize> Default for WideBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> WideBuf<N> {
    /// An empty buffer.
    pub const fn new() -> Self {
        Self {
            units: [0; N],
            len: 0,
        }
    }

    /// Empties the buffer without touching the capacity.
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Appends one character, dropping it when the buffer is full.
    pub fn push(&mut self, character: char) {
        let mut encoded = [0u16; 2];

        for unit in character.encode_utf16(&mut encoded) {
            self.push_unit(*unit);
        }
    }

    /// Appends every character of `text`, dropping what does not fit.
    pub fn push_str(&mut self, text: &str) {
        for unit in text.encode_utf16() {
            self.push_unit(unit);
        }
    }

    /// Appends `value` in decimal, without allocating.
    pub fn push_number(&mut self, value: u32) {
        let mut digits = [0u8; 10];
        let mut count = 0;
        let mut remaining = value;

        loop {
            digits[count] = b'0' + (remaining % 10) as u8;
            count += 1;
            remaining /= 10;
            if remaining == 0 {
                break;
            }
        }

        while count > 0 {
            count -= 1;
            self.push_unit(u16::from(digits[count]));
        }
    }

    /// Appends `value` as two zero-padded digits.
    pub fn push_two_digits(&mut self, value: u32) {
        self.push_unit(u16::from(b'0') + (value / 10 % 10) as u16);
        self.push_unit(u16::from(b'0') + (value % 10) as u16);
    }

    /// The units written so far.
    pub fn as_units(&self) -> &[u16] {
        &self.units[..self.len]
    }

    /// The text as a `String`, for logging and tests.
    pub fn to_text(&self) -> String {
        String::from_utf16_lossy(self.as_units())
    }

    /// Whether nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Copies `other` into this buffer and reports whether that changed anything.
    pub fn replace_with(&mut self, other: &Self) -> bool {
        if self.as_units() == other.as_units() {
            return false;
        }

        self.units[..other.len].copy_from_slice(other.as_units());
        self.len = other.len;
        true
    }

    fn push_unit(&mut self, unit: u16) {
        if self.len < N {
            self.units[self.len] = unit;
            self.len += 1;
        }
    }
}

/// Everything one bar draws as text.
#[derive(Debug, Clone)]
pub struct BarText {
    /// `HH:mm`.
    pub time: WideBuf<8>,
    /// The long date, or empty when the bar is too short for it.
    pub date: WideBuf<48>,
    /// `CPU 12%  RAM 38%`.
    pub metrics: WideBuf<32>,
    /// The active profile's name.
    pub profile: WideBuf<PROFILE_UNITS>,
}

impl BarText {
    /// Text for a fresh bar: the localised profile name and empty readouts, which the first timer
    /// tick and the first sample fill in.
    pub fn new(profile: Option<&str>, language: Language) -> Self {
        let mut text = Self {
            time: WideBuf::new(),
            date: WideBuf::new(),
            metrics: WideBuf::new(),
            profile: WideBuf::new(),
        };

        match profile {
            Some(name) if !name.trim().is_empty() => text.profile.push_str(name.trim()),
            _ => text
                .profile
                .push_str(get(language, Text::BarDefaultProfile)),
        }

        text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_is_utf16_and_stops_at_its_capacity() {
        let mut buffer = WideBuf::<4>::new();
        buffer.push_str("abcd");
        buffer.push('e');

        assert_eq!(buffer.as_units().len(), 4);
        assert_eq!(buffer.to_text(), "abcd");
    }

    #[test]
    fn a_surrogate_pair_needs_both_units() {
        let mut buffer = WideBuf::<2>::new();
        buffer.push('😀');

        assert_eq!(buffer.as_units().len(), 2);
        assert_eq!(buffer.to_text(), "😀");

        let mut tight = WideBuf::<1>::new();
        tight.push('😀');
        assert_eq!(
            tight.as_units().len(),
            1,
            "half a surrogate pair is dropped rather than written"
        );
    }

    #[test]
    fn numbers_are_written_without_an_allocator() {
        let mut buffer = WideBuf::<12>::new();
        buffer.push_number(0);
        buffer.push(' ');
        buffer.push_number(100);
        buffer.push(' ');
        buffer.push_number(7);

        assert_eq!(buffer.to_text(), "0 100 7");
    }

    #[test]
    fn two_digit_fields_are_zero_padded() {
        let mut buffer = WideBuf::<8>::new();
        buffer.push_two_digits(0);
        buffer.push(':');
        buffer.push_two_digits(59);

        assert_eq!(buffer.to_text(), "00:59");
    }

    #[test]
    fn replacing_reports_whether_anything_changed() {
        let mut target = WideBuf::<8>::new();
        let mut source = WideBuf::<8>::new();
        source.push_str("12:00");

        assert!(target.replace_with(&source));
        assert!(!target.replace_with(&source));
        assert_eq!(target.to_text(), "12:00");

        let mut longer = WideBuf::<8>::new();
        longer.push_str("12:00:00");
        assert!(target.replace_with(&longer));
        assert_eq!(target.to_text(), "12:00:00");
    }

    #[test]
    fn a_profile_name_defaults_to_the_localised_one() {
        assert_eq!(
            BarText::new(None, Language::Ru).profile.to_text(),
            get(Language::Ru, Text::BarDefaultProfile)
        );
        assert_eq!(
            BarText::new(Some("  "), Language::En).profile.to_text(),
            get(Language::En, Text::BarDefaultProfile)
        );
        assert_eq!(
            BarText::new(Some(" Ночь "), Language::Ru).profile.to_text(),
            "Ночь"
        );
    }

    #[test]
    fn a_long_profile_name_is_truncated_instead_of_overflowing() {
        let long = "a".repeat(PROFILE_UNITS * 2);

        assert_eq!(
            BarText::new(Some(&long), Language::En)
                .profile
                .as_units()
                .len(),
            PROFILE_UNITS
        );
    }
}
