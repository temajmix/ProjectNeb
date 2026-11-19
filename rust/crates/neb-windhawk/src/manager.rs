//! The non-elevated state machine the `ProjectNeb.windhawk` pipe answers from.
//!
//! The manager owns no Windows handle. It resolves the configuration and data directories, asks
//! the [`Host`] for detection, downloads, and delegates every write to a plan. That split is what
//! lets the state machine, the cache, the catalog and the install outcomes be tested without a
//! Windhawk installation, UAC or the network.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use neb_core::config::{Config, ConfigStore};
use serde::Serialize;

use crate::catalog::{self, Catalog, CatalogEntry, Fetcher};
use crate::detect::Detection;
use crate::host::Host;
use crate::installed::{self, InstalledReport};
use crate::paths::{self, Paths};
use crate::plan::{ApplyPlan, ApplyResult, sanitize_id};
use crate::registry::ModRegistry;

/// Default number of catalog rows a page carries.
pub const DEFAULT_LIMIT: usize = 50;

/// Most rows one request may ask for.
pub const MAX_LIMIT: usize = 500;

/// Everything `windhawk.getState` reports.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateDoc {
    /// Whether Windhawk is installed.
    pub installed: bool,
    /// Version reported by the installation.
    pub version: Option<String>,
    /// `windhawk.exe`.
    pub path: Option<String>,
    /// Effective data directory after the `windhawk.dataDir` override.
    pub data_dir: Option<String>,
    /// Data directory detection named, before the override.
    pub detected_data_dir: Option<String>,
    /// Whether `windhawk.exe` is running.
    pub running: bool,
    /// Which candidate matched.
    pub source: Option<String>,
    /// Whether the supervisor should run `neb-windhawk.exe`.
    pub module_enabled: bool,
    /// Whether it should start with the shell.
    pub auto_start: bool,
    /// `<data>\ModsSource`.
    pub mods_source: Option<String>,
    /// `<data>\Engine\Mods`.
    pub engine_mods: Option<String>,
    /// Number of installed mods.
    pub installed_count: usize,
    /// Number of installed mods the registry proves are enabled.
    pub enabled_count: usize,
    /// Whether a catalog snapshot exists on disk.
    pub catalog_cached: bool,
}

/// One catalog page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogPage {
    /// The query that was answered.
    pub query: String,
    /// The cap that was applied.
    pub limit: usize,
    /// The number of rows before the query and the cap.
    pub total: usize,
    /// Unix seconds of the snapshot.
    pub fetched_at: u64,
    /// `github` or `cache`.
    pub source: String,
    /// Whether the snapshot is older than six hours because a refresh failed.
    pub stale: bool,
    /// Rows.
    pub entries: Vec<CatalogEntry>,
}

/// The result of `windhawk.install`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallOutcome {
    /// Mod id.
    pub id: String,
    /// Whether the source is on disk.
    pub installed: bool,
    /// `enabled`, `disabled` or `unknown`; never a guess.
    pub enabled: String,
    /// Whether the UI should offer "Открыть Windhawk".
    pub needs_windhawk_ui: bool,
    /// Source file that was written.
    pub source_path: Option<String>,
    /// Registry key, only when the enabled state was proven.
    pub config_key: Option<String>,
    /// Non-fatal explanation.
    pub note: Option<String>,
}

/// The result of `windhawk.remove`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoveOutcome {
    /// Mod id.
    pub id: String,
    /// Whether the source still exists; `false` after a successful removal.
    pub installed: bool,
    /// Non-fatal explanation, e.g. a configuration key that needed Windhawk itself.
    pub note: Option<String>,
}

/// Everything that can go wrong in a manager command.
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    /// No ProjectNeb data root.
    #[error(transparent)]
    Paths(#[from] neb_core::paths::PathsError),

    /// Windhawk is not installed and has no data directory to write into.
    #[error("Windhawk is not installed; install it before adding mods")]
    NotInstalled,

    /// The id is unsafe or malformed.
    #[error(transparent)]
    Id(#[from] crate::plan::PlanError),

    /// A local file could not be used.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation.
        action: &'static str,
        /// File.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The catalog or a source could not be downloaded.
    #[error(transparent)]
    Catalog(#[from] catalog::CatalogError),

    /// The source download failed.
    #[error("cannot download the source of '{id}': {message}")]
    Download {
        /// Mod id.
        id: String,
        /// The transport's message.
        message: String,
    },

    /// The helper refused or could not act.
    #[error("cannot {action} '{id}': {message}")]
    Apply {
        /// Verb.
        action: &'static str,
        /// Mod id.
        id: String,
        /// The helper's explanation.
        message: String,
    },

    /// The mod is not installed, so it cannot be removed.
    #[error("'{0}' is not installed")]
    NotInstalledMod(String),
}

/// The manager behind the IPC mutex.
#[derive(Debug)]
pub struct Manager<H: Host> {
    paths: Paths,
    config: Config,
    host: H,
    registry: Box<dyn ModRegistry>,
}

impl<H: Host> Manager<H> {
    /// Opens the manager for the real data directory.
    ///
    /// # Errors
    ///
    /// [`ManagerError::Paths`] when no data directory can be resolved.
    pub fn open(host: H, registry: Box<dyn ModRegistry>) -> Result<Self, ManagerError> {
        Ok(Self::at(Paths::try_default()?, host, registry))
    }

    /// Opens the manager for an explicit layout, used by tests and portable runs.
    pub fn at(paths: Paths, host: H, registry: Box<dyn ModRegistry>) -> Self {
        let config = ConfigStore::new(&paths.config_file).load();

        Self {
            paths,
            config,
            host,
            registry,
        }
    }

    /// The host, for tests that drive its knobs.
    pub fn host(&self) -> &H {
        &self.host
    }

    /// The parsed configuration.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The current state.
    pub fn state(&self) -> StateDoc {
        let detection = self.host.detect();
        let data_dir = self.data_dir(&detection);
        let report = installed::list(&data_dir, self.registry.as_ref());

        StateDoc {
            installed: detection.installed,
            version: detection.version,
            path: detection.path,
            data_dir: Some(data_dir.display().to_string()),
            detected_data_dir: detection.data_dir,
            running: detection.running,
            source: detection.source,
            module_enabled: self.config.windhawk.enabled,
            auto_start: self.config.windhawk.auto_start,
            mods_source: Some(paths::mods_source(&data_dir).display().to_string()),
            engine_mods: Some(paths::engine_mods(&data_dir).display().to_string()),
            installed_count: report.len(),
            enabled_count: report
                .mods
                .iter()
                .filter(|entry| entry.enabled == Some(true))
                .count(),
            catalog_cached: self.paths.catalog_file.is_file(),
        }
    }

    /// Lists the installed mods.
    pub fn list_installed(&self) -> InstalledReport {
        let detection = self.host.detect();
        installed::list(&self.data_dir(&detection), self.registry.as_ref())
    }

    /// Lists catalog rows matching `query`.
    ///
    /// # Errors
    ///
    /// [`ManagerError::Catalog`] when neither a snapshot nor a fetch is usable.
    pub fn list_catalog(&self, query: &str, limit: usize) -> Result<CatalogPage, ManagerError> {
        let detection = self.host.detect();
        let report = installed::list(&self.data_dir(&detection), self.registry.as_ref());
        let installed: BTreeSet<String> = report.ids().into_iter().collect();

        let fetcher = HostFetcher { host: &self.host };
        let catalog = catalog::load(
            &fetcher,
            &self.paths.catalog_file,
            catalog::now_unix(),
            &|id| installed.contains(id),
        )?;

        Ok(page(catalog, query, limit))
    }

    /// Downloads and installs `raw_id`.
    ///
    /// The source is written into a temp data directory directly and into the machine-wide tree
    /// through the elevated helper. The enabled state is reported as `unknown` unless the helper
    /// wrote `Disabled` and read it back.
    ///
    /// # Errors
    ///
    /// [`ManagerError`] for an unsafe id, a missing installation, a failed download or a helper
    /// that could not write the source.
    pub fn install(&self, raw_id: &str) -> Result<InstallOutcome, ManagerError> {
        let id = sanitize_id(raw_id)?;
        let detection = self.host.detect();
        if !detection.installed {
            return Err(ManagerError::NotInstalled);
        }

        let data_dir = self.data_dir(&detection);
        let url = catalog::install_url(&id);
        let bytes = self
            .host
            .fetch(&url)
            .map_err(|message| ManagerError::Download {
                id: id.clone(),
                message,
            })?;

        if bytes.is_empty() {
            return Err(ManagerError::Download {
                id,
                message: "the server returned an empty file".to_owned(),
            });
        }

        self.ensure_cache_dir()?;
        let serial = next_serial();
        let download = self
            .paths
            .cache_dir
            .join(format!("download-{id}-{serial}.wh.cpp"));
        fs::write(&download, &bytes).map_err(|source| ManagerError::Io {
            action: "write",
            path: download.clone(),
            source,
        })?;

        let result_path = self.paths.apply_file(&id, serial);
        let plan = ApplyPlan::install(&id, &data_dir, &download, &result_path, true);
        let result = self.host.apply(&plan);
        let _ = fs::remove_file(&download);

        match result {
            Ok(result) => outcome(&id, result),
            Err(message) => Err(ManagerError::Apply {
                action: "install",
                id,
                message,
            }),
        }
    }

    /// Removes `raw_id`: source, configuration key and compiled DLLs.
    ///
    /// # Errors
    ///
    /// [`ManagerError::NotInstalledMod`] when nothing of the mod exists, or [`ManagerError::Apply`]
    /// when the helper refused.
    pub fn remove(&self, raw_id: &str) -> Result<RemoveOutcome, ManagerError> {
        let id = sanitize_id(raw_id)?;
        let detection = self.host.detect();
        let data_dir = self.data_dir(&detection);

        let report = installed::list(&data_dir, self.registry.as_ref());
        if !report.mods.iter().any(|entry| entry.id == id) {
            return Err(ManagerError::NotInstalledMod(id));
        }

        self.ensure_cache_dir()?;
        let result_path = self.paths.apply_file(&id, next_serial());
        let plan = ApplyPlan::remove(&id, &data_dir, &result_path);
        let result = self
            .host
            .apply(&plan)
            .map_err(|message| ManagerError::Apply {
                action: "remove",
                id: id.clone(),
                message,
            })?;

        if !result.ok {
            return Err(ManagerError::Apply {
                action: "remove",
                id,
                message: result
                    .error
                    .unwrap_or_else(|| "the helper did not report success".to_owned()),
            });
        }

        Ok(RemoveOutcome {
            id,
            installed: result.installed,
            note: result.note,
        })
    }

    /// Starts Windhawk, either its window or the tray-only process.
    ///
    /// # Errors
    ///
    /// The host's message when the executable is missing or Windows refuses the process.
    pub fn launch(&self, tray_only: bool) -> Result<(), String> {
        self.host.launch(tray_only)
    }

    /// The effective data directory: the configuration override, else the detected `AppDataPath`,
    /// else `%ProgramData%\Windhawk`.
    pub fn data_dir(&self, detection: &Detection) -> PathBuf {
        let detected = detection.data_dir.as_deref().map(Path::new);
        paths::windhawk_data_dir(self.config.windhawk.data_dir.as_deref(), detected)
    }

    fn ensure_cache_dir(&self) -> Result<(), ManagerError> {
        fs::create_dir_all(&self.paths.cache_dir).map_err(|source| ManagerError::Io {
            action: "create",
            path: self.paths.cache_dir.clone(),
            source,
        })
    }
}

fn page(catalog: Catalog, query: &str, limit: usize) -> CatalogPage {
    let limit = limit.clamp(1, MAX_LIMIT);

    let total = catalog.len();
    let entries = catalog.search(query, limit);

    CatalogPage {
        query: query.to_owned(),
        limit,
        total,
        fetched_at: catalog.fetched_at,
        source: catalog.source,
        stale: catalog.stale,
        entries,
    }
}

fn outcome(id: &str, result: ApplyResult) -> Result<InstallOutcome, ManagerError> {
    if !result.ok {
        return Err(ManagerError::Apply {
            action: "install",
            id: id.to_owned(),
            message: result
                .error
                .unwrap_or_else(|| "the helper did not report success".to_owned()),
        });
    }

    Ok(InstallOutcome {
        id: id.to_owned(),
        installed: result.installed,
        enabled: result.enabled,
        needs_windhawk_ui: result.needs_windhawk_ui,
        source_path: result.source_path,
        config_key: result.config_key,
        note: result.note,
    })
}

fn next_serial() -> u64 {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    SERIAL.fetch_add(1, Ordering::Relaxed)
}

/// A [`Fetcher`] over the manager's host.
struct HostFetcher<'a, H: Host> {
    host: &'a H,
}

impl<H: Host> std::fmt::Debug for HostFetcher<'_, H> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("HostFetcher")
    }
}

impl<H: Host> Fetcher for HostFetcher<'_, H> {
    fn get(&self, url: &str) -> Result<Vec<u8>, String> {
        self.host.fetch(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detection;
    use crate::plan::Action;
    use crate::registry::MemoryRegistry;
    use std::collections::BTreeMap;
    use std::sync::Mutex;
    use tempfile::tempdir;

    const CATALOG: &str = r#"[
        {"name":"accent-color-sync.wh.cpp","download_url":"https://raw/accent-color-sync.wh.cpp"},
        {"name":"windows-11-taskbar-styler.wh.cpp","download_url":"https://raw/taskbar.wh.cpp"},
        {"name":"uxtheme-hook.wh.cpp","download_url":"https://raw/uxtheme.wh.cpp"}
    ]"#;

    #[derive(Debug)]
    struct FakeHost {
        detection: Detection,
        sources: Mutex<BTreeMap<String, Vec<u8>>>,
        applied: Mutex<Vec<ApplyPlan>>,
        enabled: Mutex<String>,
        fetch_calls: AtomicU64,
    }

    impl FakeHost {
        fn new(data_dir: &Path) -> Self {
            Self {
                detection: Detection {
                    installed: true,
                    version: Some("1.7.3".to_owned()),
                    path: Some(r"A:\Windhawk\windhawk.exe".to_owned()),
                    data_dir: Some(data_dir.display().to_string()),
                    running: true,
                    source: Some("portable".to_owned()),
                },
                sources: Mutex::new(BTreeMap::new()),
                applied: Mutex::new(Vec::new()),
                enabled: Mutex::new("unknown".to_owned()),
                fetch_calls: AtomicU64::new(0),
            }
        }

        fn set_enabled(&self, value: &str) {
            *self
                .enabled
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = value.to_owned();
        }

        fn applied(&self) -> Vec<ApplyPlan> {
            self.applied
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }
    }

    impl Host for FakeHost {
        fn is_admin(&self) -> bool {
            false
        }

        fn detect(&self) -> Detection {
            self.detection.clone()
        }

        fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            self.fetch_calls.fetch_add(1, Ordering::Relaxed);
            if url == catalog::CATALOG_URL {
                return Ok(CATALOG.as_bytes().to_vec());
            }

            let id = url
                .strip_prefix(catalog::RAW_BASE)
                .and_then(|rest| rest.strip_prefix('/'))
                .and_then(|rest| rest.strip_suffix(".wh.cpp"))
                .ok_or_else(|| format!("unexpected url '{url}'"))?;

            self.sources
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(id)
                .cloned()
                .ok_or_else(|| format!("no source for '{id}'"))
        }

        fn apply(&self, plan: &ApplyPlan) -> Result<ApplyResult, String> {
            plan.validate().map_err(|error| error.to_string())?;
            self.applied
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(plan.clone());

            match plan.action {
                Action::Install => {
                    let target = plan.target().map_err(|error| error.to_string())?;
                    let source = plan.source.as_deref().expect("validated install");
                    let bytes = fs::read(source).map_err(|error| error.to_string())?;
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                    }
                    fs::write(&target, bytes).map_err(|error| error.to_string())?;
                    let enabled = self
                        .enabled
                        .lock()
                        .unwrap_or_else(|poison| poison.into_inner())
                        .clone();

                    Ok(ApplyResult {
                        ok: true,
                        action: Action::Install,
                        id: plan.id.clone(),
                        installed: true,
                        needs_windhawk_ui: enabled == "unknown",
                        enabled,
                        source_path: Some(target.display().to_string()),
                        config_key: None,
                        error: None,
                        note: None,
                    })
                }
                Action::Remove => {
                    let target = plan.target().map_err(|error| error.to_string())?;
                    let _ = fs::remove_file(&target);
                    Ok(ApplyResult {
                        ok: true,
                        action: Action::Remove,
                        id: plan.id.clone(),
                        installed: target.exists(),
                        enabled: "unknown".to_owned(),
                        needs_windhawk_ui: false,
                        source_path: Some(target.display().to_string()),
                        config_key: None,
                        error: None,
                        note: None,
                    })
                }
            }
        }

        fn launch(&self, tray_only: bool) -> Result<(), String> {
            Err(format!("launch tray_only={tray_only}"))
        }
    }

    fn manager(directory: &tempfile::TempDir, host: FakeHost) -> Manager<FakeHost> {
        let paths = Paths::at(directory.path());
        Manager::at(paths, host, Box::new(MemoryRegistry::new()))
    }

    #[test]
    fn state_reports_the_detected_install_and_the_effective_data_directory() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let manager = manager(&directory, FakeHost::new(&data));

        let state = manager.state();

        assert!(state.installed);
        assert_eq!(state.version.as_deref(), Some("1.7.3"));
        assert_eq!(state.path.as_deref(), Some(r"A:\Windhawk\windhawk.exe"));
        assert_eq!(state.data_dir, Some(data.display().to_string()));
        assert!(state.running);
        assert_eq!(state.installed_count, 0);
        assert!(!state.module_enabled);
    }

    #[test]
    fn the_config_override_wins_over_the_detected_data_directory() {
        let directory = tempdir().expect("temp dir");
        let detected = directory.path().join("Detected");
        let override_dir = directory.path().join("Override");
        let paths = Paths::at(directory.path());
        fs::write(
            &paths.config_file,
            format!(
                r#"{{"windhawk":{{"enabled":true,"autoStart":true,"dataDir":{}}}}}"#,
                serde_json::json!(override_dir.display().to_string())
            ),
        )
        .expect("config");

        let manager = Manager::at(
            paths,
            FakeHost::new(&detected),
            Box::new(MemoryRegistry::new()),
        );

        assert_eq!(
            manager.state().data_dir,
            Some(override_dir.display().to_string())
        );
        assert!(manager.state().module_enabled);
        assert!(manager.state().auto_start);
    }

    #[test]
    fn list_catalog_marks_sources_on_disk_and_obeys_the_page() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let source_dir = data.join("ModsSource");
        fs::create_dir_all(&source_dir).expect("sources");
        fs::write(source_dir.join("accent-color-sync.wh.cpp"), b"// mod").expect("source");

        let manager = manager(&directory, FakeHost::new(&data));
        let page = manager.list_catalog("", 2).expect("catalog");

        assert_eq!(page.total, 3);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.source, "github");
        assert!(page.entries[0].installed);
        assert!(!page.entries[1].installed);

        let searched = manager.list_catalog("taskbar", 10).expect("search");
        assert_eq!(searched.entries.len(), 1);
        assert_eq!(searched.entries[0].id, "windows-11-taskbar-styler");
    }

    #[test]
    fn the_catalog_is_cached_between_calls() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let manager = manager(&directory, FakeHost::new(&data));

        manager.list_catalog("", 10).expect("first");
        let calls = manager.host().fetch_calls.load(Ordering::Relaxed);
        assert_eq!(calls, 1);
        assert!(Paths::at(directory.path()).catalog_file.is_file());

        manager.list_catalog("", 10).expect("second");
        assert_eq!(manager.host().fetch_calls.load(Ordering::Relaxed), calls);
    }

    #[test]
    fn an_install_downloads_and_writes_a_safe_plan() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let host = FakeHost::new(&data);
        host.sources
            .lock()
            .expect("sources")
            .insert("accent-color-sync".to_owned(), b"//! source".to_vec());
        let manager = manager(&directory, host);

        let outcome = manager.install("accent-color-sync").expect("install");

        assert!(outcome.installed);
        assert_eq!(outcome.enabled, "unknown");
        assert!(outcome.needs_windhawk_ui);
        assert!(
            data.join("ModsSource")
                .join("accent-color-sync.wh.cpp")
                .is_file()
        );

        let applied = manager.host().applied();
        assert_eq!(applied.len(), 1);
        assert_eq!(applied[0].action, Action::Install);
        assert_eq!(applied[0].id, "accent-color-sync");
        assert_eq!(applied[0].disabled, Some(false));
        let leftovers = fs::read_dir(directory.path().join("cache"))
            .expect("cache")
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.file_name().to_string_lossy().starts_with("download-"))
            .count();
        assert_eq!(leftovers, 0, "the download is removed after the helper");
    }

    #[test]
    fn an_install_reports_a_proven_enabled_state() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let host = FakeHost::new(&data);
        host.set_enabled("enabled");
        host.sources
            .lock()
            .expect("sources")
            .insert("accent-color-sync".to_owned(), b"//! source".to_vec());

        let outcome = manager(&directory, host)
            .install("accent-color-sync")
            .expect("install");

        assert_eq!(outcome.enabled, "enabled");
        assert!(!outcome.needs_windhawk_ui);
    }

    #[test]
    fn an_install_without_windhawk_is_refused() {
        let directory = tempdir().expect("temp dir");
        let mut host = FakeHost::new(directory.path());
        host.detection.installed = false;

        let error = manager(&directory, host)
            .install("accent-color-sync")
            .expect_err("must fail");

        assert!(matches!(error, ManagerError::NotInstalled));
    }

    #[test]
    fn a_traversal_id_never_reaches_the_download() {
        let directory = tempdir().expect("temp dir");
        let manager = manager(&directory, FakeHost::new(directory.path()));

        let error = manager.install("../../evil").expect_err("must fail");

        assert!(matches!(error, ManagerError::Id(_)));
        assert_eq!(manager.host().fetch_calls.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_missing_download_is_reported_with_the_id() {
        let directory = tempdir().expect("temp dir");
        let manager = manager(&directory, FakeHost::new(directory.path()));

        let error = manager.install("accent-color-sync").expect_err("must fail");

        assert!(matches!(error, ManagerError::Download { .. }));
        assert!(error.to_string().contains("accent-color-sync"));
    }

    #[test]
    fn remove_needs_an_installed_mod_and_then_deletes_it() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let source = data.join("ModsSource").join("accent-color-sync.wh.cpp");
        fs::create_dir_all(source.parent().expect("parent")).expect("sources");
        fs::write(&source, b"// mod").expect("source");
        let manager = manager(&directory, FakeHost::new(&data));

        assert!(matches!(
            manager.remove("uxtheme-hook"),
            Err(ManagerError::NotInstalledMod(_))
        ));

        let outcome = manager.remove("accent-color-sync").expect("remove");

        assert_eq!(outcome.id, "accent-color-sync");
        assert!(!outcome.installed);
        assert!(!source.exists());
        let applied = manager.host().applied();
        assert_eq!(applied[0].action, Action::Remove);
    }

    #[test]
    fn an_unsafe_remove_id_is_refused_before_any_file_is_touched() {
        let directory = tempdir().expect("temp dir");
        let manager = manager(&directory, FakeHost::new(directory.path()));

        assert!(matches!(manager.remove("a/b"), Err(ManagerError::Id(_))));
    }

    #[test]
    fn the_open_command_passes_the_tray_switch_to_the_host() {
        let directory = tempdir().expect("temp dir");
        let manager = manager(&directory, FakeHost::new(directory.path()));

        assert!(
            manager
                .launch(true)
                .expect_err("fake")
                .contains("tray_only=true")
        );
        assert!(
            manager
                .launch(false)
                .expect_err("fake")
                .contains("tray_only=false")
        );
    }

    #[test]
    fn the_limit_is_clamped_to_something_usable() {
        let directory = tempdir().expect("temp dir");
        let manager = manager(&directory, FakeHost::new(directory.path()));

        assert_eq!(manager.list_catalog("", 0).expect("zero").limit, 1);
        assert_eq!(
            manager.list_catalog("", usize::MAX).expect("huge").limit,
            MAX_LIMIT
        );
    }
}
