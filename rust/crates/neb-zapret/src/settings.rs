//! The `zapret` slice of `config.json` and how a pack path is chosen.
//!
//! The manager and the elevated helper must agree on which folder is the pack even though the
//! helper is started with an explicit `--pack`. This module is that single answer: the configured
//! path wins when it is usable, the imported copy under the data directory is the fallback, and a
//! folder that is neither is reported instead of guessed.

use std::path::{Path, PathBuf};

use neb_core::config::{Config, ConfigStore, ZapretSettings};

use crate::pack::Pack;
use crate::paths::Paths;

/// The result of looking for a pack.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// The usable pack, when there is one.
    pub pack: Option<Pack>,
    /// A note about a configured path that was rejected, for the state document.
    pub note: Option<String>,
}

/// Reads the shared config, falling back to defaults exactly like the rest of ProjectNeb.
pub fn load(paths: &Paths) -> Config {
    ConfigStore::new(&paths.config_file).load()
}

/// The `zapret` section of the config at `paths`.
pub fn section(paths: &Paths) -> ZapretSettings {
    load(paths).zapret
}

/// Picks the pack: the configured folder when it is valid, the imported copy otherwise.
///
/// A configured folder that does not contain the two required files is not fatal. The imported copy
/// is still usable, so the manager keeps running and reports the rejected path in [`Resolved::note`].
pub fn resolve(config: &Config, paths: &Paths) -> Resolved {
    let imported = Pack::open(&paths.data_dir);

    let configured = config
        .zapret
        .pack_path
        .as_deref()
        .map(str::trim)
        .filter(|path| !path.is_empty());

    let Some(configured) = configured else {
        return Resolved {
            pack: imported.ok(),
            note: None,
        };
    };

    match Pack::open(configured) {
        Ok(pack) => Resolved {
            pack: Some(pack),
            note: None,
        },
        Err(error) => match imported {
            Ok(pack) => Resolved {
                pack: Some(pack),
                note: Some(format!(
                    "configured pack '{}' is not usable ({error}); using the imported copy",
                    configured
                )),
            },
            Err(_) => Resolved {
                pack: None,
                note: Some(format!(
                    "configured pack '{}' is not usable: {error}",
                    configured
                )),
            },
        },
    }
}

/// The pack folder the helper should open, from an explicit path or the configuration.
pub fn pack_path(explicit: Option<&Path>, config: &Config, paths: &Paths) -> Option<PathBuf> {
    explicit.map(Path::to_path_buf).or_else(|| {
        resolve(config, paths)
            .pack
            .map(|pack| pack.root().to_path_buf())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{BIN_DIR, WINDIVERT_SYS, WINWS_EXE};
    use std::fs;
    use tempfile::tempdir;

    fn write_pack(root: &Path) {
        fs::create_dir_all(root.join(BIN_DIR)).expect("bin");
        fs::write(root.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        fs::write(root.join(BIN_DIR).join(WINDIVERT_SYS), b"sys").expect("driver");
    }

    #[test]
    fn without_a_configured_path_the_imported_copy_is_used() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        write_pack(&paths.data_dir);

        let resolved = resolve(&Config::default(), &paths);

        assert_eq!(
            resolved.pack.as_ref().map(|pack| pack.root.clone()),
            Some(paths.data_dir),
            "the imported folder is the default"
        );
        assert!(resolved.note.is_none());
    }

    #[test]
    fn a_valid_configured_path_beats_the_imported_copy() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        write_pack(&paths.data_dir);
        let configured = directory.path().join("my-zapret");
        write_pack(&configured);

        let mut config = Config::default();
        config.zapret.pack_path = Some(configured.display().to_string());
        let resolved = resolve(&config, &paths);

        assert_eq!(
            resolved.pack.as_ref().map(|pack| pack.root.clone()),
            Some(configured)
        );
        assert!(resolved.note.is_none());
    }

    #[test]
    fn a_broken_configured_path_falls_back_with_a_note() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        write_pack(&paths.data_dir);

        let mut config = Config::default();
        config.zapret.pack_path = Some(directory.path().join("gone").display().to_string());
        let resolved = resolve(&config, &paths);

        assert_eq!(
            resolved.pack.as_ref().map(|pack| pack.root.clone()),
            Some(paths.data_dir)
        );
        assert!(
            resolved
                .note
                .is_some_and(|note| note.contains("using the imported copy"))
        );
    }

    #[test]
    fn nothing_installed_resolves_to_nothing() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());

        let resolved = resolve(&Config::default(), &paths);

        assert!(resolved.pack.is_none());
        assert!(resolved.note.is_none());
    }

    #[test]
    fn an_explicit_helper_path_wins_over_the_configuration() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        let explicit = directory.path().join("explicit");

        assert_eq!(
            pack_path(Some(&explicit), &Config::default(), &paths),
            Some(explicit)
        );
    }
}
