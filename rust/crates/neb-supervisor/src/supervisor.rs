//! The resident process manager: one supervisor owns the shell and the wallpaper.
//!
//! The supervisor is a plain synchronous loop. Every [`TICK`] it reaps dead children, starts the
//! ones whose restart is due, applies the commands the IPC handlers queued, and republishes the
//! state `supervisor.getState` answers from. Nothing blocks: a sleeping child is a `poll`, and the
//! IPC server runs on its own threads exactly as it does in the shell.
//!
//! What it guarantees, in the order the C# `Neb.App` supervisor establishes them:
//!
//! 1. one supervisor per session (a named mutex), and one copy of each child (a mutex per child);
//! 2. a crash restarts the child no sooner than [`children::RESTART_THROTTLE`], backing off
//!    through [`children::BACKOFF_STEPS`] while the crashes keep coming;
//! 3. a taskbar a crashed shell left hidden is put back before anything new starts;
//! 4. a stop the supervisor asked for is not a crash and starts nothing.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use neb_core::ipc::{self, Request, Response, ServerHandle};
use neb_core::logging::{Level, LogHandle};
use neb_core::time::UtcDateTime;
use serde::Serialize;
use serde_json::{Value, json};

use crate::children::{Child, ChildSpec, Enabled, NamedMutex};
use crate::settings;
use crate::taskbar_repair::{self, Outcome};
use crate::tray::Tray;
use crate::update::{self, UpdatesHandle, WhatsNew};
use crate::windows_mode::Mode;

/// Pipe the supervisor answers on.
pub const PIPE: &str = "ProjectNeb.supervisor";

/// Mutex that makes sure a session has exactly one supervisor.
pub const INSTANCE_MUTEX: &str = r"Local\ProjectNeb.supervisor";

/// Pipe the native shell answers on, used only to notice a shell this supervisor does not own.
pub const SHELL_PIPE: &str = "ProjectNeb.shell";

/// Log module name every line of this crate carries.
pub const MODULE: &str = "supervisor";

/// How often the loop reaps children and applies queued commands.
pub const TICK: Duration = Duration::from_millis(250);

/// Executable name of the bar, looked up next to `ProjectNeb.exe`.
const SHELL_EXE: &str = "neb-shell.exe";

/// Executable name of the living wallpaper, looked up next to `ProjectNeb.exe`.
const WALLPAPER_EXE: &str = "neb-wallpaper.exe";

/// Executable name of the Discord Rich Presence process, looked up next to `ProjectNeb.exe`.
const DISCORD_EXE: &str = "neb-discord.exe";
/// The tiling window manager, started only when `windowManager.enabled` is on.
const WM_EXE: &str = "neb-wm.exe";
/// The DPI-bypass manager, started only when `zapret.enabled` is on.
const ZAPRET_EXE: &str = "neb-zapret.exe";
/// The Windhawk integration, started only when `windhawk.enabled` is on.
const WINDHAWK_EXE: &str = "neb-windhawk.exe";

/// How long the supervisor waits for a shell to answer `shell.ping` at startup.
const SHELL_PROBE: Duration = Duration::from_secs(1);

/// How long `app.restartChild` waits for a restart to be applied before answering anyway.
///
/// The dashboard asks for a restart and cannot tell a slow start from a failed one, so the answer
/// is due after the same three seconds the plan promises, not after the child's own start timeout.
pub const RESTART_BUDGET: Duration = Duration::from_secs(3);

/// Everything the command line can change about a run.
#[derive(Debug, Clone)]
pub struct Options {
    /// Shell executable, when it is not the `neb-shell.exe` next to this one.
    pub shell: Option<PathBuf>,
    /// Wallpaper executable, when it is not the `neb-wallpaper.exe` next to this one.
    pub wallpaper: Option<PathBuf>,
    /// A single arbitrary command to supervise instead of the configured children.
    pub command: Option<(PathBuf, Vec<String>)>,
    /// Echo every transition to stdout as well as the log.
    pub echo: bool,
    /// Create the notification icon.
    pub tray: bool,
    /// Check the update manifest.
    pub updates: bool,
    /// Pipe name; [`PIPE`] unless a test wants its own.
    pub pipe: String,
    /// Instance mutex name; [`INSTANCE_MUTEX`] unless a test wants its own.
    pub instance: String,
    /// Config file to read the child switches from; the real one unless a test provides its own.
    pub config: Option<PathBuf>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            shell: None,
            wallpaper: None,
            command: None,
            echo: false,
            tray: true,
            updates: true,
            pipe: String::new(),
            instance: String::new(),
            config: None,
        }
    }
}

/// Why the supervisor could not start at all.
#[derive(Debug, thiserror::Error)]
pub enum SupervisorError {
    /// The pipe could not be served, so the process would be unmanageable.
    #[error("cannot serve '{pipe}': {source}")]
    Ipc {
        /// Pipe that failed.
        pipe: String,
        /// The IPC error.
        #[source]
        source: ipc::IpcError,
    },

    /// A named mutex could not be created, which is not something the supervisor can work around.
    #[error(transparent)]
    Mutex(#[from] crate::children::ChildError),
}

/// What `supervisor.getState` reports about one child.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChildState {
    /// Process id while the child is running.
    pub pid: Option<u32>,
    /// Whether the child is alive.
    pub running: bool,
}

/// The document `supervisor.getState` returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateDoc {
    /// The bar.
    pub shell: ChildState,
    /// The living wallpaper.
    pub wallpaper: ChildState,
    /// This supervisor's own process id, so a dashboard can measure it.
    pub pid: u32,
    /// Every child this supervisor owns, by the name `supervisor.restart` uses.
    pub children: BTreeMap<String, ChildState>,
    /// Seconds since the supervisor started.
    pub uptime_s: u64,
}

/// The state the IPC threads read and the commands they leave for the loop.
#[derive(Debug)]
pub struct Shared {
    state: Mutex<StateDoc>,
    restarts: Mutex<Vec<String>>,
    /// How many restarts the loop has applied per child, and the signal `app.restartChild` waits on.
    restarts_applied: Mutex<BTreeMap<String, u64>>,
    restart_signal: Condvar,
    stop: AtomicBool,
    open_settings: AtomicBool,
    started: Instant,
    names: Vec<String>,
    windows: Mutex<Mode>,
}

impl Shared {
    /// A shared view over the children called `names`, with the replacement mode detached.
    ///
    /// Only the tests build this shape; production always supplies a real [`Mode`].
    #[cfg(test)]
    pub fn new(names: Vec<String>) -> Self {
        Self::with_mode(names, Mode::detached())
    }

    /// A shared view whose Windows replacement mode is `windows`.
    pub fn with_mode(names: Vec<String>, windows: Mode) -> Self {
        Self {
            state: Mutex::new(StateDoc {
                pid: std::process::id(),
                ..StateDoc::default()
            }),
            restarts: Mutex::new(Vec::new()),
            restarts_applied: Mutex::new(BTreeMap::new()),
            restart_signal: Condvar::new(),
            stop: AtomicBool::new(false),
            open_settings: AtomicBool::new(false),
            started: Instant::now(),
            names,
            windows: Mutex::new(windows),
        }
    }

    /// Replaces the published state, deriving `shell`/`wallpaper` from `children`.
    pub fn publish_children(&self, children: BTreeMap<String, ChildState>) {
        let mut state = self.lock_state();
        state.shell = children.get("shell").cloned().unwrap_or_default();
        state.wallpaper = children.get("wallpaper").cloned().unwrap_or_default();
        state.children = children;
    }

    /// Replaces the bar and wallpaper halves, leaving the other children as they were.
    ///
    /// Only the tests build a state without a supervisor loop; production always publishes every
    /// child at once through [`publish_children`](Self::publish_children).
    #[cfg(test)]
    pub fn publish(&self, shell: ChildState, wallpaper: ChildState) {
        let mut state = self.lock_state();
        state.shell = shell.clone();
        state.wallpaper = wallpaper.clone();
        state.children.insert("shell".to_owned(), shell);
        state.children.insert("wallpaper".to_owned(), wallpaper);
    }

    /// The published state of the child called `name`.
    pub fn child_state(&self, name: &str) -> ChildState {
        self.lock_state()
            .children
            .get(name)
            .cloned()
            .unwrap_or_default()
    }

    /// The current state, with the uptime filled in.
    pub fn state(&self) -> StateDoc {
        let mut state = self.lock_state().clone();
        state.uptime_s = self.started.elapsed().as_secs();
        state
    }

    /// Queues a restart of `name`.
    pub fn request_restart(&self, name: &str) {
        self.lock_restarts().push(name.to_owned());
    }

    /// Takes every queued restart request.
    pub fn take_restarts(&self) -> Vec<String> {
        std::mem::take(&mut self.lock_restarts())
    }

    /// Queues a restart of `name` and waits until the loop has applied it, or `timeout` passes.
    ///
    /// Returns whether the loop applied it. The wait is on a counter the loop bumps after it has
    /// stopped the old process and tried to start the new one, so a caller never mistakes the
    /// pre-restart state for the result.
    pub fn restart_and_wait(&self, name: &str, timeout: Duration) -> bool {
        let seen = self.restarts_applied(name);
        self.request_restart(name);

        let deadline = Instant::now() + timeout;
        let mut applied = self.lock_restarts_applied();

        loop {
            if applied.get(name).copied().unwrap_or(0) > seen {
                return true;
            }

            let now = Instant::now();
            if now >= deadline {
                return false;
            }

            let (guard, _) = self
                .restart_signal
                .wait_timeout(applied, deadline - now)
                .unwrap_or_else(|poison| poison.into_inner());
            applied = guard;
        }
    }

    /// Records that the loop has applied a restart of `name`, waking any waiter.
    pub fn mark_restarted(&self, name: &str) {
        *self
            .lock_restarts_applied()
            .entry(name.to_owned())
            .or_insert(0) += 1;
        self.restart_signal.notify_all();
    }

    /// How many restarts of `name` the loop has applied so far.
    fn restarts_applied(&self, name: &str) -> u64 {
        self.lock_restarts_applied().get(name).copied().unwrap_or(0)
    }

    /// Asks the loop to stop.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Queues an `app.openSettings`: the loop launches or focuses the window on its next tick.
    pub fn request_open_settings(&self) {
        self.open_settings.store(true, Ordering::SeqCst);
    }

    /// Takes the queued settings request, if any.
    pub fn take_open_settings(&self) -> bool {
        self.open_settings.swap(false, Ordering::SeqCst)
    }

    /// Whether a stop was asked for.
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Whether `name` is one of the children this supervisor knows.
    fn knows(&self, name: &str) -> bool {
        self.names.iter().any(|known| known == name)
    }

    /// The document `windows.getState` answers with.
    pub fn windows_state(&self) -> Value {
        self.lock_windows().state_doc()
    }

    /// Applies one `windows.set` request and answers with the new state.
    pub fn windows_set(&self, params: Option<&Value>) -> Result<Value, String> {
        let update =
            neb_core::windows_mode::Update::parse(params).map_err(|error| error.to_string())?;
        let mut mode = self.lock_windows();
        mode.apply(&update)?;

        Ok(mode.state_doc())
    }

    /// Restores everything the mode changed; `true` when there was something to restore.
    pub fn restore_windows(&self) -> Result<bool, String> {
        let mut mode = self.lock_windows();
        let applied = mode.has_applied();
        mode.restore_all()?;

        Ok(applied)
    }

    /// Whether any Windows chrome is currently hidden, which the tests read.
    #[cfg(test)]
    pub fn windows_applied(&self) -> bool {
        self.lock_windows().has_applied()
    }

    /// The Windows replacement mode lock, poisoned or not.
    fn lock_windows(&self) -> std::sync::MutexGuard<'_, Mode> {
        self.windows
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The state lock, poisoned or not: a panic in a handler must not stop the supervisor.
    fn lock_state(&self) -> std::sync::MutexGuard<'_, StateDoc> {
        self.state
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The restart queue lock, poisoned or not.
    fn lock_restarts(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        self.restarts
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The applied-restart counter lock, poisoned or not.
    fn lock_restarts_applied(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, u64>> {
        self.restarts_applied
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

/// Answers one IPC request.
pub fn dispatch(shared: &Shared, request: Request) -> Response {
    let id = request.id.clone();

    match request.method.as_str() {
        "supervisor.ping" => Response::success(id, json!("pong")),
        "supervisor.getState" => Response::success(id, json!(shared.state())),
        "supervisor.restart" => match restarted_child(shared, &request) {
            Some(name) => {
                shared.request_restart(name);
                Response::success(id, json!({ "restarting": name }))
            }
            None => Response::failure(
                id,
                format!(
                    "'supervisor.restart' needs one of: {}",
                    shared.names.join(", ")
                ),
            ),
        },
        "supervisor.stop" => {
            shared.request_stop();
            Response::success(id, json!({ "stopping": true }))
        }
        // The window focuses itself when it is already open, so the answer is the same either way.
        "app.openSettings" => {
            shared.request_open_settings();
            Response::success(id, json!({ "ok": true }))
        }
        // Stops the named child and starts it again; the answer is due once it runs, or after
        // [`RESTART_BUDGET`] whichever comes first, so a dashboard button never hangs on it.
        "app.restartChild" => match restarted_child(shared, &request) {
            Some(name) => {
                let name = name.to_owned();
                let applied = shared.restart_and_wait(&name, RESTART_BUDGET);
                let state = shared.child_state(&name);

                Response::success(
                    id,
                    json!({
                        "name": name,
                        "running": state.running,
                        "pid": state.pid,
                        "applied": applied,
                    }),
                )
            }
            None => Response::failure(
                id,
                format!(
                    "'app.restartChild' needs one of: {}",
                    shared.names.join(", ")
                ),
            ),
        },
        "app.quit" => {
            shared.request_stop();
            Response::success(id, json!({ "ok": true }))
        }
        "windows.getState" => Response::success(id, shared.windows_state()),
        "windows.set" => match shared.windows_set(request.params.as_ref()) {
            Ok(state) => Response::success(id, state),
            Err(reason) => Response::failure(id, format!("'windows.set' failed: {reason}")),
        },
        "windows.restore" => match shared.restore_windows() {
            Ok(_) => Response::success(id, shared.windows_state()),
            Err(reason) => Response::failure(id, format!("'windows.restore' failed: {reason}")),
        },
        other => Response::failure(id, format!("unknown method '{other}'")),
    }
}

/// The child name in a `supervisor.restart` request: `{"child": "shell"}` or a bare string.
fn restarted_child<'a>(shared: &Shared, request: &'a Request) -> Option<&'a str> {
    let params = request.params.as_ref()?;

    let name = match params {
        Value::String(name) => name.as_str(),
        Value::Object(object) => ["child", "name"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))?,
        _ => return None,
    };

    let name = name.trim();
    shared.knows(name).then_some(name)
}

/// Writes every transition to the log, and to stdout when the run is a diagnostic one.
#[derive(Debug, Clone)]
pub struct Reporter {
    log: Option<LogHandle>,
    echo: bool,
}

impl Reporter {
    /// Opens the daily log under the ProjectNeb data directory; a machine without one still runs.
    pub fn new(echo: bool) -> Self {
        let log = neb_core::paths::try_root()
            .ok()
            .and_then(|root| neb_core::logging::init(&root.join("logs")).ok());

        Self { log, echo }
    }

    /// Writes one line at `level`.
    pub fn line(&self, level: Level, message: &str) {
        if let Some(log) = &self.log {
            log.log(level, MODULE, message);
        }

        if self.echo {
            println!(
                "{} {:<5} {}",
                UtcDateTime::now_utc().iso8601(),
                level.as_str(),
                message
            );
        }
    }

    /// Writes a [`Level::Info`] line.
    pub fn info(&self, message: &str) {
        self.line(Level::Info, message);
    }

    /// Writes a [`Level::Warn`] line.
    pub fn warn(&self, message: &str) {
        self.line(Level::Warn, message);
    }

    /// Writes a [`Level::Error`] line.
    pub fn error(&self, message: &str) {
        self.line(Level::Error, message);
    }
}

/// The running supervisor.
#[derive(Debug)]
pub struct Supervisor {
    reporter: Reporter,
    shared: Arc<Shared>,
    server: Option<ServerHandle>,
    children: Vec<Child>,
    started: Instant,
    /// Data root, used by the tray and the updater when it exists.
    root: PathBuf,
    /// Notification icon, absent when the command line or Windows refuses one.
    tray: Option<Tray>,
    /// Update checker worker, absent when updates are disabled.
    updates: Option<UpdatesHandle>,
    /// The thread showing the release notes, while the dialog is open.
    notes: Option<std::thread::JoinHandle<()>>,
    /// Held for the whole run: dropping it is what lets the next supervisor start.
    _instance: NamedMutex,
}

/// Renders the release notes and reports whether a dialog appeared.
///
/// The startup notes run through this on a worker thread, so a dialog that waits for the user
/// cannot stop the loop that supervises children. Tests inject a closure that blocks on a channel
/// until the test says the "dialog" is closed.
pub type NotesDialog = Box<dyn FnOnce(&WhatsNew) -> bool + Send + 'static>;

/// The production notes dialog: the same task dialog the tray menu opens.
fn notes_dialog(notes: &WhatsNew) -> bool {
    crate::tray::show_whats_new_dialog(None, notes)
}

impl Supervisor {
    /// Starts the supervisor and everything the configuration asks for.
    ///
    /// `Ok(None)` means another supervisor already holds [`INSTANCE_MUTEX`]: the caller exits
    /// successfully, because a second supervisor is not an error, it is a no-op.
    ///
    /// # Errors
    ///
    /// [`SupervisorError::Ipc`] when the pipe cannot be served, [`SupervisorError::Mutex`] when the
    /// instance mutex cannot be created at all.
    pub fn start(options: Options) -> Result<Option<Self>, SupervisorError> {
        let instance_name = if options.instance.trim().is_empty() {
            INSTANCE_MUTEX
        } else {
            options.instance.as_str()
        };
        let (instance, first) = NamedMutex::acquire(instance_name)?;
        let reporter = Reporter::new(options.echo);

        if !first {
            reporter.info("another supervisor is already running, leaving the children to it");
            return Ok(None);
        }

        let specs = specs(&options);
        let names: Vec<String> = specs.iter().map(|spec| spec.name.clone()).collect();
        let root = neb_core::paths::try_root().unwrap_or_default();

        // Repair before anything starts: a state file left behind by an off mode is put back now.
        let mut windows = if root.as_os_str().is_empty() {
            reporter.warn("windows-mode: no ProjectNeb data directory, the mode is disabled");
            Mode::detached()
        } else {
            Mode::open(&root, reporter.clone())
        };
        let repair_notes = windows.repair();
        let shared = Arc::new(Shared::with_mode(names, windows));

        for note in repair_notes {
            reporter.info(&format!("windows-mode: {note}"));
        }

        let mut supervisor = Self {
            reporter,
            shared,
            server: None,
            children: Vec::new(),
            started: Instant::now(),
            root,
            tray: None,
            updates: None,
            notes: None,
            _instance: instance,
        };

        supervisor.repair_taskbar(&specs);

        for spec in specs {
            let child = Child::new(spec);
            if let Some(reason) = child.blocked() {
                supervisor.reporter.warn(&format!(
                    "{}: {reason}, not starting a second copy",
                    child.name()
                ));
            } else if child.adopted() {
                let pid = child.pid().unwrap_or_default();
                supervisor.reporter.info(&format!(
                    "{}: adopted the running '{}' (pid {pid})",
                    child.name(),
                    child.exe().display()
                ));
                supervisor.watch_adopted(child.name().to_owned(), pid);
            }
            supervisor.children.push(child);
        }

        for index in 0..supervisor.children.len() {
            if supervisor.children[index].enabled()
                && supervisor.children[index].blocked().is_none()
                && !supervisor.children[index].adopted()
            {
                supervisor.start_child(index);
            }
        }

        let pipe = if options.pipe.trim().is_empty() {
            PIPE.to_owned()
        } else {
            options.pipe.clone()
        };
        let server = {
            let shared = Arc::clone(&supervisor.shared);
            ipc::serve(&pipe, move |request| dispatch(&shared, request))
        }
        .map_err(|source| SupervisorError::Ipc {
            pipe: pipe.clone(),
            source,
        })?;
        supervisor.server = Some(server);

        supervisor.reporter.info(&format!(
            "supervisor started (pid {}, ipc '{pipe}', children: {})",
            std::process::id(),
            supervisor.child_summary()
        ));

        supervisor.publish();
        supervisor.start_tray_and_updates(&options);
        supervisor.show_whats_new();

        Ok(Some(supervisor))
    }

    /// Whether a stop was requested over IPC.
    pub fn stopping(&self) -> bool {
        self.shared.stopping()
    }

    /// Opens the main window after the children are up.
    pub fn open_settings(&self) {
        settings::launch(&self.reporter);
    }

    /// Reaps dead children, starts the ones that are due and applies queued commands.
    pub fn tick(&mut self) {
        self.poll_whats_new();

        if let Some(tray) = &self.tray {
            tray.pump();
        }

        if self.shared.take_open_settings() {
            settings::launch(&self.reporter);
        }

        let now = Instant::now();

        for name in self.shared.take_restarts() {
            self.restart_child(&name);
        }

        for index in 0..self.children.len() {
            let name = self.children[index].name().to_owned();

            if let Some(exit) = self.children[index].poll() {
                self.reporter.warn(&format!(
                    "{name}: pid {} exited with code {} after {} s",
                    exit.pid,
                    exit.code,
                    seconds(exit.ran_for)
                ));

                if self.children[index].enabled() && self.children[index].blocked().is_none() {
                    let delay = self.children[index].schedule_restart(exit.ran_for);
                    self.reporter.info(&format!(
                        "{name}: restart in {} s (backoff {} s, short crashes {})",
                        seconds(delay.delay),
                        seconds(delay.step),
                        self.children[index].short_crashes()
                    ));
                }
            }

            if self.children[index].due(now) {
                self.start_child(index);
            }
        }

        self.publish();
    }

    /// Ticks until `duration` has passed, or until a stop is requested.
    pub fn pump(&mut self, duration: Duration) {
        let deadline = Instant::now() + duration;

        loop {
            self.tick();

            if self.stopping() {
                return;
            }

            let now = Instant::now();
            if now >= deadline {
                return;
            }

            std::thread::sleep(TICK.min(deadline - now));
        }
    }

    /// Ticks until a stop is requested, which is what the resident mode does.
    pub fn run(&mut self) {
        while !self.stopping() {
            self.tick();
            std::thread::sleep(TICK);
        }
    }

    /// Starts the tray icon and the update worker, each only when the run asks for it.
    fn start_tray_and_updates(&mut self, options: &Options) {
        let shared = Arc::clone(&self.shared);
        let stop: update::StopFn = Arc::new(move || shared.request_stop());
        let directory = exe_dir();

        if options.tray {
            let reporter = self.reporter.clone();
            let open_settings: crate::tray::OpenFn = Arc::new(move || settings::open(&reporter));

            match Tray::new(
                &directory,
                self.root.clone(),
                self.reporter.clone(),
                Arc::clone(&stop),
                open_settings,
            ) {
                Ok(mut tray) => {
                    let shared = Arc::clone(&self.shared);
                    let restore: crate::tray::RestoreFn =
                        Arc::new(move || shared.restore_windows());
                    tray.set_restore(restore);
                    self.tray = Some(tray);
                }
                Err(error) => self.reporter.warn(&format!("tray: {error}")),
            }
        } else {
            self.reporter.info("tray: disabled by the command line");
        }

        if !options.updates {
            self.reporter.info("updates: disabled by the command line");
            return;
        }

        if self.root.as_os_str().is_empty() {
            self.reporter
                .warn("updates: no ProjectNeb data directory, checks disabled");
            return;
        }

        let settings = update::Settings::from_file(&self.root.join("config.json"));
        if !settings.enabled {
            self.reporter.info("updates: disabled by settings");
            return;
        }

        let notify: update::Notify = match &self.tray {
            Some(tray) => {
                let notifier = tray.notifier();
                Arc::new(move |title: &str, body: &str| {
                    let _ = notifier.balloon(title, body);
                })
            }
            None => Arc::new(|_title: &str, _body: &str| {}),
        };

        let handle = update::start(
            settings.clone(),
            self.root.clone(),
            directory,
            self.reporter.clone(),
            notify,
            stop,
        );

        if let Some(tray) = self.tray.as_mut() {
            tray.set_updates(Some(handle.sender()));
        }
        self.updates = Some(handle);
        self.reporter.info(&format!(
            "updates: worker started (first check in {} s, then every {} h)",
            update::FIRST_CHECK_DELAY.as_secs(),
            settings.check_hours
        ));
    }

    /// Shows the notes of the version that is running, once, without blocking the loop.
    fn show_whats_new(&mut self) {
        self.show_whats_new_with(Box::new(notes_dialog));
    }

    /// Starts the notes dialog on its own thread with `dialog` as the renderer.
    ///
    /// Everything the thread needs is owned: the notes, the data root and a reporter. The shown
    /// marker is written by that thread after the dialog returns, which is what "shown" means.
    fn show_whats_new_with(&mut self, dialog: NotesDialog) {
        let Some(notes) = update::read_whats_new(&self.root) else {
            return;
        };

        if notes.shown || notes.version != env!("CARGO_PKG_VERSION") {
            return;
        }

        let root = self.root.clone();
        let reporter = self.reporter.clone();
        self.notes = Some(std::thread::spawn(move || {
            if !dialog(&notes) {
                return;
            }

            match update::mark_shown(&root) {
                Ok(()) => {
                    reporter.info(&format!("whats-new: showed the notes of {}", notes.version))
                }
                Err(error) => reporter.warn(&format!(
                    "whats-new: cannot mark the notes as shown: {error}"
                )),
            }
        }));
    }

    /// Reaps the notes thread once its dialog has closed.
    ///
    /// A dialog that is still open is left alone: joining it here would put the block back into the
    /// loop, which is the defect this split exists to remove.
    fn poll_whats_new(&mut self) {
        if self
            .notes
            .as_ref()
            .is_some_and(|handle| handle.is_finished())
        {
            if let Some(handle) = self.notes.take() {
                let _ = handle.join();
            }
        }
    }

    /// Stops every child and closes the pipe.
    pub fn shutdown(&mut self) {
        self.poll_whats_new();

        // The window is not a supervised child; leaving it behind would keep a WebView2 tree
        // alive after "Exit" and block an updater from replacing `neb-app.exe`.
        crate::settings::close_window();

        if let Some(mut updates) = self.updates.take() {
            updates.stop();
            self.reporter.info("updates: worker stopped");
        }

        for index in 0..self.children.len() {
            let name = self.children[index].name().to_owned();
            if let Some(pid) = self.children[index].stop() {
                self.reporter.info(&format!("{name}: pid {pid} stopped"));
            }
        }

        self.publish();

        if let Some(server) = self.server.take() {
            let pipe = server.pipe_name().to_owned();
            server.shutdown();
            self.reporter.info(&format!("ipc '{pipe}' closed"));
        }

        if self.tray.take().is_some() {
            self.reporter.info("tray: closed");
        }

        self.reporter.info(&format!(
            "supervisor stopped after {} s",
            seconds(self.started.elapsed())
        ));
    }

    /// Starts one child and logs the outcome; a failure is scheduled like a crash, not ignored.
    fn start_child(&mut self, index: usize) {
        let name = self.children[index].name().to_owned();
        let exe = self.children[index].exe().to_path_buf();

        match self.children[index].start() {
            Ok(pid) => self
                .reporter
                .info(&format!("{name}: started '{}' (pid {pid})", exe.display())),
            Err(error) => {
                self.reporter.error(&format!("{name}: {error}"));
                let delay = self.children[index].schedule_restart(Duration::ZERO);
                self.reporter
                    .warn(&format!("{name}: retrying in {} s", seconds(delay.delay)));
            }
        }
    }

    /// Parks a thread on the handle of an adopted process so its exit is noticed even though this
    /// supervisor never created it.
    ///
    /// The thread does not restart anything itself: the loop polls every child, reaps the exit and
    /// hands it to the restart planner, so an adopted child follows the same throttle and backoff
    /// as a spawned one. The waiter only ends the wait promptly and says so in the log.
    fn watch_adopted(&self, name: String, pid: u32) {
        let reporter = self.reporter.clone();

        std::thread::spawn(move || {
            crate::children::wait_for_exit(pid);
            reporter.info(&format!("{name}: adopted pid {pid} exited"));
        });
    }

    /// Stops a child and starts it again; a manual restart is not throttled.
    fn restart_child(&mut self, name: &str) {
        let Some(index) = self.children.iter().position(|child| child.name() == name) else {
            self.reporter
                .warn(&format!("restart: no child called '{name}'"));
            return;
        };

        if let Some(pid) = self.children[index].stop() {
            self.reporter
                .info(&format!("{name}: pid {pid} stopped for a manual restart"));
        }

        self.children[index].reset_backoff();
        self.start_child(index);

        // Publish before the bump: the waiter in `app.restartChild` reads this document as soon as
        // it is woken, and a state from before the restart would name the process that just died.
        self.publish();
        self.shared.mark_restarted(name);
    }

    /// Publishes what the IPC handlers answer from.
    fn publish(&self) {
        let children = self
            .children
            .iter()
            .map(|child| {
                (
                    child.name().to_owned(),
                    ChildState {
                        pid: child.pid(),
                        running: child.running(),
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        self.shared.publish_children(children);
    }

    /// Puts the taskbar back before anything starts, when a crashed shell left it hidden.
    fn repair_taskbar(&self, specs: &[ChildSpec]) {
        let Some(path) = taskbar_repair::state_path() else {
            self.reporter
                .warn("taskbar: no ProjectNeb data directory, repair skipped");
            return;
        };

        let shell_enabled = specs
            .iter()
            .any(|spec| spec.name == "shell" && spec.enabled);

        if !shell_enabled {
            return;
        }

        let outcome = taskbar_repair::repair_if_orphaned(&path, shell_answers());

        match outcome {
            Outcome::Restored => self
                .reporter
                .warn("taskbar: a shell left the taskbar hidden, the auto-hide state was restored"),
            Outcome::Failed(reason) => {
                self.reporter
                    .error(&format!("taskbar: the repair failed ({reason})"));
            }
            Outcome::ShellRunning => self
                .reporter
                .info("taskbar: a shell is running and owns the taskbar state"),
            Outcome::NotHidden => self
                .reporter
                .info("taskbar: a leftover snapshot for a shown taskbar was dropped"),
            Outcome::NoSnapshot => {}
        }
    }

    /// `name (pid N)` per running child, for the startup line.
    fn child_summary(&self) -> String {
        self.children
            .iter()
            .map(|child| match child.pid() {
                Some(pid) if child.running() => format!("{} (pid {pid})", child.name()),
                _ if child.enabled() => format!("{} (stopped)", child.name()),
                _ => format!("{} (disabled)", child.name()),
            })
            .collect::<Vec<_>>()
            .join(", ")
    }
}

/// Whether a shell answers on [`SHELL_PIPE`]; a busy or missing shell reads as "no".
fn shell_answers() -> bool {
    ipc::call(SHELL_PIPE, "shell.ping", None, SHELL_PROBE).is_ok()
}

/// The children a run supervises.
fn specs(options: &Options) -> Vec<ChildSpec> {
    if let Some((exe, args)) = &options.command {
        return vec![ChildSpec::command(exe.clone(), args.clone())];
    }

    let config_path = options
        .config
        .clone()
        .unwrap_or_else(neb_core::paths::config_file);
    let enabled = Enabled::from_file(&config_path);

    let shell = options.shell.clone().unwrap_or_else(|| sibling(SHELL_EXE));
    let wallpaper = options
        .wallpaper
        .clone()
        .unwrap_or_else(|| sibling(WALLPAPER_EXE));
    let discord = sibling(DISCORD_EXE);
    let wm = sibling(WM_EXE);
    let zapret = sibling(ZAPRET_EXE);
    let windhawk = sibling(WINDHAWK_EXE);

    vec![
        ChildSpec::new("shell", shell, enabled.shell).adopting(),
        ChildSpec::new("wallpaper", wallpaper, enabled.wallpaper).adopting(),
        ChildSpec::new("discord", discord, enabled.discord).adopting(),
        ChildSpec::new("wm", wm, enabled.wm).adopting(),
        ChildSpec::new("zapret", zapret, enabled.zapret).adopting(),
        ChildSpec::new("windhawk", windhawk, enabled.windhawk).adopting(),
    ]
}

/// `name` next to the running executable, which is where `cargo build` puts every binary.
fn sibling(name: &str) -> PathBuf {
    exe_dir().join(name)
}

/// Directory of the running executable; the portable folder, in a real installation.
fn exe_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .unwrap_or_default()
}

/// Seconds with one decimal, for log lines.
fn seconds(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;
    use neb_core::windows_mode::{MemoryRegistry, RawValue, Session, Toggle, WindowsModeSettings};
    use std::sync::Arc;

    fn shared() -> Shared {
        Shared::new(vec!["shell".to_owned(), "wallpaper".to_owned()])
    }

    /// A supervisor with no children, no pipe and no tray, rooted at `root`.
    ///
    /// The loop's own fields are private but this module is their parent, so a test can build the
    /// value without starting the resident services.
    fn idle_supervisor(root: PathBuf) -> Supervisor {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let serial = NEXT.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let name = format!(
            r"Local\ProjectNeb.supervisor.idle-test.{}.{serial}",
            std::process::id()
        );
        let (instance, first) = NamedMutex::acquire(&name).expect("the test mutex");
        assert!(first);

        Supervisor {
            reporter: Reporter {
                log: None,
                echo: false,
            },
            shared: Arc::new(Shared::new(Vec::new())),
            server: None,
            children: Vec::new(),
            started: Instant::now(),
            root,
            tray: None,
            updates: None,
            notes: None,
            _instance: instance,
        }
    }

    fn request(method: &str, params: Option<Value>) -> Request {
        Request {
            id: "1".to_owned(),
            method: method.to_owned(),
            params,
        }
    }

    #[test]
    fn ping_answers_pong() {
        let response = dispatch(&shared(), request("supervisor.ping", None));

        assert!(response.ok);
        assert_eq!(response.result, Some(json!("pong")));
    }

    #[test]
    fn the_state_document_has_the_shape_the_contract_promises() {
        let shared = shared();
        shared.publish(
            ChildState {
                pid: Some(42),
                running: true,
            },
            ChildState::default(),
        );

        let response = dispatch(&shared, request("supervisor.getState", None));
        let value = response.result.expect("a state document");

        assert_eq!(value["shell"]["pid"], json!(42));
        assert_eq!(value["shell"]["running"], json!(true));
        assert_eq!(value["wallpaper"]["pid"], Value::Null);
        assert_eq!(value["wallpaper"]["running"], json!(false));
        assert_eq!(value["pid"], json!(std::process::id()));
        assert_eq!(value["children"]["shell"]["pid"], json!(42));
        assert_eq!(value["children"]["wallpaper"]["running"], json!(false));
        assert!(value["uptimeS"].as_u64().is_some(), "{value}");
        assert!(
            value.get("uptime_s").is_none(),
            "the wire name is camelCase"
        );
    }

    #[test]
    fn a_restart_of_a_known_child_is_queued() {
        let shared = shared();

        let response = dispatch(
            &shared,
            request("supervisor.restart", Some(json!({ "child": "shell" }))),
        );

        assert!(response.ok, "{response:?}");
        assert_eq!(shared.take_restarts(), vec!["shell".to_owned()]);
    }

    #[test]
    fn a_restart_accepts_a_bare_name_too() {
        let shared = shared();

        assert!(
            dispatch(
                &shared,
                request("supervisor.restart", Some(json!("wallpaper")))
            )
            .ok
        );

        assert_eq!(shared.take_restarts(), vec!["wallpaper".to_owned()]);
    }

    #[test]
    fn a_restart_of_an_unknown_child_is_refused_with_the_list() {
        let shared = shared();

        let response = dispatch(
            &shared,
            request("supervisor.restart", Some(json!({ "child": "nope" }))),
        );

        assert!(!response.ok);
        let message = response.error.expect("an explanation");
        assert!(message.contains("shell"), "{message}");
        assert!(message.contains("wallpaper"), "{message}");
        assert!(shared.take_restarts().is_empty());
    }

    #[test]
    fn a_restart_without_a_name_is_refused() {
        let shared = shared();

        for params in [None, Some(json!(7)), Some(json!({}))] {
            let response = dispatch(&shared, request("supervisor.restart", params.clone()));
            assert!(!response.ok, "params {params:?} must be refused");
        }
    }

    #[test]
    fn app_restart_child_of_an_unknown_child_is_refused_with_the_list() {
        let shared = shared();

        let response = dispatch(
            &shared,
            request("app.restartChild", Some(json!({ "name": "nope" }))),
        );

        assert!(!response.ok, "{response:?}");
        let message = response.error.expect("an explanation");
        assert!(message.contains("shell"), "{message}");
        assert!(message.contains("wallpaper"), "{message}");
        assert!(
            shared.take_restarts().is_empty(),
            "a refused restart must not be queued"
        );
    }

    #[test]
    fn app_restart_child_answers_running_once_the_loop_applied_it() {
        let shared = Arc::new(shared());
        let loop_side = Arc::clone(&shared);

        let stub = std::thread::spawn(move || {
            for _ in 0..600 {
                if loop_side.take_restarts().iter().any(|name| name == "shell") {
                    let mut children = BTreeMap::new();
                    children.insert(
                        "shell".to_owned(),
                        ChildState {
                            pid: Some(7001),
                            running: true,
                        },
                    );
                    loop_side.publish_children(children);
                    loop_side.mark_restarted("shell");
                    return;
                }

                std::thread::sleep(Duration::from_millis(5));
            }
        });

        let started = Instant::now();
        let response = dispatch(
            &shared,
            request("app.restartChild", Some(json!({ "name": "shell" }))),
        );
        let elapsed = started.elapsed();
        stub.join().expect("the stub loop must not panic");

        assert!(response.ok, "{response:?}");
        let value = response.result.expect("a restart document");
        assert_eq!(value["name"], json!("shell"));
        assert_eq!(value["running"], json!(true));
        assert_eq!(value["pid"], json!(7001));
        assert_eq!(value["applied"], json!(true));
        assert!(
            elapsed < RESTART_BUDGET,
            "the answer is due as soon as the child runs, not after the whole budget: {elapsed:?}"
        );
    }

    #[test]
    fn a_restart_the_loop_never_applies_gives_up_at_its_deadline() {
        let shared = shared();
        let started = Instant::now();

        let applied = shared.restart_and_wait("shell", Duration::from_millis(60));

        assert!(!applied);
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the wait must honour the timeout it was given"
        );
        assert_eq!(shared.take_restarts(), vec!["shell".to_owned()]);
    }

    #[test]
    fn stop_sets_the_flag_and_answers() {
        let shared = shared();
        assert!(!shared.stopping());

        let response = dispatch(&shared, request("supervisor.stop", None));

        assert!(response.ok);
        assert_eq!(response.result, Some(json!({ "stopping": true })));
        assert!(shared.stopping(), "the loop must see the request");
    }

    #[test]
    fn app_open_settings_queues_the_spawn_and_answers_ok() {
        let shared = shared();

        let response = dispatch(&shared, request("app.openSettings", None));

        assert!(response.ok, "{response:?}");
        assert_eq!(response.result, Some(json!({ "ok": true })));
        assert!(shared.take_open_settings(), "the loop must see the request");
        assert!(
            !shared.take_open_settings(),
            "one request opens the window once"
        );
    }

    #[test]
    fn app_quit_stops_the_children_and_answers_ok() {
        let shared = shared();
        assert!(!shared.stopping());

        let response = dispatch(&shared, request("app.quit", None));

        assert!(response.ok, "{response:?}");
        assert_eq!(response.result, Some(json!({ "ok": true })));
        assert!(shared.stopping(), "the loop must stop");
    }

    #[test]
    fn an_unknown_method_is_refused_by_name() {
        let shared = shared();

        let response = dispatch(&shared, request("supervisor.explode", None));

        assert!(!response.ok);
        assert!(
            response
                .error
                .is_some_and(|message| message.contains("supervisor.explode"))
        );
    }

    #[test]
    fn the_default_children_are_the_shell_next_to_this_binary_and_the_wallpaper() {
        // A hermetic config: the machine's real one may have modules enabled by now.
        let dir = std::env::temp_dir().join(format!("neb-specs-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join("config.json");
        std::fs::write(&config, "{}").unwrap();
        let specs = specs(&Options {
            config: Some(config),
            ..Options::default()
        });

        assert_eq!(specs.len(), 6);
        assert_eq!(specs[0].name, "shell");
        assert!(specs[0].exe.ends_with(SHELL_EXE));
        assert_eq!(specs[1].name, "wallpaper");
        assert!(specs[1].exe.ends_with(WALLPAPER_EXE));
        assert_eq!(specs[2].name, "discord");
        assert!(specs[2].exe.ends_with(DISCORD_EXE));
        assert_eq!(specs[3].name, "wm");
        assert!(specs[3].exe.ends_with(WM_EXE));
        assert_eq!(specs[4].name, "zapret");
        assert!(specs[4].exe.ends_with(ZAPRET_EXE));
        assert_eq!(specs[5].name, "windhawk");
        assert!(specs[5].exe.ends_with(WINDHAWK_EXE));
        assert!(!specs[5].enabled, "Windhawk is opt-in");
        assert!(
            specs[0].enabled,
            "the shipped config has no shell section, so the shell runs"
        );
        assert!(
            !specs[2].enabled,
            "the shipped config leaves Discord off until an app id exists"
        );
        assert!(
            specs.iter().all(|spec| spec.adopt),
            "the real children are adopted when an earlier supervisor left them running"
        );
    }

    #[test]
    fn a_spawn_test_command_replaces_the_children() {
        let options = Options {
            command: Some((PathBuf::from("cmd.exe"), vec!["/c".to_owned()])),
            ..Options::default()
        };

        let specs = specs(&options);

        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "test");
        assert_eq!(specs[0].exe, PathBuf::from("cmd.exe"));
        assert!(specs[0].enabled);
        assert!(
            !specs[0].adopt,
            "a diagnostic command must not take over a shared executable"
        );
    }

    #[test]
    fn the_state_of_an_unknown_child_is_stopped() {
        let supervisor_state = ChildState::default();

        assert_eq!(supervisor_state.pid, None);
        assert!(!supervisor_state.running);
    }

    #[test]
    fn the_ipc_surface_answers_over_a_real_pipe() {
        let tag = std::process::id();
        let pipe = format!("ProjectNeb.supervisor.test.{tag}");
        let options = Options {
            command: Some((
                PathBuf::from("cmd.exe"),
                vec!["/c".to_owned(), "timeout".to_owned(), "30".to_owned()],
            )),
            echo: false,
            tray: false,
            updates: false,
            pipe: pipe.clone(),
            instance: format!(r"Local\ProjectNeb.supervisor.test.{tag}"),
            ..Options::default()
        };

        let mut supervisor = Supervisor::start(options)
            .expect("the supervisor starts")
            .expect("the test owns its own instance mutex");

        let budget = Duration::from_secs(5);

        assert_eq!(
            ipc::call(&pipe, "supervisor.ping", None, budget).expect("ping"),
            json!("pong")
        );

        let state = ipc::call(&pipe, "supervisor.getState", None, budget).expect("state");
        assert_eq!(state["shell"]["running"], json!(false));
        assert!(state["uptimeS"].as_u64().is_some(), "{state}");

        let restarted = ipc::call(
            &pipe,
            "supervisor.restart",
            Some(json!({ "child": "test" })),
            budget,
        )
        .expect("restart");
        assert_eq!(restarted["restarting"], json!("test"));

        // The loop applies a queued command on its next tick, which is what the resident mode does.
        supervisor.pump(Duration::from_millis(400));

        ipc::call(&pipe, "supervisor.stop", None, budget).expect("stop");
        supervisor.pump(Duration::from_millis(100));
        assert!(supervisor.stopping(), "the loop must see the stop");

        supervisor.shutdown();
        assert!(
            ipc::call(&pipe, "supervisor.ping", None, Duration::from_millis(500)).is_err(),
            "the pipe is gone after a shutdown"
        );
    }

    /// A shared supervisor with a windows mode over an in-memory registry and a temp config.
    fn shared_with_mode() -> (tempfile::TempDir, Arc<MemoryRegistry>, Shared) {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry = Arc::new(MemoryRegistry::default());
        let session = Session::new(
            Arc::clone(&registry),
            directory.path().join(neb_core::windows_mode::STATE_FILE),
        );
        let mode = Mode::with_parts(
            session,
            WindowsModeSettings::default(),
            Some(directory.path().join("config.json")),
        );

        let shared = Shared::with_mode(vec!["shell".to_owned(), "wallpaper".to_owned()], mode);

        (directory, registry, shared)
    }

    #[test]
    fn windows_get_state_starts_off_with_nothing_applied() {
        let (_directory, _registry, shared) = shared_with_mode();

        let response = dispatch(&shared, request("windows.getState", None));
        let state = response.result.expect("a state document");

        assert!(response.ok);
        assert_eq!(state["enabled"], json!(false));
        for toggle in Toggle::ALL {
            assert_eq!(state["toggles"][toggle.id()], json!(false));
            assert_eq!(state["applied"][toggle.id()], json!(false));
        }
    }

    #[test]
    fn windows_set_applies_and_partial_updates_merge() {
        let (directory, registry, shared) = shared_with_mode();

        let first = dispatch(
            &shared,
            request(
                "windows.set",
                Some(json!({ "toggles": { "desktopIcons": true } })),
            ),
        );
        assert!(first.ok, "{first:?}");

        let second = dispatch(
            &shared,
            request(
                "windows.set",
                Some(json!({ "toggles": { "searchBox": true } })),
            ),
        );
        assert!(second.ok, "{second:?}");
        assert_eq!(
            second.result.as_ref().expect("state")["applied"]["desktopIcons"],
            json!(true),
            "the first toggle is still applied by a partial update"
        );

        let state = dispatch(&shared, request("windows.getState", None))
            .result
            .expect("state");
        assert_eq!(state["enabled"], json!(true));
        assert_eq!(state["toggles"]["desktopIcons"], json!(true));
        assert_eq!(state["toggles"]["searchBox"], json!(true));
        assert_eq!(state["applied"]["desktopIcons"], json!(true));
        assert_eq!(state["applied"]["searchBox"], json!(true));
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 1 }
        );
        assert_eq!(
            registry.get(Toggle::SearchBox),
            RawValue::Dword { value: 0 }
        );

        let config: Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("config.json")).expect("config written"),
        )
        .expect("valid json");
        assert_eq!(config["windowsMode"]["desktopIcons"], json!(true));
        assert_eq!(config["windowsMode"]["searchBox"], json!(true));
    }

    #[test]
    fn windows_set_rejects_an_unknown_key_and_leaves_the_registry_alone() {
        let (_directory, registry, shared) = shared_with_mode();

        for params in [
            json!({ "toggles": { "telemetry": true } }),
            json!({ "telemetry": true }),
            json!({ "enabled": "yes" }),
        ] {
            let response = dispatch(&shared, request("windows.set", Some(params.clone())));
            assert!(!response.ok, "{params} must be refused");
            assert!(
                response
                    .error
                    .as_deref()
                    .is_some_and(|message| message.contains("telemetry")
                        || message.contains("true or false")),
                "{params}: {response:?}"
            );
        }

        assert_eq!(registry.writes(), 0);
        assert!(!shared.windows_applied());
    }

    #[test]
    fn windows_set_without_parameters_changes_nothing() {
        let (_directory, registry, shared) = shared_with_mode();

        let response = dispatch(&shared, request("windows.set", None));

        assert!(response.ok, "{response:?}");
        assert_eq!(registry.writes(), 0);
    }

    #[test]
    fn windows_restore_puts_every_value_back_and_clears_the_selection() {
        let (_directory, registry, shared) = shared_with_mode();
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });

        dispatch(
            &shared,
            request(
                "windows.set",
                Some(json!({ "toggles": { "desktopIcons": true, "taskView": true } })),
            ),
        );

        let response = dispatch(&shared, request("windows.restore", None));
        assert!(response.ok, "{response:?}");
        let state = response.result.expect("state");

        assert_eq!(state["enabled"], json!(false));
        assert_eq!(state["toggles"]["desktopIcons"], json!(false));
        assert_eq!(state["applied"]["desktopIcons"], json!(false));
        assert_eq!(state["applied"]["taskView"], json!(false));
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 0 }
        );
        assert_eq!(
            registry.get(Toggle::TaskView),
            RawValue::Absent,
            "a value that did not exist is deleted again"
        );
        assert!(!shared.windows_applied());
    }

    #[test]
    fn windows_set_with_enabled_false_restores_only_what_was_applied() {
        let (_directory, registry, shared) = shared_with_mode();

        dispatch(
            &shared,
            request(
                "windows.set",
                Some(json!({ "toggles": { "widgets": true, "copilot": true } })),
            ),
        );
        let response = dispatch(
            &shared,
            request("windows.set", Some(json!({ "enabled": false }))),
        );
        assert!(response.ok, "{response:?}");
        let state = response.result.expect("state");

        assert_eq!(state["enabled"], json!(false));
        assert_eq!(
            state["toggles"]["widgets"],
            json!(true),
            "the selection is remembered for the next enable"
        );
        assert_eq!(state["applied"]["widgets"], json!(false));
        assert_eq!(registry.get(Toggle::Widgets), RawValue::Absent);
        assert_eq!(registry.get(Toggle::Copilot), RawValue::Absent);
    }

    #[test]
    fn the_whats_new_dialog_does_not_block_the_tick() {
        let directory = tempfile::tempdir().expect("temp dir");
        let root = directory.path().to_path_buf();
        let version = update::Version::parse(env!("CARGO_PKG_VERSION")).expect("valid version");
        update::write_whats_new(&root, &version, "notes", "").expect("notes written");

        let mut supervisor = idle_supervisor(root.clone());

        // A child that ends on its own, so the tick has real supervision to do while the dialog is
        // "open" — the defect was that the loop stopped reaping during the modal box.
        let mut quick = Child::new(ChildSpec {
            args: vec!["/c".to_owned(), "exit".to_owned(), "3".to_owned()],
            ..ChildSpec::new("quick-notes", PathBuf::from("cmd.exe"), true)
        });
        quick.start().expect("the short-lived child starts");
        std::thread::sleep(Duration::from_millis(300));
        supervisor.children.push(quick);

        let (opened_tx, opened_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        supervisor.show_whats_new_with(Box::new(move |_notes| {
            let _ = opened_tx.send(());
            let _ = release_rx.recv();
            true
        }));

        opened_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the dialog opens on its own thread");

        let started = Instant::now();
        supervisor.tick();
        let elapsed = started.elapsed();

        assert!(
            elapsed < Duration::from_secs(2),
            "a tick must not wait for the dialog: {elapsed:?}"
        );
        assert!(
            supervisor.children[0].pid().is_none(),
            "the tick reaped the child while the dialog was open"
        );
        assert!(
            !update::read_whats_new(&root).expect("notes").shown,
            "the marker is written only after the dialog closes"
        );

        release_tx.send(()).expect("the test closes the dialog");

        let deadline = Instant::now() + Duration::from_secs(5);
        while !update::read_whats_new(&root).expect("notes").shown {
            assert!(
                Instant::now() < deadline,
                "the marker must be written once the dialog closed"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        supervisor.poll_whats_new();
        assert!(
            supervisor.notes.is_none(),
            "the finished dialog thread is reaped"
        );
    }

    #[test]
    fn shutdown_stops_a_child_it_adopted() {
        let directory = tempfile::tempdir().expect("temp dir");
        let source = std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
        let exe = directory.path().join("neb-adopted-shutdown.exe");
        std::fs::copy(source, &exe).expect("a private copy of cmd.exe");

        let mut stub = std::process::Command::new(&exe)
            .args(["/c", "timeout", "30"])
            .spawn()
            .expect("the stub starts");

        let child = Child::new(ChildSpec {
            adopt: true,
            ..ChildSpec::new(
                &format!("adopted-shutdown-{}", std::process::id()),
                exe,
                true,
            )
        });
        assert!(child.adopted(), "the stub is adopted");

        let mut supervisor = idle_supervisor(directory.path().to_path_buf());
        supervisor.children.push(child);
        supervisor.shutdown();

        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if stub.try_wait().expect("the stub is waitable").is_some() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "shutdown must stop an adopted child"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}
