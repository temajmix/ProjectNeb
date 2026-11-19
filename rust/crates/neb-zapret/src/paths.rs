//! Where the module keeps its data.
//!
//! Everything lives under the ProjectNeb data root, so `NEB_HOME` moves the imported pack together
//! with the config and the logs:
//!
//! ```text
//! <root>/
//!   config.json            the shared configuration, including `zapret`
//!   zapret/                the imported pack, or the status of an external one
//!     bin/winws.exe
//!     bin/WinDivert64.sys
//!     lists/…
//!     utils/…
//!     general*.bat
//!     status.json          written by the elevated helper
//! ```

use std::path::PathBuf;
/// Directory the module owns under the ProjectNeb data root.
pub const DIR_NAME: &str = "zapret";

/// File the elevated helper writes after every transition.
pub const STATUS_FILE: &str = "status.json";

/// A path layout resolved once, so every command in one run addresses the same files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `<root>\zapret`: import target and status location.
    pub data_dir: PathBuf,
    /// `<root>\zapret\status.json`.
    pub status_file: PathBuf,
    /// `<root>\config.json`.
    pub config_file: PathBuf,
}

impl Paths {
    /// The layout under an explicit data root, used by tests and by `NEB_HOME` runs.
    pub fn at(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        let data_dir = root.join(DIR_NAME);

        Self {
            status_file: data_dir.join(STATUS_FILE),
            data_dir,
            config_file: root.join("config.json"),
        }
    }

    /// The layout of the real installation.
    ///
    /// # Errors
    ///
    /// [`neb_core::paths::PathsError::NoDataRoot`] when Windows cannot name a local app data folder
    /// and `NEB_HOME` is unset.
    pub fn try_default() -> Result<Self, neb_core::paths::PathsError> {
        neb_core::paths::try_root().map(Self::at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_file_hangs_off_one_data_directory() {
        let paths = Paths::at(r"D:\NebData");

        assert_eq!(paths.data_dir, PathBuf::from(r"D:\NebData\zapret"));
        assert_eq!(
            paths.status_file,
            PathBuf::from(r"D:\NebData\zapret\status.json")
        );
        assert_eq!(paths.config_file, PathBuf::from(r"D:\NebData\config.json"));
    }

    #[test]
    fn the_default_layout_ends_with_the_owned_directory() {
        let paths = Paths::try_default().expect("this machine has a data directory");

        assert!(paths.data_dir.ends_with(DIR_NAME));
        assert!(paths.status_file.ends_with(STATUS_FILE));
    }
}
