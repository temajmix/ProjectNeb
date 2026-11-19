//! The persisted application configuration and its atomic store.

mod model;
mod performance;
mod store;

pub use model::{
    AnimationSettings, BarSettings, BarWidget, Config, DEFAULT_ANIMATION_SPEED, DEFAULT_BAR_HEIGHT,
    DEFAULT_LANGUAGE, DEFAULT_ROTATE_MINUTES, DEFAULT_THEME_ID, DEFAULT_WALLPAPER_COLOR,
    DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, DEFAULT_WM_BORDER, DEFAULT_WM_GAPS,
    DEFAULT_WM_LAYOUT, DEFAULT_WM_OUTER_GAPS, DEFAULT_WM_WORKSPACES, DiscordRule, DiscordSettings,
    MAX_ANIMATION_SPEED, MAX_BAR_HEIGHT, MAX_WM_WORKSPACES, MIN_ANIMATION_SPEED, MIN_BAR_HEIGHT,
    ModuleStates, SCHEMA_VERSION, SceneKind, ShellSettings, ThemeName, ThemeSettings,
    WallpaperSettings, WindhawkSettings, WindowManagerSettings, WindowRule, WindowSettings,
    ZapretSettings, default_widgets,
};
pub use performance::{
    DEFAULT_PROFILE, MAX_METRICS_MS, MAX_UPDATE_CHECK_HOURS, MAX_WALLPAPER_FPS, MIN_METRICS_MS,
    MIN_UPDATE_CHECK_HOURS, MIN_WALLPAPER_FPS, PROFILE_ANIMATION_SPEED, Performance,
    PerformanceProfile, PerformanceSettings,
};
pub use store::{ConfigError, ConfigStore};

pub use crate::windows_mode::{ToggleSet, WindowsModeSettings};
