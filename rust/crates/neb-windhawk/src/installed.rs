//! What is installed: source files, compiled DLLs and the enabled state.
//!
//! Windhawk keeps sources in `<data>\ModsSource\<id>.wh.cpp` and compiles them to
//! `<data>\Engine\Mods\{32,64,arm64}\<id>_<version>_<build>.dll`. The two are read together because
//! either can outlive the other: a source with no DLL is installed but not compiled yet, and a DLL
//! with no source is a mod removed through Windhawk's own UI or an interrupted uninstall.
//!
//! Enabled state is not in a file. Windhawk 1.7.3 reads it from
//! `HKLM\SOFTWARE\Windhawk\Engine\Mods\<id>\Disabled`; [`crate::registry`] owns that read.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::catalog::prettify;
use crate::paths;
use crate::plan::sanitize_id;
use crate::registry::ModRegistry;

/// Architecture folders Windhawk compiles into.
pub const ARCH_FOLDERS: [&str; 3] = ["32", "64", "arm64"];

/// DLLs that belong to the engine, not to a mod.
const ENGINE_DLLS: [&str; 3] = ["libc++", "libunwind", "windhawk-mod-shim"];

/// One installed mod.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledMod {
    /// Mod id.
    pub id: String,
    /// Human-readable name derived from the id.
    pub name: String,
    /// Source file, when one exists.
    pub source: Option<String>,
    /// `Some(true)`/`Some(false)` from the registry, `None` when there is no configuration key.
    pub enabled: Option<bool>,
    /// Compiled DLL file names, one per architecture.
    pub compiled: Vec<String>,
}

/// Everything [`list`] found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledReport {
    /// Windhawk data directory the report describes.
    pub data_dir: String,
    /// `<data>\ModsSource`.
    pub mods_source: String,
    /// `<data>\Engine\Mods`.
    pub engine_mods: String,
    /// Mods, sorted by id.
    pub mods: Vec<InstalledMod>,
    /// Registry reads that failed, so a caller can tell "absent" from "unreadable".
    pub issues: Vec<String>,
}

impl InstalledReport {
    /// The number of installed mods.
    pub fn len(&self) -> usize {
        self.mods.len()
    }

    /// Whether nothing is installed.
    pub fn is_empty(&self) -> bool {
        self.mods.is_empty()
    }

    /// Ids, for the catalog's `installed` flag.
    pub fn ids(&self) -> Vec<String> {
        self.mods.iter().map(|entry| entry.id.clone()).collect()
    }
}

/// Lists sources, compiled DLLs and configuration keys under `data_dir`.
pub fn list(data_dir: &Path, registry: &dyn ModRegistry) -> InstalledReport {
    let source_dir = paths::mods_source(data_dir);
    let engine_dir = paths::engine_mods(data_dir);

    let mut ids = source_ids(&source_dir);
    for id in compiled_ids(&engine_dir) {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }

    let mut issues = Vec::new();
    match registry.configured_ids() {
        Ok(configured) => {
            for id in configured {
                if is_canonical(&id) && !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Err(error) => issues.push(format!("cannot enumerate mod configuration: {error}")),
    }

    ids.sort();
    ids.dedup();

    let mods = ids
        .into_iter()
        .map(|id| {
            let source = source_dir.join(format!("{id}.wh.cpp"));
            let enabled = match registry.read_disabled(&id) {
                Ok(Some(disabled)) => Some(!disabled),
                Ok(None) => None,
                Err(error) => {
                    issues.push(format!("cannot read the enabled state of '{id}': {error}"));
                    None
                }
            };

            InstalledMod {
                name: prettify(&id),
                id: id.clone(),
                source: source.is_file().then(|| source.display().to_string()),
                enabled,
                compiled: compiled_files(&engine_dir, &id),
            }
        })
        .collect();

    InstalledReport {
        data_dir: data_dir.display().to_string(),
        mods_source: source_dir.display().to_string(),
        engine_mods: engine_dir.display().to_string(),
        mods,
        issues,
    }
}

/// Ids of the `.wh.cpp` files in `source_dir`.
pub fn source_ids(source_dir: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(source_dir) else {
        return Vec::new();
    };

    let mut ids = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter_map(|name| name.strip_suffix(".wh.cpp").map(str::to_owned))
        .filter(|id| is_canonical(id))
        .collect::<Vec<_>>();

    ids.sort();
    ids.dedup();
    ids
}

/// Mod ids named by compiled DLLs under every architecture folder.
pub fn compiled_ids(engine_dir: &Path) -> Vec<String> {
    let mut ids = Vec::new();

    for arch in ARCH_FOLDERS {
        let Ok(entries) = fs::read_dir(engine_dir.join(arch)) else {
            continue;
        };

        for entry in entries.filter_map(|entry| entry.ok()) {
            let Some(name) = entry.file_name().into_string().ok() else {
                continue;
            };
            let Some(stem) = name.strip_suffix(".dll") else {
                continue;
            };
            let Some((id, _rest)) = stem.split_once('_') else {
                continue;
            };
            if ENGINE_DLLS.contains(&id) || !is_canonical(id) {
                continue;
            }
            ids.push(id.to_owned());
        }
    }

    ids.sort();
    ids.dedup();
    ids
}

/// The compiled DLL file names of `id`, across every architecture.
pub fn compiled_files(engine_dir: &Path, id: &str) -> Vec<String> {
    let prefix = format!("{id}_");
    let mut files = Vec::new();

    for arch in ARCH_FOLDERS {
        let Ok(entries) = fs::read_dir(engine_dir.join(arch)) else {
            continue;
        };

        for entry in entries.filter_map(|entry| entry.ok()) {
            if !entry.path().is_file() {
                continue;
            }
            let Some(name) = entry.file_name().into_string().ok() else {
                continue;
            };
            if name.starts_with(&prefix) && name.ends_with(".dll") {
                files.push(name);
            }
        }
    }

    files.sort();
    files.dedup();
    files
}

/// Deletes the compiled DLLs of `id` and an `Engine\Mods\<id>` directory when a portable build
/// used one. A missing file is success.
///
/// # Errors
///
/// The first file that exists but could not be removed, as a sentence.
pub fn remove_compiled(data_dir: &Path, id: &str) -> Result<usize, String> {
    let id = sanitize_id(id).map_err(|error| error.to_string())?;
    let engine_dir = paths::engine_mods(data_dir);
    let prefix = format!("{id}_");
    let mut removed = 0usize;

    for arch in ARCH_FOLDERS {
        let folder = engine_dir.join(arch);
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };

        for entry in entries.filter_map(|entry| entry.ok()) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !(name.starts_with(&prefix) && name.ends_with(".dll")) {
                continue;
            }
            if entry.path().is_file() {
                fs::remove_file(entry.path()).map_err(|error| {
                    format!("cannot delete '{}': {error}", entry.path().display())
                })?;
                removed += 1;
            }
        }
    }

    let folder = engine_dir.join(&id);
    if folder.is_dir() {
        fs::remove_dir_all(&folder)
            .map_err(|error| format!("cannot delete '{}': {error}", folder.display()))?;
        removed += 1;
    }

    Ok(removed)
}

/// Whether `id` is already the exact form [`sanitize_id`] would produce.
///
/// A compiled DLL or a source file whose stem is `Bad` is not Windhawk's own naming, which is
/// lowercase throughout; treating it as a mod would invent an id the registry cannot address.
fn is_canonical(id: &str) -> bool {
    sanitize_id(id).is_ok_and(|canonical| canonical == id)
}

/// The source file of `id` under `data_dir`.
pub fn source_path(data_dir: &Path, id: &str) -> Result<PathBuf, String> {
    crate::plan::target_path(data_dir, id).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::MemoryRegistry;
    use tempfile::tempdir;

    fn data_dir(root: &Path) -> PathBuf {
        let data = root.join("Windhawk");
        fs::create_dir_all(data.join("ModsSource")).expect("sources");
        fs::create_dir_all(data.join("Engine").join("Mods").join("64")).expect("compiled");
        data
    }

    #[test]
    fn a_source_and_its_compiled_dll_are_one_mod() {
        let root = tempdir().expect("temp dir");
        let data = data_dir(root.path());
        fs::write(
            data.join("ModsSource").join("accent-color-sync.wh.cpp"),
            b"// mod",
        )
        .expect("source");
        fs::write(
            data.join("Engine")
                .join("Mods")
                .join("64")
                .join("accent-color-sync_1.0_123.dll"),
            b"dll",
        )
        .expect("dll");

        let report = list(
            &data,
            &MemoryRegistry::with(&[("accent-color-sync", false)]),
        );

        assert_eq!(report.len(), 1);
        assert_eq!(report.mods[0].id, "accent-color-sync");
        assert_eq!(report.mods[0].name, "Accent Color Sync");
        assert_eq!(report.mods[0].enabled, Some(true));
        assert_eq!(report.mods[0].compiled.len(), 1);
        assert!(report.mods[0].source.is_some());
    }

    #[test]
    fn a_compiled_mod_without_a_source_is_still_listed() {
        let root = tempdir().expect("temp dir");
        let data = data_dir(root.path());
        fs::write(
            data.join("Engine")
                .join("Mods")
                .join("64")
                .join("windows-11-taskbar-styler_1.9_252378.dll"),
            b"dll",
        )
        .expect("dll");

        let report = list(
            &data,
            &MemoryRegistry::with(&[("windows-11-taskbar-styler", true)]),
        );

        assert_eq!(report.len(), 1);
        assert_eq!(report.mods[0].enabled, Some(false));
        assert!(report.mods[0].source.is_none());
    }

    #[test]
    fn a_configured_mod_without_files_is_reported_without_an_enabled_guess() {
        let root = tempdir().expect("temp dir");
        let data = data_dir(root.path());

        let report = list(&data, &MemoryRegistry::with(&[("ghost", false)]));

        assert_eq!(report.len(), 1);
        assert_eq!(report.mods[0].id, "ghost");
        assert_eq!(report.mods[0].enabled, Some(true));
        assert!(report.mods[0].compiled.is_empty());
        assert!(report.mods[0].source.is_none());
    }

    #[derive(Debug)]
    struct BrokenRegistry;

    impl ModRegistry for BrokenRegistry {
        fn read_disabled(&self, _id: &str) -> Result<Option<bool>, String> {
            Err("access denied".to_owned())
        }

        fn set_disabled(&self, _id: &str, _disabled: bool) -> Result<(), String> {
            Err("access denied".to_owned())
        }

        fn remove(&self, _id: &str) -> Result<(), String> {
            Err("access denied".to_owned())
        }

        fn configured_ids(&self) -> Result<Vec<String>, String> {
            Err("access denied".to_owned())
        }
    }

    #[test]
    fn a_registry_failure_is_an_issue_and_never_a_false_enabled() {
        let root = tempdir().expect("temp dir");
        let data = data_dir(root.path());
        fs::write(
            data.join("ModsSource").join("accent-color-sync.wh.cpp"),
            b"// mod",
        )
        .expect("source");

        let report = list(&data, &BrokenRegistry);

        assert_eq!(report.mods[0].enabled, None);
        assert_eq!(report.issues.len(), 2, "{report:?}");
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.contains("enumerate"))
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.contains("enabled state"))
        );
    }

    #[test]
    fn engine_dlls_and_malformed_names_are_not_mods() {
        let root = tempdir().expect("temp dir");
        let data = data_dir(root.path());
        let folder = data.join("Engine").join("Mods").join("64");

        for name in [
            "libc++.dll",
            "libunwind.dll",
            "windhawk-mod-shim.dll",
            "nodllname.dll",
            "Bad_Name_1.0_2.dll",
            "uxtheme-hook_1.4_557662.dll",
        ] {
            fs::write(folder.join(name), b"dll").expect("dll");
        }

        assert_eq!(
            compiled_ids(&data.join("Engine").join("Mods")),
            ["uxtheme-hook"]
        );
    }

    #[test]
    fn removing_compiled_files_deletes_every_architecture_and_leaves_others() {
        let root = tempdir().expect("temp dir");
        let data = data_dir(root.path());
        let folder = data.join("Engine").join("Mods").join("64");
        fs::write(folder.join("accent-color-sync_1.0_1.dll"), b"dll").expect("ours");
        fs::write(folder.join("uxtheme-hook_1.4_1.dll"), b"dll").expect("theirs");

        let removed = remove_compiled(&data, "accent-color-sync").expect("remove");

        assert_eq!(removed, 1);
        assert!(!folder.join("accent-color-sync_1.0_1.dll").exists());
        assert!(folder.join("uxtheme-hook_1.4_1.dll").exists());
        // A second removal is success and removes nothing.
        assert_eq!(
            remove_compiled(&data, "accent-color-sync").expect("again"),
            0
        );
    }

    #[test]
    fn removing_an_unsafe_id_never_touches_a_file() {
        let root = tempdir().expect("temp dir");
        let data = data_dir(root.path());

        assert!(remove_compiled(&data, "../../evil").is_err());
    }

    #[test]
    fn the_source_path_comes_from_the_plan_validation() {
        let data = Path::new(r"C:\ProgramData\Windhawk");

        assert_eq!(
            source_path(data, "accent-color-sync").expect("path"),
            data.join("ModsSource").join("accent-color-sync.wh.cpp")
        );
        assert!(source_path(data, "../evil").is_err());
    }
}
