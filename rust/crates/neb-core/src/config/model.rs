//! Mirror of the C# configuration contract (`Neb.Core.Configuration`), one type per nested object.
//!
//! The JSON shape is fixed by the C# build that still owns the file on existing installations:
//! camelCase keys, string enum values, and every field optional so a hand-edited or older file loads.
//! [`Config::extra`] keeps keys this build does not know about, so a round trip through this crate
//! never drops a field a newer version wrote.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// Version of the on-disk shape this build writes; a future migration reads it to decide.
pub const SCHEMA_VERSION: u32 = 1;

/// Language the UI starts in before the user picks one.
pub const DEFAULT_LANGUAGE: &str = "ru";

/// Flat colour shown when no wallpaper is configured.
pub const DEFAULT_WALLPAPER_COLOR: &str = "#141021";

/// Minutes each playlist entry stays on screen.
pub const DEFAULT_ROTATE_MINUTES: i32 = 30;

/// Width of the settings window on a fresh install, in device-independent pixels.
pub const DEFAULT_WINDOW_WIDTH: f64 = 1280.0;

/// Height of the settings window on a fresh install, in device-independent pixels.
pub const DEFAULT_WINDOW_HEIGHT: f64 = 800.0;

/// Height of the bar on a fresh install, in device-independent pixels.
pub const DEFAULT_BAR_HEIGHT: i32 = 40;

/// Shortest bar the settings allow; below this the two-line clock cannot fit.
pub const MIN_BAR_HEIGHT: i32 = 20;

/// Tallest bar the settings allow; anything above it is a mistake worth clamping.
pub const MAX_BAR_HEIGHT: i32 = 120;

/// Theme used when `shell.themeId` names nothing that exists.
pub const DEFAULT_THEME_ID: &str = "liquid-glass";

/// Persisted application configuration (`config.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Config {
    /// Version of the on-disk shape; see [`SCHEMA_VERSION`].
    pub schema_version: u32,
    /// UI language tag, e.g. `ru`.
    pub language: String,
    /// Theme preferences.
    pub theme: ThemeSettings,
    /// Which dashboard modules the user enabled.
    pub modules: ModuleStates,
    /// Where the settings window sat when it was last closed.
    pub window: WindowSettings,
    /// Living-wallpaper preferences.
    pub wallpapers: WallpaperSettings,
    /// Shell (bar) preferences the native shell reads.
    ///
    /// Additive to the C# contract: the C# file has no such object and its reader ignores the key,
    /// so writing it here cannot break the other build. Absent means [`ShellSettings::default`].
    pub shell: ShellSettings,
    /// Keys written by another build; carried through untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            language: DEFAULT_LANGUAGE.to_owned(),
            theme: ThemeSettings::default(),
            modules: ModuleStates::default(),
            window: WindowSettings::default(),
            wallpapers: WallpaperSettings::default(),
            shell: ShellSettings::default(),
            extra: Map::new(),
        }
    }
}

/// How the bar behaves, mirroring the C# `ShellSettings` record field for field where it matters.
///
/// `barPosition` is a string in the C# contract (`"top"`/`"bottom"`), so it stays one here: an
/// unrecognised value must degrade to the top edge, not make the whole file unparsable.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ShellSettings {
    /// Whether bars are shown at all.
    pub bar_enabled: bool,
    /// Edge the bar takes: `top` or `bottom`.
    pub bar_position: String,
    /// Bar height in device-independent pixels; the AppBar reservation is this, scaled by DPI.
    pub bar_height: i32,
    /// Id of the theme the bar paints with.
    pub theme_id: String,
    /// Name shown next to the launcher glyph; `None` means the localised default.
    pub profile_name: Option<String>,
    /// Whether the dock window exists at all.
    pub dock_enabled: bool,
    /// Applications the user pinned to the dock, as paths they typed.
    pub pinned_apps: Vec<String>,
    /// Whether the dock stays hidden until the pointer reaches the bottom edge.
    pub dock_auto_hide: bool,
}

impl Default for ShellSettings {
    fn default() -> Self {
        Self {
            bar_enabled: true,
            bar_position: "top".to_owned(),
            bar_height: DEFAULT_BAR_HEIGHT,
            theme_id: DEFAULT_THEME_ID.to_owned(),
            profile_name: None,
            dock_enabled: true,
            pinned_apps: Vec::new(),
            dock_auto_hide: true,
        }
    }
}

impl ShellSettings {
    /// `barHeight` clamped into the range the settings window offers.
    pub fn height(&self) -> i32 {
        self.bar_height.clamp(MIN_BAR_HEIGHT, MAX_BAR_HEIGHT)
    }

    /// Whether the bar sits on the bottom edge; anything but `bottom` is the top edge.
    pub fn is_bottom(&self) -> bool {
        self.bar_position.trim().eq_ignore_ascii_case("bottom")
    }

    /// The theme id, or [`DEFAULT_THEME_ID`] when the file left it blank.
    pub fn theme(&self) -> &str {
        let id = self.theme_id.trim();
        if id.is_empty() { DEFAULT_THEME_ID } else { id }
    }
}

/// Available themes. The light theme is out of scope for phase 1.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ThemeName {
    /// The only shipped theme.
    #[default]
    Dark,
}

/// Theme-related user preferences.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ThemeSettings {
    /// Theme the dashboard renders.
    pub name: ThemeName,
}

/// Enabled/disabled state of the dashboard modules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ModuleStates {
    /// Living wallpapers.
    pub wallpapers: bool,
    /// The zapret network module.
    pub zapret: bool,
    /// The Windhawk integration.
    pub windhawk: bool,
    /// Discord rich presence.
    pub discord_rp: bool,
}

/// Persisted settings-window rectangle in device-independent pixels.
///
/// A `None` [`left`](Self::left)/[`top`](Self::top) means "not placed yet": the window is centred the
/// first time and only then starts remembering a position.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowSettings {
    /// Left edge of the restored window, or `None` to centre it.
    pub left: Option<f64>,
    /// Top edge of the restored window, or `None` to centre it.
    pub top: Option<f64>,
    /// Width of the restored (not maximized) window.
    pub width: f64,
    /// Height of the restored (not maximized) window.
    pub height: f64,
    /// Whether the window was maximized, so it comes back maximized.
    pub maximized: bool,
}

impl Default for WindowSettings {
    fn default() -> Self {
        Self {
            left: None,
            top: None,
            width: DEFAULT_WINDOW_WIDTH,
            height: DEFAULT_WINDOW_HEIGHT,
            maximized: false,
        }
    }
}

/// Which built-in scene document renders the current wallpaper.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SceneKind {
    /// A flat colour from [`WallpaperSettings::color`]; the cheapest scene.
    #[default]
    Color,
    /// A still image.
    Image,
    /// A looping clip, always muted.
    Video,
    /// A local HTML page, rendered as the wallpaper itself.
    Web,
}

/// Living-wallpaper preferences persisted inside [`Config::wallpapers`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WallpaperSettings {
    /// Whether the wallpaper process should run.
    pub enabled: bool,
    /// Which scene document renders [`path`](Self::path).
    pub kind: SceneKind,
    /// The file the scene draws: an image, a clip or an HTML page.
    pub path: Option<String>,
    /// Flat colour as `#RRGGBB`.
    pub color: String,
    /// Files rotated through on a timer.
    pub playlist: Vec<String>,
    /// Minutes each playlist entry stays on screen.
    pub rotate_minutes: i32,
    /// Pause while a fullscreen application owns the foreground.
    pub pause_on_fullscreen: bool,
    /// Pause while the machine runs on battery.
    pub pause_on_battery: bool,
}

impl Default for WallpaperSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            kind: SceneKind::default(),
            path: None,
            color: DEFAULT_WALLPAPER_COLOR.to_owned(),
            playlist: Vec::new(),
            rotate_minutes: DEFAULT_ROTATE_MINUTES,
            pause_on_fullscreen: true,
            pause_on_battery: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shell_section_defaults_to_a_visible_top_bar() {
        let shell = ShellSettings::default();

        assert!(shell.bar_enabled);
        assert_eq!(shell.height(), DEFAULT_BAR_HEIGHT);
        assert!(!shell.is_bottom());
        assert_eq!(shell.theme(), DEFAULT_THEME_ID);
        assert_eq!(shell.profile_name, None);
        assert!(shell.dock_enabled, "the dock is on by default, as in C#");
        assert!(shell.pinned_apps.is_empty());
        assert!(
            shell.dock_auto_hide,
            "an auto-hiding dock is the C# default"
        );
    }

    #[test]
    fn the_height_is_clamped_into_the_supported_range() {
        let shell = |height| ShellSettings {
            bar_height: height,
            ..ShellSettings::default()
        };

        assert_eq!(shell(-10).height(), MIN_BAR_HEIGHT);
        assert_eq!(shell(0).height(), MIN_BAR_HEIGHT);
        assert_eq!(shell(40).height(), 40);
        assert_eq!(shell(5_000).height(), MAX_BAR_HEIGHT);
    }

    #[test]
    fn the_edge_is_read_case_insensitively_and_anything_else_is_the_top() {
        for value in ["bottom", "Bottom", " bottom "] {
            assert!(
                ShellSettings {
                    bar_position: value.to_owned(),
                    ..ShellSettings::default()
                }
                .is_bottom(),
                "{value} must mean the bottom edge"
            );
        }

        for value in ["top", "", "left", "middle"] {
            assert!(
                !ShellSettings {
                    bar_position: value.to_owned(),
                    ..ShellSettings::default()
                }
                .is_bottom(),
                "{value} must mean the top edge"
            );
        }
    }

    #[test]
    fn a_blank_theme_id_falls_back_to_the_shipped_theme() {
        let shell = ShellSettings {
            theme_id: "   ".to_owned(),
            ..ShellSettings::default()
        };

        assert_eq!(shell.theme(), DEFAULT_THEME_ID);
    }

    #[test]
    fn a_c_sharp_config_without_a_shell_section_still_loads() {
        let config: Config =
            serde_json::from_str(r#"{"schemaVersion":1,"language":"en","theme":{"name":"Dark"}}"#)
                .expect("the C# file must stay readable");

        assert_eq!(config.language, "en");
        assert_eq!(config.shell, ShellSettings::default());
    }

    #[test]
    fn the_shell_section_round_trips_and_ignores_an_unknown_edge() {
        let config = Config {
            shell: ShellSettings {
                bar_enabled: false,
                bar_position: "sideways".to_owned(),
                bar_height: 56,
                theme_id: "graphite".to_owned(),
                profile_name: Some("Ночь".to_owned()),
                dock_enabled: false,
                pinned_apps: vec![
                    r"C:\Windows\System32\notepad.exe".to_owned(),
                    r"%WINDIR%\System32\mspaint.exe".to_owned(),
                ],
                dock_auto_hide: false,
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");
        let back: Config = serde_json::from_str(&text).expect("parse");

        assert_eq!(back, config);
        assert_eq!(back.shell.bar_height, 56);
        assert_eq!(back.shell.profile_name.as_deref(), Some("Ночь"));
        assert!(!back.shell.is_bottom(), "an unknown edge is the top edge");
        assert!(!back.shell.dock_enabled);
        assert!(!back.shell.dock_auto_hide);
        assert_eq!(back.shell.pinned_apps.len(), 2);
    }

    #[test]
    fn a_c_sharp_file_with_the_dock_fields_still_loads() {
        // The C# `ShellSettings` record writes these three names; the Rust model must read the same
        // spellings. Unknown keys *inside* a section are dropped — the same as every other section —
        // while unknown top-level keys are carried through, which is what `config::store` covers.
        let config: Config = serde_json::from_str(
            r##"{
                "schemaVersion": 1,
                "shell": {
                    "barEnabled": true,
                    "dockEnabled": true,
                    "dockAutoHide": false,
                    "pinnedApps": ["C:\\Windows\\notepad.exe"]
                }
            }"##,
        )
        .expect("the C# shell section must stay readable");

        assert!(config.shell.dock_enabled);
        assert!(!config.shell.dock_auto_hide);
        assert_eq!(
            config.shell.pinned_apps,
            vec![r"C:\Windows\notepad.exe".to_owned()]
        );

        let text = serde_json::to_string(&config).expect("render");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse");

        assert_eq!(
            value["shell"]["pinnedApps"][0].as_str(),
            Some(r"C:\Windows\notepad.exe")
        );
        assert_eq!(value["shell"]["dockAutoHide"].as_bool(), Some(false));
    }
}
