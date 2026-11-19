//! Profiles: a named bundle of a theme id plus the shell settings the bar needs.
//!
//! A profile file lives under `%LocalAppData%\ProjectNeb\profiles\<id>.json` and looks like this:
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "id": "night",
//!   "name": "Night",
//!   "themeId": "minimal-dark",
//!   "shell": {
//!     "barEnabled": true,
//!     "barPosition": "top",
//!     "barHeight": 36,
//!     "dockEnabled": true,
//!     "dockAutoHide": true,
//!     "launcherEnabled": true,
//!     "hideTaskbar": false,
//!     "pinnedApps": []
//!   }
//! }
//! ```
//!
//! Applying a profile writes `config.json` through [`ConfigStore`], which is atomic and keeps unknown
//! config keys, and returns the new [`Config`]. The caller posts `shell.reload` afterwards; the store
//! never talks to the shell itself, so the settings window and the supervisor use the same path.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{pretty_json, write_atomic};
use crate::config::{
    BarSettings, Config, ConfigStore, DEFAULT_THEME_ID, MAX_BAR_HEIGHT, MIN_BAR_HEIGHT,
    ShellSettings,
};
use crate::theming::is_valid_id;

/// Version of the profile shape this build writes.
pub const PROFILE_SCHEMA_VERSION: u32 = 1;

/// Longest profile name accepted.
const MAX_NAME: usize = 80;

/// The shell settings a profile owns, minus the theme id (which is kept at the profile root).
///
/// The field list mirrors the Rust and C# `ShellSettings`; unknown keys inside the snapshot are kept
/// in [`extra`](Self::extra) so a newer build's flag survives a save by this one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ShellSnapshot {
    /// Whether bars are shown at all.
    pub bar_enabled: bool,
    /// Edge the bar takes: `top` or `bottom`.
    pub bar_position: String,
    /// Bar height in device-independent pixels.
    pub bar_height: i32,
    /// Whether the dock window exists.
    pub dock_enabled: bool,
    /// Whether the dock only appears when the pointer reaches its edge.
    pub dock_auto_hide: bool,
    /// Whether the `Alt+Space` launcher is armed.
    pub launcher_enabled: bool,
    /// Whether the Windows taskbar is hidden while the shell runs.
    pub hide_taskbar: bool,
    /// Applications pinned to the dock, in order.
    pub pinned_apps: Vec<String>,
    /// What the bar draws, in order.
    pub bar: BarSettings,
    /// Keys written by another build; carried through untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for ShellSnapshot {
    fn default() -> Self {
        Self::from(&ShellSettings::default())
    }
}

impl From<&ShellSettings> for ShellSnapshot {
    fn from(shell: &ShellSettings) -> Self {
        Self {
            bar_enabled: shell.bar_enabled,
            bar_position: shell.bar_position.clone(),
            bar_height: shell.bar_height,
            dock_enabled: shell.dock_enabled,
            dock_auto_hide: shell.dock_auto_hide,
            launcher_enabled: shell.launcher_enabled,
            hide_taskbar: shell.hide_taskbar,
            pinned_apps: shell.pinned_apps.clone(),
            bar: shell.bar.clone(),
            extra: Map::new(),
        }
    }
}

impl ShellSnapshot {
    /// Copies the snapshot into `shell`, clamping the bar height into the range the settings allow.
    pub fn apply_to(&self, shell: &mut ShellSettings) {
        shell.bar_enabled = self.bar_enabled;
        shell.bar_position = self.bar_position.clone();
        shell.bar_height = self.bar_height.clamp(MIN_BAR_HEIGHT, MAX_BAR_HEIGHT);
        shell.dock_enabled = self.dock_enabled;
        shell.dock_auto_hide = self.dock_auto_hide;
        shell.launcher_enabled = self.launcher_enabled;
        shell.hide_taskbar = self.hide_taskbar;
        shell.pinned_apps = self.pinned_apps.clone();
        shell.bar = self.bar.clone();
    }
}

/// A named bundle of a theme and the shell settings it goes with.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Profile {
    /// Version of the profile shape.
    pub schema_version: u32,
    /// Stable id, which is also the file name.
    pub id: String,
    /// Name shown in the settings window.
    pub name: String,
    /// Theme the profile paints with.
    pub theme_id: String,
    /// Bar, dock and launcher settings.
    pub shell: ShellSnapshot,
    /// Keys written by another build; carried through untouched.
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Default for Profile {
    fn default() -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            id: String::new(),
            name: String::new(),
            theme_id: DEFAULT_THEME_ID.to_owned(),
            shell: ShellSnapshot::default(),
            extra: Map::new(),
        }
    }
}

impl Profile {
    /// Captures the current theme and shell settings under `id`.
    pub fn capture(id: impl Into<String>, name: impl Into<String>, config: &Config) -> Self {
        Self {
            schema_version: PROFILE_SCHEMA_VERSION,
            id: id.into().trim().to_owned(),
            name: name.into().trim().to_owned(),
            theme_id: config.shell.theme().to_owned(),
            shell: ShellSnapshot::from(&config.shell),
            extra: Map::new(),
        }
    }

    /// Validates a parsed JSON document.
    ///
    /// # Errors
    ///
    /// [`ProfileError::Invalid`] naming the first field that is not acceptable.
    pub fn from_value(value: &Value) -> Result<Self, ProfileError> {
        let mut profile: Self = serde_json::from_value(value.clone())
            .map_err(|error| ProfileError::Invalid(format!("not a profile: {error}")))?;

        if profile.schema_version != PROFILE_SCHEMA_VERSION {
            return Err(ProfileError::Invalid(format!(
                "schemaVersion must be {PROFILE_SCHEMA_VERSION}"
            )));
        }
        if !is_valid_id(&profile.id) {
            return Err(ProfileError::Invalid(format!(
                "'{}' is not a usable profile id",
                profile.id
            )));
        }
        if profile.name.trim().is_empty() {
            profile.name = profile.id.clone();
        }
        if profile.name.chars().count() > MAX_NAME {
            return Err(ProfileError::Invalid(format!(
                "'name' is longer than {MAX_NAME} characters"
            )));
        }
        if profile.theme_id.trim().is_empty() {
            profile.theme_id = DEFAULT_THEME_ID.to_owned();
        }
        if !is_valid_id(&profile.theme_id) {
            return Err(ProfileError::Invalid(format!(
                "'{}' is not a usable theme id",
                profile.theme_id
            )));
        }

        Ok(profile)
    }

    /// Renders the profile as the JSON value written to disk.
    pub fn to_value(&self) -> Result<Value, ProfileError> {
        serde_json::to_value(self)
            .map_err(|error| ProfileError::Invalid(format!("profile is not serialisable: {error}")))
    }

    /// Copies the profile's theme id and shell settings into `shell`.
    pub fn apply_to_shell(&self, shell: &mut ShellSettings) {
        shell.theme_id = self.theme_id.clone();
        self.shell.apply_to(shell);
    }
}

/// Why a profile could not be read, validated, applied or written.
#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    /// The file system refused the operation.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `write`.
        action: &'static str,
        /// File or folder the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The document is not a usable profile.
    #[error("invalid profile: {0}")]
    Invalid(String),

    /// No profile file answers for the id.
    #[error("profile '{0}' was not found")]
    NotFound(String),

    /// The configuration could not be loaded or saved.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

impl From<serde_json::Error> for ProfileError {
    fn from(error: serde_json::Error) -> Self {
        Self::Invalid(error.to_string())
    }
}

/// Loads, saves, applies and deletes profiles in one folder.
#[derive(Debug, Clone)]
pub struct ProfileStore {
    dir: PathBuf,
}

impl ProfileStore {
    /// A store for an explicit folder, used by tests and portable runs.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store for `%LocalAppData%\ProjectNeb\profiles`.
    pub fn at_default_location() -> Self {
        Self::new(crate::paths::profiles_dir())
    }

    /// The folder this store owns.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every readable profile, sorted by id. A missing folder is an empty list; a broken file is
    /// skipped so one hand edit cannot hide the others.
    pub fn list(&self) -> Vec<Profile> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };

        let mut profiles: Vec<Profile> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .filter_map(|path| read_profile(&path).ok())
            .collect();
        profiles.sort_by(|left, right| left.id.cmp(&right.id));

        profiles
    }

    /// The profile called `id`, if the file exists and is readable.
    pub fn get(&self, id: &str) -> Option<Profile> {
        self.path_for(id).and_then(|path| read_profile(&path).ok())
    }

    /// Validates and writes `profile` as `<dir>/<id>.json`, atomically.
    ///
    /// # Errors
    ///
    /// [`ProfileError::Invalid`] for an unusable id, name or theme id; [`ProfileError::Io`] when the
    /// folder or file cannot be written.
    pub fn save(&self, profile: &Profile) -> Result<PathBuf, ProfileError> {
        let checked = Profile::from_value(&profile.to_value()?)?;
        let text = pretty_json(&checked.to_value()?)?;
        let path = self.path_for(&checked.id).ok_or_else(|| {
            ProfileError::Invalid(format!("'{}' is not a usable profile id", checked.id))
        })?;

        write_atomic(&path, text.as_bytes()).map_err(|source| io_error("write", &path, source))?;

        Ok(path)
    }

    /// Removes the profile called `id`.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NotFound`] when there is no file, [`ProfileError::Io`] when the file system
    /// refuses.
    pub fn delete(&self, id: &str) -> Result<(), ProfileError> {
        let path = self
            .path_for(id)
            .ok_or_else(|| ProfileError::Invalid(format!("'{id}' is not a usable profile id")))?;

        fs::remove_file(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ProfileError::NotFound(id.to_owned())
            } else {
                io_error("remove", &path, source)
            }
        })
    }

    /// Applies the profile called `id` to `config.json` and returns the new configuration.
    ///
    /// The write is [`ConfigStore::save`], so it is atomic, serialised against other savers and
    /// leaves every unknown config key intact. The caller posts `shell.reload` with the result.
    ///
    /// # Errors
    ///
    /// [`ProfileError::NotFound`] when no profile answers, [`ProfileError::Config`] when the config
    /// cannot be read or written.
    pub fn apply_to_config(&self, id: &str, store: &ConfigStore) -> Result<Config, ProfileError> {
        let profile = self
            .get(id)
            .ok_or_else(|| ProfileError::NotFound(id.to_owned()))?;

        let mut config = store.load_checked()?;
        profile.apply_to_shell(&mut config.shell);
        store.save(&config)?;

        Ok(config)
    }

    /// The file for `id`, or `None` when the id could escape the folder.
    fn path_for(&self, id: &str) -> Option<PathBuf> {
        is_valid_id(id).then(|| self.dir.join(format!("{}.json", id.trim())))
    }
}

/// Reads and validates one profile file.
fn read_profile(path: &Path) -> Result<Profile, ProfileError> {
    let text = fs::read_to_string(path).map_err(|source| io_error("read", path, source))?;
    let value: Value = serde_json::from_str(&text)?;

    Profile::from_value(&value)
}

/// Builds a typed file-system error.
fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> ProfileError {
    ProfileError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{BarWidget, ConfigStore};
    use tempfile::tempdir;

    fn sample() -> Profile {
        let config = Config {
            shell: ShellSettings {
                bar_position: "bottom".to_owned(),
                bar_height: 48,
                dock_auto_hide: false,
                launcher_enabled: false,
                hide_taskbar: true,
                pinned_apps: vec![r"C:\Apps\editor.exe".to_owned()],
                bar: BarSettings {
                    widgets: vec![BarWidget {
                        kind: "clock".to_owned(),
                        align: "center".to_owned(),
                        ..BarWidget::default()
                    }],
                },
                ..ShellSettings::default()
            },
            ..Config::default()
        };

        Profile::capture("night", "Night", &config)
    }

    fn store() -> (tempfile::TempDir, ProfileStore, ConfigStore) {
        let dir = tempdir().expect("temp dir");
        let profiles = ProfileStore::new(dir.path().join("profiles"));
        let config = ConfigStore::new(dir.path().join("config.json"));
        (dir, profiles, config)
    }

    #[test]
    fn capture_takes_the_theme_and_every_shell_flag() {
        let profile = sample();

        assert_eq!(profile.id, "night");
        assert_eq!(profile.theme_id, DEFAULT_THEME_ID);
        assert_eq!(profile.shell.bar_position, "bottom");
        assert_eq!(profile.shell.bar_height, 48);
        assert!(!profile.shell.dock_auto_hide);
        assert!(!profile.shell.launcher_enabled);
        assert!(profile.shell.hide_taskbar);
        assert_eq!(profile.shell.pinned_apps.len(), 1);
        assert_eq!(profile.shell.bar.widgets[0].kind, "clock");
    }

    #[test]
    fn save_list_get_delete_round_trip() {
        let (_dir, profiles, _config) = store();
        let profile = sample();

        let path = profiles.save(&profile).expect("save");
        assert!(path.is_file());
        assert_eq!(profiles.get("night"), Some(profile.clone()));
        assert_eq!(profiles.list(), vec![profile.clone()]);

        profiles.delete("night").expect("delete");
        assert!(profiles.get("night").is_none());
        assert!(matches!(
            profiles.delete("night"),
            Err(ProfileError::NotFound(_))
        ));
    }

    #[test]
    fn applying_a_profile_writes_the_config_and_returns_it() {
        let (dir, profiles, config_store) = store();
        let profile = sample();
        profiles.save(&profile).expect("save");

        let config = profiles
            .apply_to_config("night", &config_store)
            .expect("apply");

        assert_eq!(config.shell.theme_id, profile.theme_id);
        assert!(config.shell.is_bottom());
        assert_eq!(config.shell.bar_height, 48);
        assert!(!config.shell.dock_auto_hide);
        assert!(!config.shell.launcher_enabled);
        assert!(config.shell.hide_taskbar);
        assert_eq!(config.shell.bar.widgets[0].kind, "clock");
        assert_eq!(
            config,
            config_store
                .load_checked()
                .expect("the file must match the return")
        );

        let names: Vec<String> = fs::read_dir(dir.path())
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        assert_eq!(names, vec!["config.json".to_owned(), "profiles".to_owned()]);
    }

    #[test]
    fn applying_a_missing_profile_is_a_typed_not_found() {
        let (_dir, profiles, config_store) = store();

        assert!(matches!(
            profiles.apply_to_config("absent", &config_store),
            Err(ProfileError::NotFound(_))
        ));
        assert!(!config_store.path().exists(), "nothing may be written");
    }

    #[test]
    fn a_profile_id_that_could_escape_the_folder_is_refused() {
        for id in ["../config", r"a\b", "a/b", "..", ".hidden", "   "] {
            let mut profile = sample();
            profile.id = id.to_owned();

            assert!(
                profiles_reject(&profile),
                "'{id}' must not become a profile id"
            );
        }
    }

    fn profiles_reject(profile: &Profile) -> bool {
        let dir = tempdir().expect("temp dir");
        let profiles = ProfileStore::new(dir.path());
        profiles.save(profile).is_err()
    }

    #[test]
    fn unknown_fields_survive_a_save() {
        let (_dir, profiles, _config) = store();
        let mut profile = sample();
        profile
            .extra
            .insert("futureProfileField".to_owned(), serde_json::json!(true));
        profile
            .shell
            .extra
            .insert("futureShellFlag".to_owned(), serde_json::json!("yes"));

        profiles.save(&profile).expect("save");
        let back = profiles.get("night").expect("read");

        assert_eq!(back.extra["futureProfileField"], serde_json::json!(true));
        assert_eq!(
            back.shell.extra["futureShellFlag"],
            serde_json::json!("yes")
        );
    }

    #[test]
    fn a_hand_edited_profile_without_optional_fields_still_loads() {
        let (_dir, profiles, _config) = store();
        fs::create_dir_all(profiles.dir()).expect("dir");
        fs::write(
            profiles.dir().join("plain.json"),
            r#"{"id":"plain","themeId":"cyberpunk"}"#,
        )
        .expect("write");

        let profile = profiles.get("plain").expect("a sparse profile loads");

        assert_eq!(profile.name, "plain");
        assert_eq!(profile.theme_id, "cyberpunk");
        assert!(profile.shell.bar_enabled);
        assert_eq!(profile.shell.bar_height, 40);
    }

    #[test]
    fn the_applied_bar_height_is_clamped() {
        let mut profile = sample();
        profile.shell.bar_height = 5_000;
        let mut shell = ShellSettings::default();

        profile.apply_to_shell(&mut shell);

        assert_eq!(shell.bar_height, MAX_BAR_HEIGHT);
    }

    #[test]
    fn the_default_store_points_at_the_data_root() {
        assert_eq!(
            ProfileStore::at_default_location().dir(),
            crate::paths::profiles_dir()
        );
    }

    #[test]
    fn a_broken_file_is_skipped_but_the_others_survive() {
        let (_dir, profiles, _config) = store();
        profiles.save(&sample()).expect("save");
        fs::write(profiles.dir().join("broken.json"), "{ nope").expect("write");

        assert_eq!(profiles.list().len(), 1);
    }
}
