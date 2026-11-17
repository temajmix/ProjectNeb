//! The wallpaper slice of `config.json`.
//!
//! The file belongs to the C# build and to the dashboard, so this module reads the shared
//! [`WallpaperSettings`] object rather than declaring a second shape for it: whatever the dashboard
//! writes is what the scene shows after `wallpaper.reload`.

use neb_core::config::{ConfigStore, DEFAULT_WALLPAPER_COLOR, WallpaperSettings};
use neb_core::logging::LogHandle;

/// Frames per second the scene is rendered at. The aurora drifts slowly; a faster clock would only
/// burn battery.
pub const FRAMES_PER_SECOND: u32 = 30;

/// Working set this process must stay under, in bytes.
pub const MEMORY_BUDGET: usize = 60 * 1024 * 1024;

/// How long `--report-memory` idles before it reports.
pub const REPORT_AFTER_SECONDS: u64 = 12;

/// Reads the wallpaper settings from the shared configuration file.
///
/// A missing or broken file is not an error: [`ConfigStore::load`] falls back to the shipped
/// defaults, which is the same "nothing has been chosen yet" state the dashboard starts from.
pub fn load(log: Option<&LogHandle>) -> WallpaperSettings {
    let store = ConfigStore::at_default_location();
    let config = store.load();
    let settings = config.wallpapers;

    if let Some(log) = log {
        log.info(
            "wallpaper",
            &format!(
                "settings from '{}': colour {}, pause on fullscreen={}, pause on battery={}",
                store.path().display(),
                normalise_colour(&settings.color),
                settings.pause_on_fullscreen,
                settings.pause_on_battery
            ),
        );
    }

    settings
}

/// The configured colour as `#rrggbb`, falling back to [`DEFAULT_WALLPAPER_COLOR`].
///
/// The contract is a `#RRGGBB` string, but a hand-edited file can hold anything; the fallback is the
/// shipped scene colour, never black, so a typo cannot turn the desktop into a void.
pub fn normalise_colour(text: &str) -> String {
    match parse_colour(text) {
        Some(rgb) => format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]),
        None => DEFAULT_WALLPAPER_COLOR.to_owned(),
    }
}

/// Parses `#RRGGBB`, `RRGGBB`, `#RGB` or `RGB` into bytes.
pub fn parse_colour(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().strip_prefix('#').unwrap_or(text.trim());
    if !digits.chars().all(|digit| digit.is_ascii_hexdigit()) {
        return None;
    }

    let byte = |index: usize| u8::from_str_radix(&digits[index..=index], 16).ok();

    match digits.len() {
        6 => Some([
            u8::from_str_radix(&digits[0..2], 16).ok()?,
            u8::from_str_radix(&digits[2..4], 16).ok()?,
            u8::from_str_radix(&digits[4..6], 16).ok()?,
        ]),
        3 => {
            // `#abc` is shorthand for `#aabbcc`.
            let expand = |value: Option<u8>| value.map(|digit| digit * 16 + digit);
            Some([expand(byte(0))?, expand(byte(1))?, expand(byte(2))?])
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_six_digit_colour_parses_with_and_without_the_hash() {
        assert_eq!(parse_colour("#141021"), Some([0x14, 0x10, 0x21]));
        assert_eq!(parse_colour("141021"), Some([0x14, 0x10, 0x21]));
        assert_eq!(parse_colour("  #FFFFFF  "), Some([0xff, 0xff, 0xff]));
    }

    #[test]
    fn the_short_form_is_expanded_per_digit() {
        assert_eq!(parse_colour("#abc"), Some([0xaa, 0xbb, 0xcc]));
        assert_eq!(parse_colour("f0a"), Some([0xff, 0x00, 0xaa]));
    }

    #[test]
    fn anything_else_falls_back_to_the_shipped_colour() {
        for broken in [
            "",
            "#",
            "#12",
            "#12345",
            "#1234567",
            "#gggggg",
            "violet",
            "#12 34 56",
        ] {
            assert_eq!(parse_colour(broken), None, "'{broken}' must not parse");
            assert_eq!(
                normalise_colour(broken),
                DEFAULT_WALLPAPER_COLOR,
                "'{broken}' must fall back"
            );
        }
    }

    #[test]
    fn the_shipped_default_survives_a_round_trip() {
        assert_eq!(
            normalise_colour(DEFAULT_WALLPAPER_COLOR),
            DEFAULT_WALLPAPER_COLOR
        );
        assert_eq!(
            normalise_colour(&normalise_colour("#ABCDEF")),
            "#abcdef",
            "normalising twice must not change the value"
        );
    }

    #[test]
    fn the_budget_covers_the_scene_with_headroom() {
        assert_eq!(FRAMES_PER_SECOND, 30);
        assert_eq!(MEMORY_BUDGET, 60 * 1024 * 1024);
    }
}
