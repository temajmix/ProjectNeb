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

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use neb_core::ipc::{self, Request, Response, ServerHandle};
use neb_core::logging::{Level, LogHandle};
use neb_core::time::UtcDateTime;
use serde::Serialize;
use serde_json::{Value, json};

use crate::children::{Child, ChildSpec, Enabled, NamedMutex};
use crate::taskbar_repair::{self, Outcome};

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

/// Executable name of the bar, looked up next to `neb-supervisor.exe`.
const SHELL_EXE: &str = "neb-shell.exe";

/// Executable name of the living wallpaper, looked up next to `neb-supervisor.exe`.
const WALLPAPER_EXE: &str = "neb-wallpaper.exe";

/// How long the supervisor waits for a shell to answer `shell.ping` at startup.
const SHELL_PROBE: Duration = Duration::from_secs(1);

/// Everything the command line can change about a run.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Shell executable, when it is not the `neb-shell.exe` next to this one.
    pub shell: Option<PathBuf>,
    /// Wallpaper executable, when it is not the `neb-wallpaper.exe` next to this one.
    pub wallpaper: Option<PathBuf>,
    /// A single arbitrary command to supervise instead of the configured children.
    pub command: Option<(PathBuf, Vec<String>)>,
    /// Echo every transition to stdout as well as the log.
    pub echo: bool,
    /// Pipe name; [`PIPE`] unless a test wants its own.
    pub pipe: String,
    /// Instance mutex name; [`INSTANCE_MUTEX`] unless a test wants its own.
    pub instance: String,
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
    /// Seconds since the supervisor started.
    pub uptime_s: u64,
}

/// The state the IPC threads read and the commands they leave for the loop.
#[derive(Debug)]
pub struct Shared {
    state: Mutex<StateDoc>,
    restarts: Mutex<Vec<String>>,
    stop: AtomicBool,
    started: Instant,
    names: Vec<String>,
}

impl Shared {
    /// A shared view over the children called `names`.
    pub fn new(names: Vec<String>) -> Self {
        Self {
            state: Mutex::new(StateDoc::default()),
            restarts: Mutex::new(Vec::new()),
            stop: AtomicBool::new(false),
            started: Instant::now(),
            names,
        }
    }

    /// Replaces the published state.
    pub fn publish(&self, shell: ChildState, wallpaper: ChildState) {
        let mut state = self.lock_state();
        state.shell = shell;
        state.wallpaper = wallpaper;
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

    /// Asks the loop to stop.
    pub fn request_stop(&self) {
        self.stop.store(true, Ordering::SeqCst);
    }

    /// Whether a stop was asked for.
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Whether `name` is one of the children this supervisor knows.
    fn knows(&self, name: &str) -> bool {
        self.names.iter().any(|known| known == name)
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
#[derive(Debug)]
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
    /// Held for the whole run: dropping it is what lets the next supervisor start.
    _instance: NamedMutex,
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
        let shared = Arc::new(Shared::new(names));

        let mut supervisor = Self {
            reporter,
            shared,
            server: None,
            children: Vec::new(),
            started: Instant::now(),
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
            }
            supervisor.children.push(child);
        }

        for index in 0..supervisor.children.len() {
            if supervisor.children[index].enabled()
                && supervisor.children[index].blocked().is_none()
            {
                supervisor.start_child(index);
            }
        }

        let pipe = if options.pipe.trim().is_empty() {
            PIPE.to_owned()
        } else {
            options.pipe
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

        Ok(Some(supervisor))
    }

    /// Whether a stop was requested over IPC.
    pub fn stopping(&self) -> bool {
        self.shared.stopping()
    }

    /// Reaps dead children, starts the ones that are due and applies queued commands.
    pub fn tick(&mut self) {
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

    /// Stops every child and closes the pipe.
    pub fn shutdown(&mut self) {
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
    }

    /// Publishes what the IPC handlers answer from.
    fn publish(&self) {
        let shell = self.child_state("shell");
        let wallpaper = self.child_state("wallpaper");
        self.shared.publish(shell, wallpaper);
    }

    /// The published state of the child called `name`.
    fn child_state(&self, name: &str) -> ChildState {
        self.children
            .iter()
            .find(|child| child.name() == name)
            .map_or_else(ChildState::default, |child| ChildState {
                pid: child.pid(),
                running: child.running(),
            })
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

    let enabled = Enabled::from_file(&neb_core::paths::config_file());

    let shell = options.shell.clone().unwrap_or_else(|| sibling(SHELL_EXE));
    let wallpaper = options
        .wallpaper
        .clone()
        .unwrap_or_else(|| sibling(WALLPAPER_EXE));

    vec![
        ChildSpec::new("shell", shell, enabled.shell),
        ChildSpec::new("wallpaper", wallpaper, enabled.wallpaper),
    ]
}

/// `name` next to the running executable, which is where `cargo build` puts every binary.
fn sibling(name: &str) -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(Path::to_path_buf))
        .map_or_else(|| PathBuf::from(name), |directory| directory.join(name))
}

/// Seconds with one decimal, for log lines.
fn seconds(duration: Duration) -> String {
    format!("{:.1}", duration.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Shared {
        Shared::new(vec!["shell".to_owned(), "wallpaper".to_owned()])
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
    fn stop_sets_the_flag_and_answers() {
        let shared = shared();
        assert!(!shared.stopping());

        let response = dispatch(&shared, request("supervisor.stop", None));

        assert!(response.ok);
        assert_eq!(response.result, Some(json!({ "stopping": true })));
        assert!(shared.stopping(), "the loop must see the request");
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
        let specs = specs(&Options::default());

        assert_eq!(specs.len(), 2);
        assert_eq!(specs[0].name, "shell");
        assert!(specs[0].exe.ends_with(SHELL_EXE));
        assert_eq!(specs[1].name, "wallpaper");
        assert!(specs[1].exe.ends_with(WALLPAPER_EXE));
        assert!(
            specs[0].enabled,
            "the shipped config has no shell section, so the shell runs"
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
}
