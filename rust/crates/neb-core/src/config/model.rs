//! Mirror of the C# configuration contract (`Neb.Core.Configuration`), one type per nested object.
//!
//! The JSON shape is fixed by the C# build that still owns the file on existing installations:
//! camelCase keys, string enum values, and every field optional so a hand-edited or older file loads.
//! [`Config::extra`] keeps keys this build does not know about, so a round trip through this crate
//! never drops a field a newer version wrote.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::windows_mode::WindowsModeSettings;

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

/// Speed multiplier of the shell animations on a fresh install.
pub const DEFAULT_ANIMATION_SPEED: f64 = 1.0;

/// Slowest animation a user may ask for; below this a transition would read as a freeze.
pub const MIN_ANIMATION_SPEED: f64 = 0.5;

/// Fastest animation a user may ask for; above this the movement is not visible.
pub const MAX_ANIMATION_SPEED: f64 = 2.0;

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
    /// Motion preferences every ProjectNeb surface honours.
    pub animations: AnimationSettings,
    /// Discord Rich Presence preferences.
    ///
    /// Additive to the C# contract: the C# build ignores the key, and an absent object means
    /// [`DiscordSettings::default`], which is the shipped "not configured" state.
    pub discord: DiscordSettings,
    /// Reversible hiding of the default Windows shell chrome.
    ///
    /// Additive to the C# contract: the other build ignores the key, and an absent object means
    /// [`WindowsModeSettings::default`], which is "nothing hidden".
    pub windows_mode: WindowsModeSettings,
    /// Tiling window manager preferences.
    ///
    /// Additive to the C# contract, like [`ShellSettings`]: the other build ignores the key, and an
    /// absent object means disabled. The manager itself lives in the `neb-wm` process.
    pub window_manager: WindowManagerSettings,
    /// The DPI-bypass module (`neb-zapret`).
    ///
    /// Additive to the C# contract: an absent object means [`ZapretSettings::default`], which is
    /// "nothing imported and nothing started", so an older file loads unchanged.
    pub zapret: ZapretSettings,
    /// The Windhawk integration (`neb-windhawk`).
    ///
    /// Additive to the C# contract: an absent object means [`WindhawkSettings::default`], which
    /// is "the module is off and does not start with the shell".
    pub windhawk: WindhawkSettings,
    /// ProjectNeb Pro account and licence service.
    ///
    /// Additive to the C# contract, like every other section: an absent object means
    /// [`AccountSettings::default`], i.e. "no service configured and no device id yet".
    pub account: AccountSettings,
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
            animations: AnimationSettings::default(),
            discord: DiscordSettings::default(),
            windows_mode: WindowsModeSettings::default(),
            window_manager: WindowManagerSettings::default(),
            zapret: ZapretSettings::default(),
            windhawk: WindhawkSettings::default(),
            account: AccountSettings::default(),
            extra: Map::new(),
        }
    }
}

/// Preferences of the `neb-wm` tiling manager, persisted under `windowManager`.
///
/// The shape is deliberately plain: the manager process owns the semantics, while the settings
/// window only edits values. [`layout`](Self::layout) is a free-form string for the same reason
/// `shell.barPosition` is: a file that names a layout this build does not know must still load, and
/// the manager falls back to [`DEFAULT_WM_LAYOUT`] with one warning.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowManagerSettings {
    /// Whether the manager process manages windows at all.
    pub enabled: bool,
    /// `dwindle`, `masterstack`, `grid` or `monocle`.
    pub layout: String,
    /// Space between two tiles, in physical pixels.
    pub gaps: i32,
    /// Space between the tiling area and the monitor work area, in physical pixels.
    pub outer_gaps: i32,
    /// Border inset of every tile, in physical pixels.
    pub border: i32,
    /// Number of virtual workspaces the manager keeps; `1..=9` so every one has a hotkey.
    pub workspaces: usize,
    /// Whether a layout change animates the windows instead of jumping.
    pub animations: bool,
    /// Per-application overrides, first match wins.
    pub rules: Vec<WindowRule>,
}

/// Layout used when the configuration names nothing this build knows.
pub const DEFAULT_WM_LAYOUT: &str = "dwindle";

/// Default space between tiles, in physical pixels.
pub const DEFAULT_WM_GAPS: i32 = 8;

/// Default space around the tiling area, in physical pixels.
pub const DEFAULT_WM_OUTER_GAPS: i32 = 12;

/// Default border inset of a tile, in physical pixels.
pub const DEFAULT_WM_BORDER: i32 = 1;

/// Default number of virtual workspaces.
pub const DEFAULT_WM_WORKSPACES: usize = 4;

/// Most workspaces the hotkey row can address.
pub const MAX_WM_WORKSPACES: usize = 9;

impl Default for WindowManagerSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            layout: DEFAULT_WM_LAYOUT.to_owned(),
            gaps: DEFAULT_WM_GAPS,
            outer_gaps: DEFAULT_WM_OUTER_GAPS,
            border: DEFAULT_WM_BORDER,
            workspaces: DEFAULT_WM_WORKSPACES,
            animations: true,
            rules: Vec::new(),
        }
    }
}

impl WindowManagerSettings {
    /// Gap between tiles, clamped to a range that still leaves a usable tile on a small screen.
    pub fn gaps(&self) -> i32 {
        self.gaps.clamp(0, 64)
    }

    /// Space around the tiling area, clamped like [`gaps`](Self::gaps).
    pub fn outer_gaps(&self) -> i32 {
        self.outer_gaps.clamp(0, 128)
    }

    /// Border inset, clamped so a tile can never be consumed by its own border.
    pub fn border(&self) -> i32 {
        self.border.clamp(0, 8)
    }

    /// Workspace count clamped into `1..=9`.
    pub fn workspaces(&self) -> usize {
        self.workspaces.clamp(1, MAX_WM_WORKSPACES)
    }

    /// The layout name, or [`DEFAULT_WM_LAYOUT`] when the file left it blank.
    pub fn layout(&self) -> &str {
        let layout = self.layout.trim();
        if layout.is_empty() {
            DEFAULT_WM_LAYOUT
        } else {
            layout
        }
    }
}

/// One per-application override inside `windowManager.rules`.
///
/// Every field is optional so a rule can change one thing and inherit the rest. `match` is a
/// regular expression tested against the window's executable name, class and title; the first
/// matching rule wins. A rule without `match` matches every window and is therefore a catch-all.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowRule {
    /// Regular expression tested against executable, class and title, case-insensitively.
    #[serde(rename = "match", skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// Whether matching windows float instead of tiling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub float: Option<bool>,
    /// Workspace (zero-based) a matching window is placed on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<usize>,
    /// Whether a matching window keeps the system decorations instead of the manager's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_border: Option<bool>,
    /// Whether a matching window is left completely alone.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore: Option<bool>,
}

/// The "обход блокировок" module: where the user's zapret pack lives, which strategy they chose,
/// and whether it should come up with the shell.
///
/// The module never ships third-party binaries: `packPath` points either at the folder the user
/// imported into `%LocalAppData%\ProjectNeb\zapret` or at a pack they keep themselves.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ZapretSettings {
    /// Whether the module is enabled in the dashboard. It does not start anything by itself.
    pub enabled: bool,
    /// Folder holding `bin\winws.exe`; `None` means the imported copy under the data directory.
    pub pack_path: Option<String>,
    /// Name of the strategy the user ran last, e.g. `general (ALT)`.
    pub strategy: Option<String>,
    /// Whether the supervisor should start that strategy when it starts.
    ///
    /// This build records the preference only; starting the module is still an explicit command,
    /// because a strategy needs an elevated helper and should not pop a UAC prompt at logon.
    pub auto_start: bool,
}

/// The Windhawk integration: whether the manager runs and whether it starts with the shell.
///
/// Windhawk itself is a third-party application; ProjectNeb only browses its catalog, installs
/// sources into its own data directory and reports what is there. Nothing here duplicates
/// Windhawk's settings, and an absent section means the module is off.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindhawkSettings {
    /// Whether the supervisor should run `neb-windhawk.exe`.
    pub enabled: bool,
    /// Whether that process should come up with the shell. Recorded for the supervisor.
    pub auto_start: bool,
    /// Overrides the Windhawk data directory (`%ProgramData%\Windhawk` by default).
    ///
    /// Set by portable installations and by tests; `None` means "use the directory the detected
    /// installation names in its `windhawk.ini`".
    pub data_dir: Option<String>,
}

/// The ProjectNeb Pro account: where the licence worker lives and which device this is.
///
/// `apiBase` is empty on a build that ships without a service, and the Account screen then says so
/// instead of offering a sign-in that cannot work. `deviceId` is filled in on the first refresh and
/// kept, so one installation keeps one device registration across restarts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AccountSettings {
    /// Root URL of the licence worker, e.g. `https://api.projectneb.example`. Empty means "not
    /// configured"; nothing is called and the UI says so.
    pub api_base: String,
    /// Random UUID identifying this installation. Empty until the first sign-in or refresh.
    pub device_id: String,
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
    /// Whether the `Alt+Space` launcher is armed.
    ///
    /// Mirrors the C# `ShellSettings.LauncherEnabled`; a file without the key keeps the launcher on.
    pub launcher_enabled: bool,
    /// Whether the Windows taskbar is hidden while the shell runs.
    ///
    /// Mirrors the C# `ShellSettings.HideTaskbar`; the default keeps the taskbar visible.
    pub hide_taskbar: bool,
    /// What the bar draws, in the order it draws it.
    pub bar: BarSettings,
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
            launcher_enabled: true,
            hide_taskbar: false,
            bar: BarSettings::default(),
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

/// What the bar draws: one entry per widget, in the order they appear inside their align group.
///
/// Additive to the C# contract, like [`ShellSettings`]: the other build ignores `shell.bar`. An
/// absent object means [`BarSettings::default`], which is exactly the layout every earlier build drew.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BarSettings {
    /// Widgets left to right inside each of the three groups.
    pub widgets: Vec<BarWidget>,
}

impl Default for BarSettings {
    fn default() -> Self {
        Self {
            widgets: default_widgets(),
        }
    }
}

/// The fresh-install layout: launcher and profile left, clock and date centre, metrics and settings
/// right. Changing this list changes what a new user sees, so it stays in one place.
pub fn default_widgets() -> Vec<BarWidget> {
    let widget = |kind: &str, align: &str| BarWidget {
        kind: kind.to_owned(),
        align: align.to_owned(),
        ..BarWidget::default()
    };

    vec![
        widget("launcher", "left"),
        widget("profile", "left"),
        widget("clock", "center"),
        widget("date", "center"),
        widget("metrics", "right"),
        widget("settings", "right"),
    ]
}

/// One entry of `shell.bar.widgets`.
///
/// `type` and `align` are free-form strings on purpose: an older build must be able to load a file
/// that names a widget type it does not know, and the shell skips it with one warning instead of
/// refusing the whole configuration. The optional fields only matter to the widget they belong to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BarWidget {
    /// `launcher`, `profile`, `clock`, `date`, `metrics`, `media`, `text`, `settings` or `spacer`.
    #[serde(rename = "type")]
    pub kind: String,
    /// `left`, `center` (or `centre`) or `right`.
    pub align: String,
    /// `clock` format, e.g. `HH:mm` or `HH:mm:ss`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub format: Option<String>,
    /// Whether a `clock` carries the date underneath it, and whether a `date` is drawn at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub show_date: Option<bool>,
    /// The string a `text` widget draws.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Whether a `metrics` widget includes the CPU share.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cpu: Option<bool>,
    /// Whether a `metrics` widget includes the memory share.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ram: Option<bool>,
}

impl Default for BarWidget {
    fn default() -> Self {
        Self {
            kind: String::new(),
            align: "left".to_owned(),
            format: None,
            show_date: None,
            text: None,
            cpu: None,
            ram: None,
        }
    }
}

/// Motion preferences. `enabled` is the master switch; `speed` multiplies every duration.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AnimationSettings {
    /// `false` makes every transition instant.
    pub enabled: bool,
    /// Duration multiplier in `0.5..=2.0`; higher is faster.
    pub speed: f64,
}

impl Default for AnimationSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            speed: DEFAULT_ANIMATION_SPEED,
        }
    }
}

impl AnimationSettings {
    /// The speed clamped into the range the settings window offers.
    pub fn speed(&self) -> f64 {
        self.speed.clamp(MIN_ANIMATION_SPEED, MAX_ANIMATION_SPEED)
    }

    /// A base duration in milliseconds, scaled by the user's speed.
    ///
    /// A disabled animation is zero milliseconds, which is what makes "instant" one code path
    /// rather than a flag every caller has to remember.
    pub fn scaled_ms(&self, base_ms: u32) -> u32 {
        if !self.enabled {
            return 0;
        }

        let scaled = f64::from(base_ms) / self.speed();
        scaled.round().clamp(0.0, f64::from(u32::MAX)) as u32
    }
}

/// Discord Rich Presence preferences.
///
/// The application id is empty until the user creates their own Discord application; an empty id is
/// the honest "not configured" state the module reports, never a pretend connection. Presets live in
/// [`crate::paths::discord_presets_dir`]; this object only records which one is pinned and which
/// foreground rules are armed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DiscordSettings {
    /// Whether the supervisor should run `neb-discord.exe`.
    pub enabled: bool,
    /// Discord application id used for the handshake; empty means "not configured".
    pub app_id: String,
    /// Preset the user pinned manually; `None` means the foreground rules decide.
    pub active_preset: Option<String>,
    /// Rules evaluated every poll, in order, against the foreground window.
    pub rules: Vec<DiscordRule>,
}

/// One foreground-to-preset rule.
///
/// `match` is a reserved word in Rust and a plain key in the JSON contract, hence the rename. The
/// pattern may be prefixed with `exe:`, `class:` or `title:`; without a prefix it is compared with
/// all three. `*` is a wildcard, and every comparison is case-insensitive.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct DiscordRule {
    /// What to look for in the foreground window.
    #[serde(rename = "match")]
    pub pattern: String,
    /// Preset name to apply while the rule matches.
    pub preset: String,
    /// Whether the rule takes part in evaluation.
    pub enabled: bool,
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
    use crate::windows_mode::ToggleSet;

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
        assert!(shell.launcher_enabled, "the launcher is armed by default");
        assert!(!shell.hide_taskbar, "the taskbar stays visible by default");
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
                launcher_enabled: false,
                hide_taskbar: true,
                bar: BarSettings::default(),
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
        assert!(!back.shell.launcher_enabled);
        assert!(back.shell.hide_taskbar);
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

    #[test]
    fn the_default_widget_list_is_todays_layout() {
        let widgets = BarSettings::default().widgets;
        let order: Vec<(&str, &str)> = widgets
            .iter()
            .map(|widget| (widget.kind.as_str(), widget.align.as_str()))
            .collect();

        assert_eq!(
            order,
            vec![
                ("launcher", "left"),
                ("profile", "left"),
                ("clock", "center"),
                ("date", "center"),
                ("metrics", "right"),
                ("settings", "right"),
            ]
        );
        assert!(widgets.iter().all(|widget| widget.format.is_none()));
    }

    #[test]
    fn a_widget_list_round_trips_with_its_options() {
        let config = Config {
            shell: ShellSettings {
                bar: BarSettings {
                    widgets: vec![
                        BarWidget {
                            kind: "clock".to_owned(),
                            align: "left".to_owned(),
                            format: Some("HH:mm:ss".to_owned()),
                            show_date: Some(false),
                            ..BarWidget::default()
                        },
                        BarWidget {
                            kind: "text".to_owned(),
                            align: "right".to_owned(),
                            text: Some("Ночь".to_owned()),
                            ..BarWidget::default()
                        },
                        BarWidget {
                            kind: "future-widget".to_owned(),
                            align: "center".to_owned(),
                            ..BarWidget::default()
                        },
                    ],
                },
                ..ShellSettings::default()
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");
        let back: Config = serde_json::from_str(&text).expect("parse");

        assert_eq!(back, config);
        assert_eq!(
            back.shell.bar.widgets[0].format.as_deref(),
            Some("HH:mm:ss")
        );
        assert_eq!(back.shell.bar.widgets[0].show_date, Some(false));
        assert_eq!(
            back.shell.bar.widgets[2].kind, "future-widget",
            "an unknown type is preserved for a newer build"
        );
    }

    #[test]
    fn a_shorter_widget_entry_takes_the_defaults_for_the_rest() {
        let config: Config = serde_json::from_str(
            r#"{"shell":{"bar":{"widgets":[{"type":"clock"},{"type":"spacer","align":"right"}]}}}"#,
        )
        .expect("a hand-edited file must load");

        assert_eq!(config.shell.bar.widgets.len(), 2);
        assert_eq!(config.shell.bar.widgets[0].kind, "clock");
        assert_eq!(config.shell.bar.widgets[0].align, "left");
        assert_eq!(config.shell.bar.widgets[0].format, None);
        assert_eq!(config.shell.bar.widgets[1].align, "right");
    }

    #[test]
    fn animations_default_to_on_at_normal_speed() {
        let animations = AnimationSettings::default();

        assert!(animations.enabled);
        assert!((animations.speed() - DEFAULT_ANIMATION_SPEED).abs() < f64::EPSILON);
        assert_eq!(animations.scaled_ms(220), 220);
    }

    #[test]
    fn the_animation_speed_is_clamped_and_scales_the_duration() {
        let animations = |speed| AnimationSettings {
            enabled: true,
            speed,
        };

        assert!((animations(0.1).speed() - MIN_ANIMATION_SPEED).abs() < f64::EPSILON);
        assert!((animations(9.0).speed() - MAX_ANIMATION_SPEED).abs() < f64::EPSILON);
        assert_eq!(animations(2.0).scaled_ms(220), 110);
        assert_eq!(animations(0.5).scaled_ms(220), 440);
    }

    #[test]
    fn a_disabled_animation_is_zero_milliseconds() {
        let disabled = AnimationSettings {
            enabled: false,
            speed: 1.0,
        };

        assert_eq!(disabled.scaled_ms(220), 0);
        assert_eq!(disabled.scaled_ms(16), 0);
    }

    #[test]
    fn a_c_sharp_config_without_animations_or_widgets_gets_the_defaults() {
        let config: Config = serde_json::from_str(r#"{"schemaVersion":1}"#).expect("parse");

        assert_eq!(config.animations, AnimationSettings::default());
        assert_eq!(config.shell.bar, BarSettings::default());
    }

    #[test]
    fn discord_defaults_to_the_honest_not_configured_state() {
        let discord = Config::default().discord;

        assert!(!discord.enabled);
        assert!(discord.app_id.is_empty(), "no placeholder application id");
        assert_eq!(discord.active_preset, None);
        assert!(discord.rules.is_empty());
    }

    #[test]
    fn a_c_sharp_config_without_discord_still_loads() {
        let config: Config = serde_json::from_str(r#"{"schemaVersion":1}"#).expect("parse");

        assert_eq!(config.discord, DiscordSettings::default());
    }

    #[test]
    fn the_discord_section_round_trips_under_the_documented_names() {
        let config = Config {
            discord: DiscordSettings {
                enabled: true,
                app_id: "123456789012345678".to_owned(),
                active_preset: Some("Gaming".to_owned()),
                rules: vec![DiscordRule {
                    pattern: "exe:game.exe".to_owned(),
                    preset: "Gaming".to_owned(),
                    enabled: true,
                }],
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse json");

        assert_eq!(value["discord"]["enabled"], serde_json::json!(true));
        assert_eq!(
            value["discord"]["appId"],
            serde_json::json!("123456789012345678")
        );
        assert_eq!(value["discord"]["activePreset"], "Gaming");
        assert_eq!(value["discord"]["rules"][0]["match"], "exe:game.exe");
        assert_eq!(value["discord"]["rules"][0]["preset"], "Gaming");
        assert_eq!(value["discord"]["rules"][0]["enabled"], true);

        let back: Config = serde_json::from_str(&text).expect("parse back");
        assert_eq!(back, config);
    }

    #[test]
    fn windows_mode_defaults_to_nothing_selected() {
        let mode = WindowsModeSettings::default();

        assert!(!mode.enabled);
        assert!(!mode.toggles.any());
        assert_eq!(Config::default().windows_mode, mode);
    }

    #[test]
    fn a_c_sharp_config_without_windows_mode_still_loads() {
        let config: Config =
            serde_json::from_str(r#"{"schemaVersion":1,"language":"en","theme":{"name":"Dark"}}"#)
                .expect("the C# file must stay readable");

        assert_eq!(config.windows_mode, WindowsModeSettings::default());
        assert!(!config.windows_mode.enabled);
    }

    #[test]
    fn the_windows_mode_section_round_trips_under_the_camel_case_names() {
        let config = Config {
            windows_mode: WindowsModeSettings {
                enabled: true,
                toggles: ToggleSet {
                    desktop_icons: true,
                    search_box: true,
                    ..ToggleSet::default()
                },
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");

        assert!(text.contains(r#""windowsMode":{"enabled":true"#), "{text}");
        assert!(text.contains(r#""desktopIcons":true"#), "{text}");
        assert!(text.contains(r#""searchBox":true"#), "{text}");
        assert!(text.contains(r#""taskbar":false"#), "{text}");

        let back: Config = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, config);
        assert_eq!(back.windows_mode.desired().count(), 2);
    }

    #[test]
    fn the_window_manager_is_off_by_default_with_four_workspaces() {
        let manager = WindowManagerSettings::default();

        assert!(!manager.enabled, "a tiling manager must be opt-in");
        assert_eq!(manager.layout(), DEFAULT_WM_LAYOUT);
        assert_eq!(manager.gaps(), DEFAULT_WM_GAPS);
        assert_eq!(manager.outer_gaps(), DEFAULT_WM_OUTER_GAPS);
        assert_eq!(manager.border(), DEFAULT_WM_BORDER);
        assert_eq!(manager.workspaces(), DEFAULT_WM_WORKSPACES);
        assert!(manager.animations);
        assert!(manager.rules.is_empty());
        assert_eq!(Config::default().window_manager, manager);
    }

    #[test]
    fn a_config_without_a_window_manager_section_still_loads() {
        let config: Config = serde_json::from_str(r#"{"schemaVersion":1}"#).expect("parse");

        assert_eq!(config.window_manager, WindowManagerSettings::default());
        assert!(!config.window_manager.enabled);
    }

    #[test]
    fn the_window_manager_reads_the_camel_case_shape_and_rules() {
        let config: Config = serde_json::from_str(
            r##"{
                "windowManager": {
                    "enabled": true,
                    "layout": "Grid",
                    "gaps": 4,
                    "outerGaps": 6,
                    "border": 2,
                    "workspaces": 3,
                    "animations": false,
                    "rules": [
                        {"match": "notepad\\.exe", "workspace": 2},
                        {"match": "steam", "float": true, "ignore": false}
                    ]
                }
            }"##,
        )
        .expect("parse the manager section");

        let manager = &config.window_manager;
        assert!(manager.enabled);
        assert_eq!(manager.layout(), "Grid");
        assert_eq!(manager.gaps(), 4);
        assert_eq!(manager.outer_gaps(), 6);
        assert_eq!(manager.border(), 2);
        assert_eq!(manager.workspaces(), 3);
        assert!(!manager.animations);
        assert_eq!(manager.rules.len(), 2);
        assert_eq!(manager.rules[0].pattern.as_deref(), Some("notepad\\.exe"));
        assert_eq!(manager.rules[0].workspace, Some(2));
        assert_eq!(manager.rules[1].float, Some(true));
        assert_eq!(manager.rules[1].ignore, Some(false));
    }

    #[test]
    fn the_window_manager_section_round_trips_and_omits_unset_rule_fields() {
        let config = Config {
            window_manager: WindowManagerSettings {
                enabled: true,
                layout: "monocle".to_owned(),
                gaps: 0,
                outer_gaps: 0,
                border: 0,
                workspaces: 9,
                animations: false,
                rules: vec![WindowRule {
                    pattern: Some(".*".to_owned()),
                    float: Some(true),
                    ..WindowRule::default()
                }],
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");
        let back: Config = serde_json::from_str(&text).expect("parse");

        assert_eq!(back, config);
        assert!(
            text.contains(r#""windowManager":{"enabled":true"#),
            "{text}"
        );
        assert!(text.contains(r#""match":".*""#), "{text}");
        assert!(text.contains(r#""workspaces":9"#), "{text}");
        assert!(!text.contains(r#""noBorder""#), "unset fields are omitted");
    }

    #[test]
    fn the_window_manager_values_are_clamped_to_the_supported_range() {
        let manager = WindowManagerSettings {
            layout: "   ".to_owned(),
            gaps: -5,
            outer_gaps: 9_000,
            border: 99,
            workspaces: 0,
            ..WindowManagerSettings::default()
        };

        assert_eq!(manager.layout(), DEFAULT_WM_LAYOUT);
        assert_eq!(manager.gaps(), 0);
        assert_eq!(manager.outer_gaps(), 128);
        assert_eq!(manager.border(), 8);
        assert_eq!(manager.workspaces(), 1);

        let wide = WindowManagerSettings {
            workspaces: 100,
            ..WindowManagerSettings::default()
        };
        assert_eq!(wide.workspaces(), MAX_WM_WORKSPACES);
    }

    #[test]
    fn windhawk_is_off_by_default_and_an_older_file_still_loads() {
        assert_eq!(Config::default().windhawk, WindhawkSettings::default());
        assert!(!Config::default().windhawk.enabled);
        assert!(!Config::default().windhawk.auto_start);
        assert_eq!(Config::default().windhawk.data_dir, None);

        let config: Config = serde_json::from_str(r#"{"schemaVersion":1}"#).expect("parse");
        assert_eq!(config.windhawk, WindhawkSettings::default());
    }

    #[test]
    fn the_windhawk_section_round_trips_under_the_documented_names() {
        let config = Config {
            windhawk: WindhawkSettings {
                enabled: true,
                auto_start: true,
                data_dir: Some(r"D:\WindhawkData".to_owned()),
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");
        let value: serde_json::Value = serde_json::from_str(&text).expect("parse json");

        assert_eq!(value["windhawk"]["enabled"], serde_json::json!(true));
        assert_eq!(value["windhawk"]["autoStart"], serde_json::json!(true));
        assert_eq!(
            value["windhawk"]["dataDir"],
            serde_json::json!(r"D:\WindhawkData")
        );
        assert_eq!(
            serde_json::from_str::<Config>(&text).expect("parse back"),
            config
        );
    }

    #[test]
    fn the_account_section_defaults_to_not_configured() {
        let account = Config::default().account;

        assert!(account.api_base.is_empty(), "no placeholder URL");
        assert!(
            account.device_id.is_empty(),
            "the device id is minted on first use"
        );

        let config: Config = serde_json::from_str(r#"{"schemaVersion":1}"#).expect("parse");
        assert_eq!(config.account, AccountSettings::default());
    }

    #[test]
    fn the_account_section_round_trips_under_the_camel_case_names() {
        let config = Config {
            account: AccountSettings {
                api_base: "https://api.projectneb.example".to_owned(),
                device_id: "0f8fad5b-d9cb-469f-a165-70867728950e".to_owned(),
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");
        let value: Value = serde_json::from_str(&text).expect("json");

        assert_eq!(
            value["account"]["apiBase"],
            "https://api.projectneb.example"
        );
        assert_eq!(
            value["account"]["deviceId"],
            "0f8fad5b-d9cb-469f-a165-70867728950e"
        );
        assert_eq!(serde_json::from_str::<Config>(&text).expect("back"), config);
    }
}
