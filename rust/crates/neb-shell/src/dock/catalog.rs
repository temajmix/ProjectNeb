//! Turning `shell.pinnedApps` into dock entries.
//!
//! The list is hand-editable, so this is where a quoted path, a `%WINDIR%` variable, the same app
//! spelled twice and an app that has been uninstalled are dealt with — once, before anything is
//! drawn. The rules mirror the C# `AppCatalog`, because the two builds share `config.json`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use neb_core::config::Config;

/// One application the user pinned to the dock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppEntry {
    /// What the tile's tooltip calls it: the file name without its extension.
    pub name: String,
    /// The absolute path that is launched when the app is not running.
    pub path: PathBuf,
}

/// Entries for the pinned list of `config`, in the order it was written.
pub fn load(config: &Config) -> Vec<AppEntry> {
    load_paths(&config.shell.pinned_apps)
}

/// Entries for `pinned`: normalised, de-duplicated case-insensitively, and only for files that exist.
///
/// A duplicate keeps its first spelling, because that is the one the user typed. A path that no
/// longer exists is logged and skipped rather than drawn as a tile that cannot be launched.
pub fn load_paths<I, S>(pinned: I) -> Vec<AppEntry>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut entries = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();

    for raw in pinned {
        let Some(path) = normalize(raw.as_ref()) else {
            continue;
        };

        let key = path.to_string_lossy().to_lowercase();
        if !seen.insert(key) {
            continue;
        }

        if !path.is_file() {
            crate::log_warn!(
                "dock",
                "pinned app '{}' does not exist, skipped",
                path.display()
            );
            continue;
        }

        entries.push(AppEntry {
            name: name_for(&path),
            path,
        });
    }

    entries
}

/// The absolute path a pinned entry points at, or `None` when it cannot be one.
///
/// Quotes pasted from a command line are dropped and `%VARIABLES%` are expanded; blanks, relative
/// paths and paths Windows cannot parse are rejected. Existence is checked by [`load_paths`], not
/// here, so the same normal form compares two paths.
pub fn normalize(raw: &str) -> Option<PathBuf> {
    let mut candidate = raw.trim();

    if candidate.len() >= 2 && candidate.starts_with('"') && candidate.ends_with('"') {
        candidate = candidate[1..candidate.len() - 1].trim();
    }

    if candidate.is_empty() || candidate.contains('\0') {
        return None;
    }

    let expanded = expand_variables(candidate);
    let path = PathBuf::from(expanded.trim());

    if path.is_absolute() && path.file_name().is_some() {
        Some(path)
    } else {
        None
    }
}

/// The display name of an entry: the file name without `.lnk` or `.exe`.
pub fn name_for(path: &Path) -> String {
    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) if !stem.trim().is_empty() => stem.to_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

/// Expands `%NAME%` against this process's environment, leaving unknown names as written.
pub fn expand_variables(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(start) = rest.find('%') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];

        match after.find('%') {
            Some(end) => {
                let name = &after[..end];
                match std::env::var(name) {
                    Ok(value) if !name.is_empty() => out.push_str(&value),
                    _ => {
                        out.push('%');
                        out.push_str(name);
                        out.push('%');
                    }
                }
                rest = &after[end + 1..];
            }
            None => {
                out.push('%');
                rest = after;
            }
        }
    }

    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("neb-dock-catalog-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("create the temp dir");
        dir
    }

    #[test]
    fn a_quoted_absolute_path_is_normalised() {
        let path = normalize(r#"  "C:\Windows\System32\notepad.exe"  "#);

        assert_eq!(
            path,
            Some(PathBuf::from(r"C:\Windows\System32\notepad.exe"))
        );
    }

    #[test]
    fn blanks_relative_paths_and_nonsense_are_refused() {
        for raw in [
            "",
            "   ",
            "notepad.exe",
            r"System32\notepad.exe",
            "C:\\a\\\0b",
        ] {
            assert_eq!(normalize(raw), None, "'{raw}' must not be an entry");
        }
    }

    #[test]
    fn environment_variables_are_expanded() {
        let expanded = expand_variables(r"%WINDIR%\System32\notepad.exe");

        assert!(expanded.ends_with(r"System32\notepad.exe"), "{expanded}");
        assert!(!expanded.contains('%'), "{expanded}");
        assert_eq!(
            expand_variables("%NEB_NOT_A_REAL_VARIABLE%\\x.exe"),
            "%NEB_NOT_A_REAL_VARIABLE%\\x.exe",
            "an unknown name is left alone"
        );
        assert_eq!(expand_variables("100%"), "100%");
    }

    #[test]
    fn the_name_is_the_file_stem() {
        assert_eq!(
            name_for(Path::new(r"C:\Windows\System32\mspaint.exe")),
            "mspaint"
        );
        assert_eq!(name_for(Path::new(r"C:\Start\7-Zip.lnk")), "7-Zip");
    }

    #[test]
    fn duplicates_keep_their_first_spelling_and_missing_files_are_skipped() {
        let dir = temp_dir("dedupe");
        let app = dir.join("Tool.exe");
        fs::write(&app, b"x").expect("write the fake exe");
        let missing = dir.join("Gone.exe");

        let entries = load_paths([
            app.to_string_lossy().into_owned(),
            app.to_string_lossy().to_uppercase(),
            missing.to_string_lossy().into_owned(),
            String::new(),
        ]);

        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].name, "Tool");
        assert_eq!(entries[0].path, app);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_c_sharp_config_with_pinned_apps_is_read_by_the_loader() {
        let dir = temp_dir("config");
        let app = dir.join("Pinned.exe");
        fs::write(&app, b"x").expect("write the fake exe");

        let config = Config {
            shell: neb_core::config::ShellSettings {
                pinned_apps: vec![app.to_string_lossy().into_owned()],
                ..neb_core::config::ShellSettings::default()
            },
            ..Config::default()
        };

        let entries = load(&config);

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, app);

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_empty_pinned_list_is_empty() {
        let config = Config::default();

        assert!(load(&config).is_empty());
    }
}
