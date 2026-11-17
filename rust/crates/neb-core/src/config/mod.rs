//! The persisted application configuration and its atomic store.

mod model;
mod store;

pub use model::{
    Config, DEFAULT_BAR_HEIGHT, DEFAULT_LANGUAGE, DEFAULT_ROTATE_MINUTES, DEFAULT_THEME_ID,
    DEFAULT_WALLPAPER_COLOR, DEFAULT_WINDOW_HEIGHT, DEFAULT_WINDOW_WIDTH, MAX_BAR_HEIGHT,
    MIN_BAR_HEIGHT, ModuleStates, SCHEMA_VERSION, SceneKind, ShellSettings, ThemeName,
    ThemeSettings, WallpaperSettings, WindowSettings,
};
pub use store::{ConfigError, ConfigStore};
