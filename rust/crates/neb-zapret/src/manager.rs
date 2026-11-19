//! The state machine behind `zapret.*`: installed, starting, running, stopping, stopped.
//!
//! The manager is not elevated. It owns the pack (import, strategy parsing, config) and delegates
//! every action that touches `winws.exe` to [`Host`]. The truth about a running process lives in
//! `status.json`, written by the elevated helper, so a manager that restarts between two commands
//! still reports the same state.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use neb_core::config::{Config, ConfigStore};
use serde::Serialize;

use crate::host::{Host, HostError, Launch};
use crate::pack::{self, PackError};
use crate::paths::Paths;
use crate::selftest;
use crate::settings;
use crate::status::{self, StatusFile};
use crate::strategy::{self, SkippedStrategy, Strategy, StrategyError};

/// How long a start may sit without a status before it is reported as failed.
///
/// Long enough for a user to read the UAC dialog and type a password, short enough that a helper
/// which never started does not leave the UI waiting forever.
pub const PENDING_TIMEOUT: Duration = Duration::from_secs(120);

/// The document `zapret.getState` returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateDoc {
    /// Whether a usable pack was found.
    pub installed: bool,
    /// Version marker of the pack, empty when nothing is installed.
    pub pack_version: String,
    /// Strategy recorded in the status file or chosen by the user.
    pub strategy: Option<String>,
    /// Whether `winws.exe` is alive.
    pub running: bool,
    /// Pid of `winws.exe` while it is alive.
    pub pid: Option<u32>,
    /// Why the last action failed, in the helper's words.
    pub last_error: Option<String>,
    /// Whether a start is waiting for the helper to report.
    pub pending: bool,
    /// Whether a stop was requested and the process has not gone yet.
    pub stopping: bool,
    /// Folder the pack was found in.
    pub pack_path: Option<String>,
    /// A short note about the last transition.
    pub message: String,
}

/// The answer to `zapret.listStrategies`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StrategyList {
    /// Whether a pack was found.
    pub installed: bool,
    /// Version marker of that pack.
    pub pack_version: String,
    /// Usable strategies, sorted by file name.
    pub strategies: Vec<Strategy>,
    /// Scripts that were skipped and why.
    pub skipped: Vec<SkippedStrategy>,
}

/// Everything a manager method can fail with.
#[derive(Debug, thiserror::Error)]
pub enum ManagerError {
    /// No data directory could be resolved.
    #[error(transparent)]
    Paths(#[from] neb_core::paths::PathsError),

    /// There is no usable pack.
    #[error("no zapret pack is imported; run 'zapret.import' with the pack folder first")]
    NotInstalled,

    /// A start or stop is already in flight, or the process is already running.
    #[error("winws.exe is already running or starting")]
    AlreadyRunning,

    /// There is nothing to stop.
    #[error("winws.exe is not running")]
    NotRunning,

    /// The pack cannot be replaced while its process is alive.
    #[error("cannot import a pack while winws.exe is running or stopping")]
    Busy,

    /// The strategy is not in the pack.
    #[error("strategy '{strategy}' is not in this pack; choose one of: {available}")]
    UnknownStrategy {
        /// The requested name.
        strategy: String,
        /// Names that do exist.
        available: String,
    },

    /// The strategy scripts could not be read.
    #[error(transparent)]
    Strategies(#[from] StrategyError),

    /// The import failed.
    #[error(transparent)]
    Import(#[from] PackError),

    /// The helper could not be started.
    #[error(transparent)]
    Host(#[from] HostError),

    /// A file operation failed.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation.
        action: &'static str,
        /// Path the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },
}

/// The pack-and-process state machine.
#[derive(Debug)]
pub struct Manager<H: Host> {
    paths: Paths,
    config: Config,
    host: H,
    state: StateDoc,
    pending_since: Option<Instant>,
}

impl<H: Host> Manager<H> {
    /// Opens the manager for the real data directory.
    ///
    /// # Errors
    ///
    /// [`ManagerError::Paths`] when no data directory can be resolved.
    pub fn open(host: H) -> Result<Self, ManagerError> {
        Ok(Self::at(Paths::try_default()?, host))
    }

    /// Opens the manager for an explicit layout, used by tests and portable runs.
    pub fn at(paths: Paths, host: H) -> Self {
        let config = ConfigStore::new(&paths.config_file).load();
        let strategy = config.zapret.strategy.clone();
        let mut manager = Self {
            paths,
            config,
            host,
            state: StateDoc {
                strategy,
                ..StateDoc::default()
            },
            pending_since: None,
        };

        manager.refresh();
        manager
    }

    /// The layout this manager addresses.
    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// The loaded config, including the zapret section.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The host, for a test that wants to inspect what the manager asked it to do.
    pub fn host(&self) -> &H {
        &self.host
    }

    /// The current state, after folding in the status file.
    pub fn state(&mut self) -> StateDoc {
        self.refresh();
        self.state.clone()
    }

    /// The current state without touching the file system again.
    pub fn snapshot(&self) -> StateDoc {
        self.state.clone()
    }

    /// Re-reads the pack and the status file and republishes the state.
    pub fn refresh(&mut self) {
        let resolved = settings::resolve(&self.config, &self.paths);
        self.state.installed = resolved.pack.is_some();
        self.state.pack_version = resolved
            .pack
            .as_ref()
            .map(|pack| pack.version.clone())
            .unwrap_or_default();
        self.state.pack_path = resolved
            .pack
            .as_ref()
            .map(|pack| pack.root.display().to_string());

        if let Some(note) = resolved.note {
            self.state.message = note;
        }

        if let Some(error) = self.host.take_error() {
            self.state.last_error = Some(error.clone());
            self.state.message = error;
            self.state.pending = false;
            self.pending_since = None;
        }

        match status::read(&self.paths.status_file) {
            Ok(Some(file)) => self.apply_status(file),
            Ok(None) => {
                self.state.running = false;
                self.state.pid = None;
                self.state.stopping = false;
            }
            Err(error) => {
                self.state.running = false;
                self.state.pid = None;
                self.state.pending = false;
                self.state.stopping = false;
                self.state.last_error = Some(format!("cannot read the status file: {error}"));
            }
        }

        if self.state.pending
            && self
                .pending_since
                .is_some_and(|since| since.elapsed() > PENDING_TIMEOUT)
        {
            self.state.pending = false;
            self.state.last_error = Some(
                "the elevated helper did not report back; the UAC prompt may have been cancelled"
                    .to_owned(),
            );
            self.state.message = self.state.last_error.clone().unwrap_or_default();
            self.pending_since = None;
        }
    }

    /// Folds one status document into the state.
    fn apply_status(&mut self, file: StatusFile) {
        if !file.strategy.is_empty() {
            self.state.strategy = Some(file.strategy.clone());
        }
        if !(self.state.stopping && file.running) {
            self.state.message = file.message.clone();
        }

        if file.running {
            let alive = file.pid.is_some_and(|pid| self.host.pid_alive(pid));
            self.state.running = alive;
            self.state.pid = if alive { file.pid } else { None };
            self.state.pending = false;
            self.pending_since = None;
            self.state.last_error = file.last_error.clone();

            if alive && !file.stop_requested {
                self.state.stopping = false;
            }

            return;
        }

        self.state.running = false;
        self.state.pid = None;
        self.state.pending = false;
        self.state.stopping = false;
        self.pending_since = None;
        self.state.last_error = file.last_error.clone();
    }

    /// Lists the strategies of the current pack. No pack is an empty list, not an error.
    ///
    /// # Errors
    ///
    /// [`ManagerError::Strategies`] when the pack folder cannot be read.
    pub fn list_strategies(&mut self) -> Result<StrategyList, ManagerError> {
        self.refresh();
        let resolved = settings::resolve(&self.config, &self.paths);
        let Some(pack) = resolved.pack else {
            return Ok(StrategyList::default());
        };

        let loaded = strategy::load(&pack)?;

        Ok(StrategyList {
            installed: true,
            pack_version: pack.version,
            strategies: loaded.strategies,
            skipped: loaded.skipped,
        })
    }

    /// Starts `strategy`: validates it, then asks the host for the prepared helper.
    ///
    /// `Ok` with [`StateDoc::pending`] set means the helper was asked to start; the next
    /// [`refresh`](Self::refresh) turns that into `running` once the status file names a live pid.
    ///
    /// # Errors
    ///
    /// [`ManagerError::NotInstalled`], [`ManagerError::AlreadyRunning`],
    /// [`ManagerError::UnknownStrategy`], [`ManagerError::Strategies`], [`ManagerError::Host`].
    pub fn start(&mut self, strategy_name: &str) -> Result<StateDoc, ManagerError> {
        self.refresh();

        if self.state.running || self.state.pending || self.state.stopping {
            return Err(ManagerError::AlreadyRunning);
        }

        let resolved = settings::resolve(&self.config, &self.paths);
        let Some(pack) = resolved.pack else {
            let error = ManagerError::NotInstalled;
            self.state.last_error = Some(error.to_string());
            self.state.message = error.to_string();
            return Err(error);
        };

        let loaded = strategy::load(&pack)?;
        let strategy = find_strategy(&loaded.strategies, strategy_name).ok_or_else(|| {
            let error = ManagerError::UnknownStrategy {
                strategy: strategy_name.to_owned(),
                available: loaded
                    .strategies
                    .iter()
                    .map(|strategy| strategy.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            };
            self.state.last_error = Some(error.to_string());
            self.state.message = error.to_string();
            error
        })?;

        // A status left by the previous run would otherwise be read as this run's state.
        let _ = fs::remove_file(&self.paths.status_file);

        let elevate = !self.host.is_admin();
        let launch = self
            .host
            .start(&strategy.name, &pack.root, &self.paths.status_file, elevate)
            .map_err(|error| {
                self.state.last_error = Some(error.to_string());
                self.state.message = error.to_string();
                self.state.pending = false;
                ManagerError::Host(error)
            })?;

        self.state.pending = true;
        self.state.stopping = false;
        self.state.running = false;
        self.state.pid = None;
        self.state.strategy = Some(strategy.name.clone());
        self.state.last_error = None;
        self.state.message = match launch {
            Launch::Elevated => "ждём подтверждения UAC".to_owned(),
            Launch::Direct(pid) => {
                format!("helper started (pid {pid}), waiting for winws.exe to report")
            }
        };
        self.pending_since = Some(Instant::now());

        self.config.zapret.strategy = Some(strategy.name.clone());
        self.save_config();

        Ok(self.state.clone())
    }

    /// Stops the process the status file records.
    ///
    /// # Errors
    ///
    /// [`ManagerError::NotRunning`] when there is nothing to stop, [`ManagerError::Host`] when the
    /// helper cannot be started.
    pub fn stop(&mut self) -> Result<StateDoc, ManagerError> {
        self.refresh();

        if !self.state.running && !self.state.pending && !self.state.stopping {
            return Err(ManagerError::NotRunning);
        }

        let resolved = settings::resolve(&self.config, &self.paths);
        let Some(pack) = resolved.pack else {
            let error = ManagerError::NotInstalled;
            self.state.last_error = Some(error.to_string());
            return Err(error);
        };

        let elevate = !self.host.is_admin();
        self.host
            .stop(&pack.root, &self.paths.status_file, elevate)
            .map_err(|error| {
                self.state.last_error = Some(error.to_string());
                self.state.message = error.to_string();
                ManagerError::Host(error)
            })?;

        self.state.stopping = true;
        self.state.pending = false;
        self.state.message = "остановка winws.exe…".to_owned();
        self.pending_since = Some(Instant::now());

        // A direct helper usually finishes before this read; an elevated one answers later and the
        // caller sees `stopping` until the status changes.
        self.refresh();
        Ok(self.state.clone())
    }

    /// Imports `source` over the module's data directory.
    ///
    /// # Errors
    ///
    /// [`ManagerError::Busy`] while a process is alive, [`ManagerError::Import`] when the source is
    /// not a usable pack or the copy fails.
    pub fn import(&mut self, source: &Path) -> Result<StateDoc, ManagerError> {
        self.refresh();

        if self.state.running || self.state.pending || self.state.stopping {
            return Err(ManagerError::Busy);
        }

        let pack = pack::import(source, &self.paths.data_dir)?;
        self.config.zapret.pack_path = Some(pack.root.display().to_string());
        self.save_config();

        self.state.last_error = None;
        self.refresh();
        self.state.message = format!(
            "imported zapret pack {} ({})",
            pack.version,
            pack.root.display()
        );
        Ok(self.state.clone())
    }

    /// Runs the readiness checks against the live machine.
    pub fn self_test(&mut self) -> selftest::Report {
        self.refresh();
        selftest::collect()
    }

    /// Saves the zapret selection; a failed save is reported in the message, not as a failed start.
    fn save_config(&mut self) {
        let store = ConfigStore::new(&self.paths.config_file);

        if let Err(error) = store.save(&self.config) {
            let note = format!("the config was not saved: {error}");
            if self.state.message.is_empty() {
                self.state.message = note;
            } else {
                self.state.message = format!("{}; {note}", self.state.message);
            }
        }
    }
}

fn find_strategy<'a>(strategies: &'a [Strategy], name: &str) -> Option<&'a Strategy> {
    let name = name.trim();

    strategies
        .iter()
        .find(|strategy| strategy.name == name)
        .or_else(|| {
            strategies
                .iter()
                .find(|strategy| strategy.name.eq_ignore_ascii_case(name))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{BIN_DIR, LISTS_DIR, Pack, WINDIVERT_SYS, WINWS_EXE};
    use crate::status;
    use std::collections::BTreeSet;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
    use tempfile::tempdir;

    /// A host that writes the status file the way the real helper does, with no UAC involved.
    #[derive(Debug)]
    struct FakeHost {
        admin: bool,
        alive: Mutex<BTreeSet<u32>>,
        calls: Mutex<Vec<String>>,
        error: Mutex<Option<String>>,
        next_pid: AtomicU32,
        fail_start: AtomicBool,
    }

    impl FakeHost {
        fn new(admin: bool) -> Self {
            Self {
                admin,
                alive: Mutex::new(BTreeSet::new()),
                calls: Mutex::new(Vec::new()),
                error: Mutex::new(None),
                next_pid: AtomicU32::new(5000),
                fail_start: AtomicBool::new(false),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone()
        }

        fn die(&self, pid: u32) {
            self.alive
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&pid);
        }
    }

    impl Host for FakeHost {
        fn is_admin(&self) -> bool {
            self.admin
        }

        fn start(
            &self,
            strategy: &str,
            pack: &Path,
            status_path: &Path,
            elevate: bool,
        ) -> Result<Launch, HostError> {
            if self.fail_start.load(Ordering::SeqCst) {
                return Err(HostError::Spawn(io::Error::other("refused")));
            }

            let pid = self.next_pid.fetch_add(1, Ordering::SeqCst);
            let version = Pack::open(pack).map_or_else(|_| String::new(), |pack| pack.version);
            let mut file = StatusFile::started(pid, strategy, &version, !elevate);
            file.message = "started by the fake host".to_owned();
            status::write(status_path, &file).expect("fake status");
            self.alive
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(pid);
            self.calls
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(format!("start {strategy} elevate={elevate}"));
            Ok(Launch::Direct(pid))
        }

        fn stop(&self, _pack: &Path, status_path: &Path, elevate: bool) -> Result<(), HostError> {
            let mut file = status::read(status_path).ok().flatten().unwrap_or_default();
            if let Some(pid) = file.pid {
                self.alive
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&pid);
                file.stop_requested = true;
                file.message = format!("winws.exe (pid {pid}) stopped");
            }
            file.pid = None;
            file.running = false;
            file.last_error = None;
            status::write(status_path, &file).expect("fake status");
            self.calls
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(format!("stop elevate={elevate}"));
            Ok(())
        }

        fn pid_alive(&self, pid: u32) -> bool {
            self.alive
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains(&pid)
        }

        fn take_error(&self) -> Option<String> {
            self.error
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .take()
        }
    }

    fn pack(root: &Path) {
        fs::create_dir_all(root.join(BIN_DIR)).expect("bin");
        fs::create_dir_all(root.join(LISTS_DIR)).expect("lists");
        fs::write(root.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        fs::write(root.join(BIN_DIR).join(WINDIVERT_SYS), b"sys").expect("driver");
        fs::write(
            root.join("general.bat"),
            "@echo off\r\nstart \"\" \"%~dp0bin\\winws.exe\" --new\r\n",
        )
        .expect("general");
        fs::write(
            root.join("general (ALT).bat"),
            "@echo off\r\nstart \"\" \"%~dp0bin\\winws.exe\" --new --alt\r\n",
        )
        .expect("alt");
    }

    fn manager() -> (tempfile::TempDir, Manager<FakeHost>) {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        pack(&paths.data_dir);
        let manager = Manager::at(paths, FakeHost::new(false));

        (directory, manager)
    }

    #[test]
    fn a_fresh_manager_with_a_pack_is_installed_but_stopped() {
        let (_directory, mut manager) = manager();
        let state = manager.state();

        assert!(state.installed);
        assert!(!state.running);
        assert_eq!(state.pid, None);
        assert!(!state.pending);
        assert_eq!(state.strategy, None);
        assert_eq!(state.pack_version.len(), 16);
        assert!(state.pack_path.is_some());
    }

    #[test]
    fn the_strategy_list_is_sorted_and_carries_the_argv_length() {
        let (_directory, mut manager) = manager();
        let list = manager.list_strategies().expect("list");

        assert!(list.installed);
        assert_eq!(list.strategies.len(), 2);
        assert_eq!(list.strategies[0].name, "general");
        assert_eq!(list.strategies[0].args, vec!["--new".to_owned()]);
        assert_eq!(list.strategies[1].name, "general (ALT)");
        assert_eq!(list.strategies[1].argument_count(), 2);
        assert_eq!(list.skipped.len(), 0);
    }

    #[test]
    fn the_state_walks_from_installed_to_started_to_stopped() {
        let (_directory, mut manager) = manager();

        assert!(manager.snapshot().installed);
        assert!(!manager.snapshot().running);

        let starting = manager.start("general").expect("start");
        assert!(starting.pending, "the helper was asked to start");
        assert!(starting.message.contains("helper started"));

        manager.refresh();
        let running = manager.snapshot();
        assert!(running.running);
        assert_eq!(running.pid, Some(5000));
        assert_eq!(running.strategy.as_deref(), Some("general"));
        assert!(!running.pending);
        assert!(running.last_error.is_none());

        let stopping = manager.stop().expect("stop");
        assert!(!stopping.running);
        assert_eq!(stopping.pid, None);
        assert!(stopping.message.contains("stopped"));

        let calls = manager.host().calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].starts_with("start general"));
        assert!(calls[1].starts_with("stop"));
        assert!(
            calls[0].ends_with("elevate=true"),
            "a non-elevated manager asks for UAC: {calls:?}"
        );
    }

    #[test]
    fn starting_twice_is_refused_and_stopping_twice_is_refused() {
        let (_directory, mut manager) = manager();

        manager.start("general").expect("start");
        assert!(matches!(
            manager.start("general (ALT)"),
            Err(ManagerError::AlreadyRunning)
        ));

        manager.stop().expect("stop");
        assert!(matches!(manager.stop(), Err(ManagerError::NotRunning)));
    }

    #[test]
    fn an_unknown_strategy_is_refused_with_the_names_that_exist() {
        let (_directory, mut manager) = manager();

        match manager.start("general (MISSING)") {
            Err(ManagerError::UnknownStrategy {
                strategy,
                available,
            }) => {
                assert_eq!(strategy, "general (MISSING)");
                assert!(available.contains("general (ALT)"), "{available}");
            }
            other => panic!("expected UnknownStrategy, got {other:?}"),
        }
        assert!(manager.snapshot().last_error.is_some());
    }

    #[test]
    fn a_start_that_the_host_refuses_becomes_a_last_error() {
        let (_directory, mut manager) = manager();
        manager.host().fail_start.store(true, Ordering::SeqCst);

        assert!(matches!(
            manager.start("general"),
            Err(ManagerError::Host(_))
        ));

        let state = manager.snapshot();
        assert!(!state.pending);
        assert!(state.last_error.is_some());
    }

    #[test]
    fn a_pid_that_is_gone_reads_as_stopped() {
        let (_directory, mut manager) = manager();
        manager.start("general").expect("start");
        manager.refresh();
        let pid = manager.snapshot().pid.expect("a pid");

        manager.host().die(pid);
        manager.refresh();

        let state = manager.snapshot();
        assert!(!state.running);
        assert_eq!(state.pid, None);
    }

    #[test]
    fn importing_saves_the_pack_path_and_replaces_the_strategies() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        let mut manager = Manager::at(paths.clone(), FakeHost::new(true));
        assert!(!manager.snapshot().installed);

        let source = directory.path().join("downloaded");
        fs::create_dir_all(source.join(BIN_DIR)).expect("bin");
        fs::write(source.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        fs::write(source.join(BIN_DIR).join(WINDIVERT_SYS), b"sys").expect("driver");
        fs::write(
            source.join("general.bat"),
            "@echo off\r\nstart \"\" \"%~dp0bin\\winws.exe\" --new\r\n",
        )
        .expect("strategy");

        let state = manager.import(&source).expect("import");

        assert!(state.installed);
        assert!(state.pack_path.is_some());
        assert!(manager.snapshot().message.contains("imported"));
        assert_eq!(manager.list_strategies().expect("list").strategies.len(), 1);
        assert_eq!(
            manager.config().zapret.pack_path.as_deref(),
            state.pack_path.as_deref()
        );

        let saved = ConfigStore::new(&paths.config_file).load();
        assert_eq!(saved.zapret.pack_path, state.pack_path);
    }

    #[test]
    fn importing_while_running_is_refused() {
        let (_directory, mut manager) = manager();
        manager.start("general").expect("start");
        manager.refresh();

        let source = std::env::temp_dir();

        assert!(matches!(manager.import(&source), Err(ManagerError::Busy)));
    }

    #[test]
    fn a_missing_pack_makes_start_and_stop_explain_themselves() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        let mut manager = Manager::at(paths, FakeHost::new(true));

        assert!(matches!(
            manager.start("general"),
            Err(ManagerError::NotInstalled)
        ));
        assert!(matches!(manager.stop(), Err(ManagerError::NotRunning)));
        assert!(
            manager
                .list_strategies()
                .expect("list")
                .strategies
                .is_empty()
        );
    }

    #[test]
    fn the_self_test_always_reports_something_about_a_missing_pack() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        let mut manager = Manager::at(paths, FakeHost::new(true));

        let report = manager.self_test();

        assert!(!report.ok);
        assert!(report.checks.iter().any(|check| check.id == "pack"));
    }

    #[test]
    fn a_configured_external_pack_is_used_and_imports_into_the_data_directory() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        let external = directory.path().join("external");
        pack(&external);

        let mut config = Config::default();
        config.zapret.pack_path = Some(external.display().to_string());
        ConfigStore::new(&paths.config_file)
            .save(&config)
            .expect("save config");

        let mut manager = Manager::at(paths.clone(), FakeHost::new(true));
        assert_eq!(
            manager.snapshot().pack_path.as_deref(),
            Some(external.display().to_string().as_str())
        );

        manager.start("general").expect("start");
        manager.stop().expect("stop");
        assert_eq!(manager.snapshot().strategy.as_deref(), Some("general"));
    }
}
