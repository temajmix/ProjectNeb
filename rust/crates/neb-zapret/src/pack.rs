//! Finding, validating and importing a zapret pack.
//!
//! A pack is the folder the user downloaded: `bin\winws.exe` with its WinDivert driver,
//! `lists\`, `utils\` and the `general*.bat` strategy scripts. ProjectNeb never ships those files;
//! it copies the user's folder into its own data directory (or points at it in place) and records a
//! version marker so the UI can notice that the folder changed under it.
//!
//! The marker is a wrapping sum of every size and modification time under `bin`, `lists`, `utils`
//! and the top-level `*.bat` files. It is deliberately cheap: a pack has fewer than a hundred files,
//! and the value is compared for equality, not used for integrity.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process;
use std::time::UNIX_EPOCH;

/// Name of the executable every strategy invokes.
pub const WINWS_EXE: &str = "winws.exe";

/// Name of the driver file `winws.exe` loads through the WinDivert service.
pub const WINDIVERT_SYS: &str = "WinDivert64.sys";

/// Subdirectory holding `winws.exe`, the driver and the fake-packet blobs.
pub const BIN_DIR: &str = "bin";

/// Subdirectory holding the host and ipset lists.
pub const LISTS_DIR: &str = "lists";

/// Subdirectory holding the pack's helper scripts and toggles.
pub const UTILS_DIR: &str = "utils";

/// A pack whose two required files exist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pack {
    /// Folder containing `bin`, `lists`, `utils` and the strategy scripts.
    pub root: PathBuf,
    /// [`version_marker`] of this folder, as a hexadecimal string.
    pub version: String,
}

impl Pack {
    /// Opens `root` as a pack, refusing folders `winws.exe` could not run from.
    ///
    /// # Errors
    ///
    /// [`PackError::NotADirectory`] when `root` is not a folder, [`PackError::MissingFile`] when
    /// either `bin\winws.exe` or `bin\WinDivert64.sys` is absent, [`PackError::Io`] when the version
    /// marker cannot be computed.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self, PackError> {
        let root = root.into();
        // The helper is started with `--pack`, and an elevated process does not necessarily share
        // this process's working directory. A relative folder is therefore pinned to an absolute
        // one here, without canonicalising (a `\\?\` prefix can confuse the pack's own scripts).
        let root = if root.is_absolute() {
            root
        } else {
            match std::env::current_dir() {
                Ok(directory) => directory.join(root),
                Err(_) => root,
            }
        };

        if !root.is_dir() {
            return Err(PackError::NotADirectory { path: root });
        }

        validate_required_files(&root)?;
        let version = version_marker(&root)?;

        Ok(Self { root, version })
    }

    /// `bin\winws.exe` of this pack.
    pub fn winws(&self) -> PathBuf {
        self.root.join(BIN_DIR).join(WINWS_EXE)
    }

    /// `bin\WinDivert64.sys` of this pack.
    pub fn windivert(&self) -> PathBuf {
        self.root.join(BIN_DIR).join(WINDIVERT_SYS)
    }

    /// `bin\`, the working directory `winws.exe` expects.
    pub fn bin_dir(&self) -> PathBuf {
        self.root.join(BIN_DIR)
    }

    /// The pack as a path, for a caller that only needs the folder.
    pub fn root(&self) -> &Path {
        &self.root
    }
}

/// Why a folder is not usable as a zapret pack, or why an import failed.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    /// The path is not a directory.
    #[error("'{}' is not a folder", path.display())]
    NotADirectory {
        /// The rejected path.
        path: PathBuf,
    },

    /// A required file is absent.
    #[error("'{}' is missing from the pack", path.display())]
    MissingFile {
        /// The file that must exist.
        path: PathBuf,
    },

    /// The file system refused an operation.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `copy` or `read`.
        action: &'static str,
        /// Path the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The source and the import target are the same folder.
    #[error("'{}' is already the import target", path.display())]
    SameFolder {
        /// The folder that was both source and target.
        path: PathBuf,
    },

    /// One of the two folders contains the other, so copying would recurse or destroy the source.
    #[error("'{}' and '{}' overlap", from.display(), target.display())]
    Overlapping {
        /// The folder the user asked to import.
        from: PathBuf,
        /// The import target.
        target: PathBuf,
    },
}

/// Checks the two files without paying for the version marker.
///
/// # Errors
///
/// [`PackError::NotADirectory`] or [`PackError::MissingFile`].
pub fn validate(root: &Path) -> Result<(), PackError> {
    if !root.is_dir() {
        return Err(PackError::NotADirectory {
            path: root.to_path_buf(),
        });
    }

    validate_required_files(root)
}

fn validate_required_files(root: &Path) -> Result<(), PackError> {
    for path in [
        root.join(BIN_DIR).join(WINWS_EXE),
        root.join(BIN_DIR).join(WINDIVERT_SYS),
    ] {
        if !path.is_file() {
            return Err(PackError::MissingFile { path });
        }
    }

    Ok(())
}

/// Copies the usable parts of `source` into `target` and opens the result.
///
/// Only `bin`, `lists`, `utils` and the top-level `*.bat` files are copied: those are what a
/// strategy reads. The copy is staged in a sibling directory and swapped in, so an interrupted
/// import leaves the previous pack untouched.
///
/// # Errors
///
/// [`PackError::MissingFile`] before anything is copied when the source is not a pack,
/// [`PackError::SameFolder`] or [`PackError::Overlapping`] when the paths make a copy unsafe, and
/// [`PackError::Io`] for a failed copy or rename.
pub fn import(source: &Path, target: &Path) -> Result<Pack, PackError> {
    validate(source)?;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent).map_err(|source| io_error("create", parent, source))?;
    }

    let source_key = canonical_key(source)?;
    let target_key = canonical_key(target)?;

    if source_key == target_key {
        return Err(PackError::SameFolder {
            path: source.to_path_buf(),
        });
    }

    if source_key.starts_with(&format!("{target_key}\\"))
        || target_key.starts_with(&format!("{source_key}\\"))
    {
        return Err(PackError::Overlapping {
            from: source.to_path_buf(),
            target: target.to_path_buf(),
        });
    }

    let name = target
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "zapret".to_owned());
    let staging = target.with_file_name(format!("{name}.import-{}", process::id()));

    if staging.exists() {
        fs::remove_dir_all(&staging).map_err(|source| io_error("remove", &staging, source))?;
    }

    match copy_selected(source, &staging) {
        Ok(()) => {}
        Err(error) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(error);
        }
    }

    if let Err(error) = validate_required_files(&staging) {
        let _ = fs::remove_dir_all(&staging);
        return Err(error);
    }

    let backup = target.with_file_name(format!("{name}.old-{}", process::id()));
    let had_target = target.exists();

    if had_target {
        if backup.exists() {
            let _ = fs::remove_dir_all(&backup);
        }
        fs::rename(target, &backup).map_err(|source| io_error("replace", target, source))?;
    }

    if let Err(source_error) = fs::rename(&staging, target) {
        if had_target {
            let _ = fs::rename(&backup, target);
        }
        let _ = fs::remove_dir_all(&staging);
        return Err(io_error("replace", target, source_error));
    }

    if had_target {
        let _ = fs::remove_dir_all(&backup);
    }

    Pack::open(target)
}

/// A stable key for overlap checks: absolute, lowercase, no trailing separator.
fn canonical_key(path: &Path) -> Result<String, PackError> {
    let canonical = if path.exists() {
        fs::canonicalize(path).map_err(|source| io_error("resolve", path, source))?
    } else {
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|source| io_error("create", parent, source))?;
        let parent =
            fs::canonicalize(parent).map_err(|source| io_error("resolve", parent, source))?;
        match path.file_name() {
            Some(name) => parent.join(name),
            None => parent,
        }
    };

    Ok(canonical
        .to_string_lossy()
        .replace('/', "\\")
        .trim_end_matches('\\')
        .to_ascii_lowercase())
}

/// Copies the files a strategy can reference, creating `destination`.
fn copy_selected(source: &Path, destination: &Path) -> Result<(), PackError> {
    fs::create_dir_all(destination).map_err(|source| io_error("create", destination, source))?;

    for entry in read_dir(source)? {
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        let from = entry.path();
        let to = destination.join(&name);

        if from.is_dir() {
            if name_text.eq_ignore_ascii_case(BIN_DIR)
                || name_text.eq_ignore_ascii_case(LISTS_DIR)
                || name_text.eq_ignore_ascii_case(UTILS_DIR)
            {
                copy_tree(&from, &to)?;
            }
            continue;
        }

        if from.is_file()
            && from
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bat"))
        {
            fs::copy(&from, &to).map_err(|source| io_error("copy", &from, source))?;
        }
    }

    Ok(())
}

/// Copies `source` into `destination` recursively, skipping links and unreadable entries.
fn copy_tree(source: &Path, destination: &Path) -> Result<(), PackError> {
    fs::create_dir_all(destination).map_err(|source| io_error("create", destination, source))?;

    for entry in read_dir(source)? {
        let from = entry.path();
        let to = destination.join(entry.file_name());
        let file_type = entry
            .file_type()
            .map_err(|source| io_error("inspect", &from, source))?;

        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            copy_tree(&from, &to)?;
        } else if file_type.is_file() {
            fs::copy(&from, &to).map_err(|source| io_error("copy", &from, source))?;
        }
    }

    Ok(())
}

/// Reads a directory, sorted by file name so an import is reproducible.
fn read_dir(path: &Path) -> Result<Vec<fs::DirEntry>, PackError> {
    let mut entries = Vec::new();

    for entry in fs::read_dir(path).map_err(|source| io_error("read", path, source))? {
        entries.push(entry.map_err(|source| io_error("read", path, source))?);
    }

    entries.sort_by_key(|entry| entry.file_name().to_string_lossy().to_ascii_lowercase());
    Ok(entries)
}

/// A wrapping sum of the size and modification time of every file a strategy can reach.
///
/// `status.json` is either skipped (top level) or never visited (only `bin`, `lists`, `utils` and
/// `*.bat` are walked), so writing the status does not change the marker.
///
/// # Errors
///
/// [`PackError::Io`] when a directory under the pack cannot be read.
pub fn version_marker(root: &Path) -> Result<String, PackError> {
    let mut files = Vec::new();

    for directory in [BIN_DIR, LISTS_DIR, UTILS_DIR] {
        collect_files(&root.join(directory), &mut files)?;
    }

    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let is_bat = path
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("bat"));
            if is_bat && path.is_file() {
                files.push(path);
            }
        }
    }

    files.sort();

    let mut sum: u64 = 0;
    for path in files {
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let modified = metadata.modified().map_or(0, |time| {
            time.duration_since(UNIX_EPOCH)
                .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX))
                .unwrap_or(0)
        });
        sum = sum.wrapping_add(metadata.len()).wrapping_add(modified);
    }

    Ok(format!("{sum:016x}"))
}

/// Collects every regular file under `root`; a missing directory is not an error.
fn collect_files(root: &Path, into: &mut Vec<PathBuf>) -> Result<(), PackError> {
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error("read", root, source)),
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };

        if file_type.is_symlink() {
            continue;
        }

        if file_type.is_dir() {
            collect_files(&path, into)?;
        } else if file_type.is_file() {
            into.push(path);
        }
    }

    Ok(())
}

fn io_error(action: &'static str, path: &Path, source: io::Error) -> PackError {
    PackError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// Writes a minimal pack into `root` and returns it.
    fn fake_pack(root: &Path) -> PathBuf {
        let pack = root.join("pack");
        fs::create_dir_all(pack.join(BIN_DIR)).expect("bin");
        fs::create_dir_all(pack.join(LISTS_DIR)).expect("lists");
        fs::create_dir_all(pack.join(UTILS_DIR)).expect("utils");
        fs::write(pack.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        fs::write(pack.join(BIN_DIR).join(WINDIVERT_SYS), b"sys").expect("driver");
        fs::write(
            pack.join(LISTS_DIR).join("list-general.txt"),
            b"example.com\n",
        )
        .expect("list");
        fs::write(pack.join(UTILS_DIR).join("game_filter.enabled"), b"all\n").expect("flag");
        fs::write(pack.join("general.bat"), b"@echo off\r\n").expect("bat");
        fs::write(pack.join("notes.txt"), b"not part of a pack").expect("txt");
        pack
    }

    #[test]
    fn a_pack_with_both_binaries_opens_and_marks_its_version() {
        let directory = tempdir().expect("temp dir");
        let pack = Pack::open(fake_pack(directory.path())).expect("valid pack");

        assert!(pack.winws().ends_with(WINWS_EXE));
        assert!(pack.windivert().ends_with(WINDIVERT_SYS));
        assert_eq!(pack.version.len(), 16);
        assert!(pack.version.chars().all(|digit| digit.is_ascii_hexdigit()));
    }

    #[test]
    fn the_marker_changes_when_a_file_changes() {
        let directory = tempdir().expect("temp dir");
        let root = fake_pack(directory.path());
        let first = Pack::open(&root).expect("pack").version;

        fs::write(root.join(BIN_DIR).join(WINWS_EXE), b"another exe").expect("touch");

        let second = Pack::open(&root).expect("pack").version;
        assert_ne!(first, second, "a replaced binary must change the marker");
    }

    #[test]
    fn the_marker_ignores_the_status_file() {
        let directory = tempdir().expect("temp dir");
        let root = fake_pack(directory.path());
        let before = Pack::open(&root).expect("pack").version;

        fs::write(root.join("status.json"), br#"{"running":true}"#).expect("status");

        assert_eq!(Pack::open(&root).expect("pack").version, before);
    }

    #[test]
    fn a_folder_without_winws_is_refused() {
        let directory = tempdir().expect("temp dir");
        let root = fake_pack(directory.path());
        fs::remove_file(root.join(BIN_DIR).join(WINWS_EXE)).expect("remove");

        match Pack::open(&root) {
            Err(PackError::MissingFile { path }) => assert!(path.ends_with(WINWS_EXE)),
            other => panic!("expected a missing-file error, got {other:?}"),
        }
    }

    #[test]
    fn a_folder_without_the_driver_is_refused() {
        let directory = tempdir().expect("temp dir");
        let root = fake_pack(directory.path());
        fs::remove_file(root.join(BIN_DIR).join(WINDIVERT_SYS)).expect("remove");

        match Pack::open(&root) {
            Err(PackError::MissingFile { path }) => assert!(path.ends_with(WINDIVERT_SYS)),
            other => panic!("expected a missing-file error, got {other:?}"),
        }
    }

    #[test]
    fn a_file_is_not_a_pack() {
        let directory = tempdir().expect("temp dir");
        let file = directory.path().join("pack.zip");
        fs::write(&file, b"zip").expect("write");

        assert!(matches!(
            Pack::open(&file),
            Err(PackError::NotADirectory { .. })
        ));
    }

    #[test]
    fn import_copies_the_usable_parts_and_skips_unrelated_files() {
        let directory = tempdir().expect("temp dir");
        let source = fake_pack(directory.path());
        let target = directory.path().join("data").join("zapret");

        let pack = import(&source, &target).expect("import");

        assert_eq!(pack.root, target);
        assert!(target.join(BIN_DIR).join(WINWS_EXE).is_file());
        assert!(target.join(BIN_DIR).join(WINDIVERT_SYS).is_file());
        assert!(target.join(LISTS_DIR).join("list-general.txt").is_file());
        assert!(target.join(UTILS_DIR).join("game_filter.enabled").is_file());
        assert!(target.join("general.bat").is_file());
        assert!(
            !target.join("notes.txt").exists(),
            "only the pack's own files are copied"
        );
    }

    #[test]
    fn import_refuses_a_source_that_is_not_a_pack() {
        let directory = tempdir().expect("temp dir");
        let source = directory.path().join("empty");
        fs::create_dir_all(&source).expect("source");
        let target = directory.path().join("zapret");

        assert!(matches!(
            import(&source, &target),
            Err(PackError::MissingFile { .. })
        ));
        assert!(!target.exists(), "a refused import leaves nothing behind");
    }

    #[test]
    fn import_refuses_the_target_as_its_own_source() {
        let directory = tempdir().expect("temp dir");
        let source = fake_pack(directory.path());
        let target = directory.path().join("zapret");
        import(&source, &target).expect("first import");

        match import(&target, &target) {
            Err(PackError::SameFolder { path }) => assert_eq!(path, target),
            other => panic!("expected SameFolder, got {other:?}"),
        }
    }

    #[test]
    fn import_refuses_nested_folders() {
        let directory = tempdir().expect("temp dir");
        let source = fake_pack(directory.path());
        let inner = source.join("inner");

        assert!(matches!(
            import(&source, &inner),
            Err(PackError::Overlapping { .. })
        ));
    }

    #[test]
    fn importing_over_an_existing_pack_replaces_it() {
        let directory = tempdir().expect("temp dir");
        let source = fake_pack(directory.path());
        let target = directory.path().join("zapret");
        import(&source, &target).expect("first import");
        fs::write(target.join("stale.txt"), b"stale").expect("stale");

        import(&source, &target).expect("second import");

        assert!(
            !target.join("stale.txt").exists(),
            "the old folder is replaced, not merged"
        );
        assert!(target.join(BIN_DIR).join(WINWS_EXE).is_file());
    }

    #[test]
    fn the_marker_covers_the_lists_and_the_strategy_scripts() {
        let directory = tempdir().expect("temp dir");
        let root = fake_pack(directory.path());
        let before = version_marker(&root).expect("marker");

        fs::write(
            root.join(LISTS_DIR).join("list-google.txt"),
            b"google.com\n",
        )
        .expect("list");
        let after_list = version_marker(&root).expect("marker");
        assert_ne!(before, after_list);

        fs::write(root.join("general (ALT).bat"), b"@echo off\r\n").expect("bat");
        assert_ne!(after_list, version_marker(&root).expect("marker"));
    }
}
