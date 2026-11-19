//! Staging and packaging: the portable tree, its archive, and the installer script.
//!
//! The portable layout is data, [`COPIED_FILES`] plus [`generated_files`], so the list can be asserted
//! in a unit test instead of being inferred from a directory listing after a build.

use std::fs::{self, File};
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipWriter};

use crate::commands::CliError;
use crate::commands::release::semver::Version;

/// Files copied into the portable tree as-is: archive name first, repository-relative source second.
pub const COPIED_FILES: [(&str, &str); 14] = [
    ("ProjectNeb.exe", "rust/target/release/ProjectNeb.exe"),
    ("neb-shell.exe", "rust/target/release/neb-shell.exe"),
    ("neb-wallpaper.exe", "rust/target/release/neb-wallpaper.exe"),
    ("neb-updater.exe", "rust/target/release/neb-updater.exe"),
    ("neb-app.exe", "rust/target/release/neb-app.exe"),
    ("WebView2Loader.dll", "rust/target/release/WebView2Loader.dll"),
    ("neb-zapret.exe", "rust/target/release/neb-zapret.exe"),
    ("neb-windhawk.exe", "rust/target/release/neb-windhawk.exe"),
    ("neb-discord.exe", "rust/target/release/neb-discord.exe"),
    ("neb-wm.exe", "rust/target/release/neb-wm.exe"),
    ("neb-cli.exe", "rust/target/release/neb-cli.exe"),
    ("neb.ico", "assets/neb.ico"),
    ("logo-mark.png", "assets/logo-mark.png"),
    ("README.md", "docs/beta/README-BETA.md"),
];

/// Files the pipeline writes into the portable tree instead of copying.
pub fn generated_files(version: &Version) -> Vec<(&'static str, String)> {
    vec![
        ("start-portable.cmd", portable_launcher()),
        ("README-PORTABLE.txt", portable_readme(version)),
    ]
}

/// Every archive entry, in the order a user sees them inside the zip.
///
/// The pipeline builds the same list while staging; this view exists for the ordering test.
#[cfg(test)]
pub fn staged_names(version: &Version) -> Vec<String> {
    let mut names: Vec<String> = COPIED_FILES
        .iter()
        .map(|(name, _)| (*name).to_owned())
        .collect();
    names.extend(
        generated_files(version)
            .into_iter()
            .map(|(name, _)| name.to_owned()),
    );
    names
}

/// Walks the built dashboard (`src/Neb.UI/dist`) into `(relative path, file)` pairs, sorted, so the
/// portable archive carries the UI the main window loads offline.
pub fn ui_entries(root: &Path) -> io::Result<Vec<(String, PathBuf)>> {
    fn walk(base: &Path, dir: &Path, files: &mut Vec<(String, PathBuf)>) -> io::Result<()> {
        let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);

        for entry in entries {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, files)?;
            } else if path.is_file() {
                let relative = path
                    .strip_prefix(base)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .replace('\\', "/");
                files.push((relative, path));
            }
        }

        Ok(())
    }

    let mut files = Vec::new();
    if root.is_dir() {
        walk(root, root, &mut files)?;
    }
    Ok(files)
}

/// The archive file name for a release.
pub fn portable_filename(version: &Version, channel: &str) -> String {
    format!("ProjectNeb-{version}-{channel}-portable.zip")
}

/// The installer's `OutputBaseFilename` value: no directory, no extension.
pub fn setup_base(version: &Version, channel: &str) -> String {
    format!("ProjectNeb-{version}-{channel}-setup")
}

/// The installer file name for a release.
pub fn setup_filename(version: &Version, channel: &str) -> String {
    format!("{}.exe", setup_base(version, channel))
}

/// Renders the portable launcher, which only needs cmd.exe.
fn portable_launcher() -> String {
    concat!(
        "@echo off\r\n",
        "setlocal\r\n",
        "cd /d \"%~dp0\"\r\n",
        "rem Starts ProjectNeb; it brings up the main window, panel, dock and live wallpaper.\r\n",
        "start \"\" \"ProjectNeb.exe\"\r\n",
        "endlocal\r\n",
    )
    .to_owned()
}

/// Renders the plain-text readme that sits next to the launcher.
fn portable_readme(version: &Version) -> String {
    format!(
        "ProjectNeb {version} Beta — портативная сборка\r\n\
         \r\n\
         1. Запусти start-portable.cmd (или ProjectNeb.exe).\r\n\
         2. Супервизор сам поднимет панель, док и живые обои.\r\n\
         3. Настройки и логи: %LocalAppData%\\ProjectNeb.\r\n\
         \r\n\
         Версия ничего не пишет в системные папки и не требует прав администратора.\r\n\
         Подробности — в README.md.\r\n"
    )
}

/// Rewrites the Inno Setup template for one repository and version.
///
/// Three directives point at the machine the release is built on, so they cannot stay as committed:
/// `AppVersion`, `OutputBaseFilename`, and `Root`.
///
/// # Errors
///
/// [`CliError::Failed`] when the template no longer contains one of those directives, which means the
/// installer would silently package the wrong version or the wrong tree.
pub fn inno_script(
    template: &str,
    version: &Version,
    channel: &str,
    root: &Path,
) -> Result<String, CliError> {
    let mut saw_version = false;
    let mut saw_output = false;
    let mut saw_root = false;
    let mut script = String::with_capacity(template.len() + 64);

    for line in template.lines() {
        let trimmed = line.trim_start();

        let replacement = if trimmed.starts_with("#define AppVersion") {
            saw_version = true;
            Some(format!("#define AppVersion \"{version}\""))
        } else if trimmed.starts_with("#define Root") {
            saw_root = true;
            Some(format!("#define Root \"{}\"", root.display()))
        } else if trimmed.starts_with("OutputBaseFilename=") {
            saw_output = true;
            Some(format!(
                "OutputBaseFilename={}",
                setup_base(version, channel)
            ))
        } else {
            None
        };

        match replacement {
            Some(line) => script.push_str(&line),
            None => script.push_str(line),
        }
        script.push('\n');
    }

    if !(saw_version && saw_output && saw_root) {
        return Err(CliError::Failed(
            "installer/ProjectNeb.iss must define AppVersion, OutputBaseFilename and Root"
                .to_owned(),
        ));
    }

    Ok(script)
}

/// Zips `entries` into `archive`, one entry per `(name, source)` pair.
///
/// # Errors
///
/// [`CliError::Failed`] with the file that could not be read or written.
pub fn create_zip(archive: &Path, entries: &[(String, PathBuf)]) -> Result<(), CliError> {
    let file = File::create(archive).map_err(|error| io_error("create", archive, &error))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);

    for (name, source) in entries {
        writer.start_file(name.as_str(), options).map_err(|error| {
            CliError::Failed(format!("cannot add '{name}' to the zip: {error}"))
        })?;

        let mut input = File::open(source).map_err(|error| io_error("open", source, &error))?;
        io::copy(&mut input, &mut writer).map_err(|error| {
            CliError::Failed(format!("cannot write '{name}' to the zip: {error}"))
        })?;
    }

    writer.finish().map_err(|error| {
        CliError::Failed(format!("cannot finish '{}': {error}", archive.display()))
    })?;
    Ok(())
}

/// Upper-case SHA-256 of a file's bytes, the form the manifest already uses.
///
/// # Errors
///
/// [`CliError::Failed`] when the file cannot be opened or read.
pub fn sha256(path: &Path) -> Result<String, CliError> {
    let mut file = File::open(path).map_err(|error| io_error("open", path, &error))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|error| io_error("read", path, &error))?;

        if read == 0 {
            break;
        }

        hasher.update(&buffer[..read]);
    }

    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect())
}

/// Copies a file, creating the destination's parent first.
///
/// # Errors
///
/// [`CliError::Failed`] with both paths when the copy fails.
pub fn copy_file(source: &Path, destination: &Path) -> Result<(), CliError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent).map_err(|error| io_error("create", parent, &error))?;
    }

    fs::copy(source, destination).map_err(|error| {
        CliError::Failed(format!(
            "cannot copy '{}' to '{}': {error}",
            source.display(),
            destination.display()
        ))
    })?;
    Ok(())
}

/// Phrases an I/O error around the path it happened on.
fn io_error(action: &str, path: &Path, error: &io::Error) -> CliError {
    CliError::Failed(format!("cannot {action} '{}': {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("version")
    }

    #[test]
    fn the_portable_tree_has_the_documented_entries() {
        let names = staged_names(&version("0.1.1"));

        assert_eq!(
            names,
            vec![
                "ProjectNeb.exe".to_owned(),
                "neb-shell.exe".to_owned(),
                "neb-wallpaper.exe".to_owned(),
                "neb-updater.exe".to_owned(),
                "neb-app.exe".to_owned(),
                "WebView2Loader.dll".to_owned(),
                "neb-zapret.exe".to_owned(),
                "neb-windhawk.exe".to_owned(),
                "neb-discord.exe".to_owned(),
                "neb-wm.exe".to_owned(),
                "neb-cli.exe".to_owned(),
                "neb.ico".to_owned(),
                "logo-mark.png".to_owned(),
                "README.md".to_owned(),
                "start-portable.cmd".to_owned(),
                "README-PORTABLE.txt".to_owned(),
            ]
        );
    }

    #[test]
    fn copied_sources_point_at_build_outputs_and_assets() {
        for (name, source) in COPIED_FILES {
            if name.ends_with(".exe") {
                assert_eq!(source, format!("rust/target/release/{name}"));
            }

            assert!(
                !source.starts_with('/') && !source.contains(".."),
                "'{source}' must stay inside the repository"
            );
        }

        assert!(COPIED_FILES.contains(&("README.md", "docs/beta/README-BETA.md")));
    }

    #[test]
    fn the_generated_files_describe_the_release() {
        let generated = generated_files(&version("0.1.1"));
        let launcher = &generated[0];
        let readme = &generated[1];

        assert_eq!(launcher.0, "start-portable.cmd");
        assert!(launcher.1.contains("ProjectNeb.exe"));
        assert!(launcher.1.contains("\r\n"), "cmd files want CRLF");
        assert_eq!(readme.0, "README-PORTABLE.txt");
        assert!(readme.1.contains("0.1.1"));
        assert!(readme.1.contains("start-portable.cmd"));
    }

    #[test]
    fn names_carry_the_channel() {
        assert_eq!(
            portable_filename(&version("0.1.1"), "beta"),
            "ProjectNeb-0.1.1-beta-portable.zip"
        );
        assert_eq!(
            setup_filename(&version("0.1.1"), "beta"),
            "ProjectNeb-0.1.1-beta-setup.exe"
        );
    }

    #[test]
    fn the_installer_script_points_at_this_repo_and_version() {
        let template = concat!(
            "#define AppName \"ProjectNeb\"\n",
            "#define AppVersion \"0.1.0\"\n",
            "#define Root \"A:\\ProjectNeb\"\n",
            "\n",
            "[Setup]\n",
            "OutputBaseFilename=ProjectNeb-{#AppVersion}-beta-setup\n",
            "OutputDir={#Root}\\dist\n",
        );
        let script = inno_script(
            template,
            &version("0.1.1"),
            "beta",
            Path::new(r"A:\ProjectNeb-wt\release"),
        )
        .expect("script");

        assert!(script.contains("#define AppVersion \"0.1.1\""));
        assert!(script.contains(r#"#define Root "A:\ProjectNeb-wt\release""#));
        assert!(script.contains("OutputBaseFilename=ProjectNeb-0.1.1-beta-setup"));
        assert!(script.contains("OutputDir={#Root}\\dist"));
    }

    #[test]
    fn a_template_without_the_directives_is_refused() {
        assert!(
            inno_script(
                "AppName=ProjectNeb\n",
                &version("0.1.1"),
                "beta",
                Path::new("C:\\")
            )
            .is_err()
        );
    }

    #[test]
    fn sha256_matches_the_known_digest_of_abc() {
        let dir = tempfile::tempdir().expect("temp dir");
        let path = dir.path().join("abc.txt");
        fs::write(&path, b"abc").expect("write");

        assert_eq!(
            sha256(&path).expect("hash"),
            "BA7816BF8F01CFEA414140DE5DAE2223B00361A396177A9CB410FF61F20015AD"
        );
    }

    #[test]
    fn a_zip_round_trips_every_entry() {
        let dir = tempfile::tempdir().expect("temp dir");
        let first = dir.path().join("one.txt");
        let second = dir.path().join("two.txt");
        fs::write(&first, b"one").expect("write");
        fs::write(&second, b"two").expect("write");

        let archive = dir.path().join("bundle.zip");
        create_zip(
            &archive,
            &[
                ("one.txt".to_owned(), first),
                ("two.txt".to_owned(), second),
            ],
        )
        .expect("zip");

        let mut zip = zip::ZipArchive::new(File::open(&archive).expect("open")).expect("read zip");
        assert_eq!(zip.len(), 2);

        let mut text = String::new();
        zip.by_name("one.txt")
            .expect("one")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text, "one");

        text.clear();
        zip.by_name("two.txt")
            .expect("two")
            .read_to_string(&mut text)
            .expect("read");
        assert_eq!(text, "two");
    }
}
