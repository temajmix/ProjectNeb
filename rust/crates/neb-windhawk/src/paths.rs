//! Where the module keeps its own data and where Windhawk keeps its.
//!
//! ProjectNeb data follows the shared root, so `NEB_HOME` moves the catalog cache along with the
//! configuration:
//!
//! ```text
//! <root>/
//!   config.json                         the shared configuration, including `windhawk`
//!   cache/windhawk-catalog.json         the 6-hour catalog snapshot
//!   cache/windhawk-apply-*.json         one install/remove plan and its result
//! ```
//!
//! Windhawk's own data directory is a separate root. It is `%ProgramData%\Windhawk` unless
//! `windhawk.dataDir` overrides it, which is what lets a live check write into a temporary tree
//! instead of the machine-wide one.

use std::path::{Path, PathBuf};

/// Directory under the ProjectNeb data root that this module owns.
pub const DIR_NAME: &str = "windhawk";

/// File name of the cached catalog inside [`Paths::cache_dir`].
pub const CATALOG_FILE: &str = "windhawk-catalog.json";

/// Directory Windhawk uses when nothing else says otherwise.
pub const DEFAULT_DATA_DIR: &str = r"C:\ProgramData\Windhawk";

/// The layout of the ProjectNeb side, resolved once per command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `<root>`.
    pub root: PathBuf,
    /// `<root>\config.json`.
    pub config_file: PathBuf,
    /// `<root>\cache`.
    pub cache_dir: PathBuf,
    /// `<root>\cache\windhawk-catalog.json`.
    pub catalog_file: PathBuf,
    /// `<root>\windhawk`, reserved for future module-owned state.
    pub data_dir: PathBuf,
}

impl Paths {
    /// The layout under an explicit data root, used by tests and by `NEB_HOME` runs.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let cache_dir = root.join("cache");

        Self {
            config_file: root.join("config.json"),
            catalog_file: cache_dir.join(CATALOG_FILE),
            data_dir: root.join(DIR_NAME),
            cache_dir,
            root,
        }
    }

    /// The layout of the real installation.
    ///
    /// # Errors
    ///
    /// [`neb_core::paths::PathsError::NoDataRoot`] when Windows cannot name a local app data
    /// folder and `NEB_HOME` is unset.
    pub fn try_default() -> Result<Self, neb_core::paths::PathsError> {
        neb_core::paths::try_root().map(Self::at)
    }

    /// A fresh plan/result file name for one helper invocation.
    pub fn apply_file(&self, id: &str, serial: u64) -> PathBuf {
        self.cache_dir.join(format!(
            "windhawk-apply-{}-{serial}-{id}.json",
            std::process::id()
        ))
    }
}

/// The effective Windhawk data directory: the explicit override, else `%ProgramData%\Windhawk`.
///
/// `detected` is the `AppDataPath` a real installation names in its `windhawk.ini`, when one was
/// found. The override wins over both, because it is the user's (or a test's) deliberate choice.
pub fn windhawk_data_dir(override_dir: Option<&str>, detected: Option<&Path>) -> PathBuf {
    if let Some(value) = override_dir
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return PathBuf::from(value);
    }

    detected
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_DATA_DIR))
}

/// `ModsSource` under a Windhawk data directory.
pub fn mods_source(data_dir: &Path) -> PathBuf {
    data_dir.join("ModsSource")
}

/// `Engine\Mods` under a Windhawk data directory.
pub fn engine_mods(data_dir: &Path) -> PathBuf {
    data_dir.join("Engine").join("Mods")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_project_neb_layout_hangs_off_one_root() {
        let paths = Paths::at(r"D:\NebData");

        assert_eq!(paths.config_file, PathBuf::from(r"D:\NebData\config.json"));
        assert_eq!(
            paths.catalog_file,
            PathBuf::from(r"D:\NebData\cache\windhawk-catalog.json")
        );
        assert_eq!(paths.data_dir, PathBuf::from(r"D:\NebData\windhawk"));
    }

    #[test]
    fn the_default_layout_ends_with_the_owned_names() {
        let paths = Paths::try_default().expect("this machine has a data directory");

        assert!(paths.catalog_file.ends_with(CATALOG_FILE));
        assert!(paths.cache_dir.ends_with("cache"));
    }

    #[test]
    fn an_override_beats_a_detected_data_directory() {
        let detected = Path::new(r"E:\Detected\Windhawk");

        assert_eq!(
            windhawk_data_dir(Some(r"F:\Temp\Windhawk"), Some(detected)),
            PathBuf::from(r"F:\Temp\Windhawk")
        );
        assert_eq!(
            windhawk_data_dir(Some("   "), Some(detected)),
            detected.to_path_buf()
        );
        assert_eq!(
            windhawk_data_dir(None, None),
            PathBuf::from(DEFAULT_DATA_DIR)
        );
    }

    #[test]
    fn mods_live_under_the_documented_engine_folders() {
        let data = Path::new(r"C:\ProgramData\Windhawk");

        assert_eq!(mods_source(data), data.join("ModsSource"));
        assert_eq!(engine_mods(data), data.join("Engine").join("Mods"));
    }
}
