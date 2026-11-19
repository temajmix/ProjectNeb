//! The six palettes the build ships.
//!
//! The JSON lives in `crates/neb-core/themes` and is compiled in with `include_str!`, so a fresh
//! installation has the presets before any file is written and a broken themes folder cannot take
//! them away. The files are ordinary packages (see [`super::Theme`]), which means the same validator
//! covers a preset, an imported theme and a hand-edited one.
//!
//! Order matters: it is the order the settings window lists them in.

use crate::theme::ThemeTokens;

use super::Theme;

/// Ids of the presets, in display order.
pub const BUILT_IN_IDS: [&str; 6] = [
    "nebula-glass",
    "hyprland",
    "macos",
    "minimal-dark",
    "cyberpunk",
    "windows-clean",
];

/// The preset documents, paired with their ids.
const PRESETS: [(&str, &str); 6] = [
    (
        "nebula-glass",
        include_str!("../../themes/nebula-glass.json"),
    ),
    ("hyprland", include_str!("../../themes/hyprland.json")),
    ("macos", include_str!("../../themes/macos.json")),
    (
        "minimal-dark",
        include_str!("../../themes/minimal-dark.json"),
    ),
    ("cyberpunk", include_str!("../../themes/cyberpunk.json")),
    (
        "windows-clean",
        include_str!("../../themes/windows-clean.json"),
    ),
];

/// Every preset, in display order.
///
/// A preset that does not validate is skipped rather than taking the whole list down; the tests
/// parse all six, so the skip is a safety net for a corrupted build, not an expected state.
pub fn all() -> Vec<Theme> {
    PRESETS
        .iter()
        .filter_map(|(id, text)| parse(id, text))
        .collect()
}

/// The preset called `id`.
pub fn built_in(id: &str) -> Option<Theme> {
    PRESETS
        .iter()
        .find(|(preset, _)| *preset == id)
        .and_then(|(id, text)| parse(id, text))
}

/// The token set of the preset called `id`, for the shell's fallback path.
pub fn tokens(id: &str) -> Option<ThemeTokens> {
    built_in(id).map(|theme| theme.tokens)
}

/// Whether `id` names a preset.
pub fn is_builtin(id: &str) -> bool {
    PRESETS.iter().any(|(preset, _)| *preset == id)
}

/// The ids of the presets, in display order.
pub fn built_in_ids() -> &'static [&'static str] {
    &BUILT_IN_IDS
}

/// The raw documents, for tests and tools that want the shipped bytes.
pub fn documents() -> impl Iterator<Item = (&'static str, &'static str)> {
    PRESETS.into_iter()
}

/// Parses one compiled-in document, with the expected id as the fallback.
fn parse(id: &str, text: &str) -> Option<Theme> {
    Theme::parse(text, Some(id)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_six_presets_parse_and_are_listed_in_order() {
        let themes = all();
        let ids: Vec<&str> = themes.iter().map(|theme| theme.id.as_str()).collect();

        assert_eq!(ids, BUILT_IN_IDS);
        assert_eq!(themes.len(), 6);
    }

    #[test]
    fn every_preset_is_a_complete_package() {
        for theme in all() {
            assert_eq!(theme.schema_version, super::super::PACKAGE_SCHEMA_VERSION);
            assert!(!theme.name.trim().is_empty(), "{}", theme.id);
            assert!(!theme.description.trim().is_empty(), "{}", theme.id);
            assert_eq!(theme.author, "ProjectNeb", "{}", theme.id);
            assert_eq!(theme.tokens.id, theme.id);
            assert_eq!(theme.tokens.name, theme.name);
            assert!(
                !theme.tokens.font.trim().is_empty(),
                "{} has no font",
                theme.id
            );
            assert!(theme.tokens.bar_height.is_some(), "{}", theme.id);
            assert!(theme.tokens.dock_tile.is_some(), "{}", theme.id);
            assert!(
                (0.0..=super::super::PACKAGE_MAX_RADIUS).contains(&theme.tokens.radius),
                "{} radius",
                theme.id
            );
            assert!(
                (super::super::PACKAGE_MIN_OPACITY..=1.0).contains(&theme.tokens.opacity),
                "{} opacity",
                theme.id
            );
        }
    }

    #[test]
    fn the_presets_are_visually_distinct() {
        let themes = all();

        for (index, left) in themes.iter().enumerate() {
            for right in &themes[index + 1..] {
                assert_ne!(
                    (
                        left.tokens.background,
                        left.tokens.accent,
                        left.tokens.accent2
                    ),
                    (
                        right.tokens.background,
                        right.tokens.accent,
                        right.tokens.accent2
                    ),
                    "{} and {} share a palette",
                    left.id,
                    right.id
                );
            }
        }
    }

    #[test]
    fn the_current_look_is_nebula_glass() {
        let theme = built_in("nebula-glass").expect("the shipped look");

        assert_eq!(theme.name, "Nebula Glass");
        assert_eq!(theme.tokens.background.red, 0x0B);
        assert_eq!(theme.tokens.accent.red, 0xA7);
    }

    #[test]
    fn the_shell_fallback_finds_a_preset() {
        let cyberpunk = tokens("cyberpunk").expect("cyberpunk is compiled in");

        assert_eq!(cyberpunk.id, "cyberpunk");
        assert_eq!(cyberpunk.accent.red, 0xFF);
        assert_eq!(cyberpunk.accent2.blue, 0xFF);
        assert!(tokens("not-a-preset").is_none());
    }

    #[test]
    fn every_raw_document_is_the_package_it_claims_to_be() {
        for (id, text) in documents() {
            let value: serde_json::Value =
                serde_json::from_str(text).expect("a preset must be valid json");

            assert_eq!(value["schemaVersion"], serde_json::json!(1), "{id}");
            assert_eq!(value["id"], serde_json::json!(id), "{id}");
        }
    }
}
