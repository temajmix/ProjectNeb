//! The Rich Presence state machine and its worker thread.
//!
//! ```text
//! Disconnected --connect--> Connecting --READY--> Ready
//!       ^                        |                  |
//!       |                     failure             close/error
//!       |                        v                  v
//!       +----disconnect-------- Error <-------------+
//! ```
//!
//! The manager thread owns the connection. It blocks on a channel with a short timeout, so a
//! command from the settings window is answered without the window ever waiting on Discord: the IPC
//! handler only posts a command and returns. The reader is a second thread because `PeekNamedPipe`
//! has to be polled; it stops within [`crate::pipe::READ_POLL`] when the manager closes a connection.
//!
//! No state is invented. `Ready` is entered only after a `DISPATCH`/`READY` frame has been read, and
//! an empty application id keeps the service in `Disconnected` with the message `не настроено`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use neb_core::config::{Config, ConfigStore, DiscordRule, DiscordSettings};
use neb_core::logging::LogHandle;
use serde_json::{Value, json};

use crate::activity;
use crate::foreground;
use crate::pipe::{self, Connection, PipeError};
use crate::presets::{Preset, PresetStore, Scan, StoreError};
use crate::protocol::{Frame, Opcode};
use crate::rules::{self, Foreground, Selection};

/// How long the manager waits for any event before re-checking its deadlines.
const TICK: Duration = Duration::from_millis(250);

/// How long Discord gets to answer the handshake with `READY`.
pub const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// How long opening one explicitly named pipe may wait for it to become free.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Reconnect delays, indexed by consecutive failures. The last value repeats.
pub const BACKOFF_STEPS: [Duration; 6] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(4),
    Duration::from_secs(8),
    Duration::from_secs(16),
    Duration::from_secs(30),
];

/// The message shown whenever no application id is configured.
pub const NOT_CONFIGURED: &str = "не настроено";

/// Where the client looks for Discord.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum PipeSelector {
    /// `\\.\pipe\discord-ipc-0` through `-9`, in order.
    #[default]
    Auto,
    /// One explicitly named pipe, used by tests and by `neb-discord --pipe`.
    Named(String),
}

impl PipeSelector {
    fn open(&self) -> Result<Connection, PipeError> {
        match self {
            Self::Auto => pipe::connect(),
            Self::Named(name) => pipe::connect_named(name, CONNECT_TIMEOUT),
        }
    }
}

/// The four states the UI can show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Not connected, and not trying.
    Disconnected,
    /// A pipe is open or being opened; the handshake has not been answered yet.
    Connecting,
    /// `READY` arrived.
    Ready,
    /// The last attempt failed; a retry may be scheduled.
    Error,
}

impl Phase {
    /// The lowercase name used by `discord.getState`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disconnected => "disconnected",
            Self::Connecting => "connecting",
            Self::Ready => "ready",
            Self::Error => "error",
        }
    }
}

/// The reconnect ladder, with no sleeping in it so every step is a unit test.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Backoff {
    failures: u32,
}

impl Backoff {
    /// How many consecutive failures have happened since the last reset.
    pub fn failures(&self) -> u32 {
        self.failures
    }

    /// The delay before the next attempt, and the new failure count.
    pub fn delay(&mut self) -> Duration {
        let index = (self.failures as usize).min(BACKOFF_STEPS.len() - 1);
        self.failures = self.failures.saturating_add(1);
        BACKOFF_STEPS[index]
    }

    /// Forgets the ladder after a successful `READY`.
    pub fn reset(&mut self) {
        self.failures = 0;
    }
}

/// Everything the service needs to run.
#[derive(Debug, Clone)]
pub struct Options {
    /// Discord application id; empty means "not configured".
    pub app_id: String,
    /// Whether `config.discord.enabled` asked for the module.
    pub enabled: bool,
    /// A pinned preset, or `None` for the foreground rules.
    pub active_preset: Option<String>,
    /// Rules from `config.discord.rules`.
    pub rules: Vec<DiscordRule>,
    /// Preset directory.
    pub presets: PresetStore,
    /// Whether to start trying as soon as the service starts.
    pub connect_on_start: bool,
    /// Which pipe to use.
    pub pipe: PipeSelector,
    /// The real config, when the service may persist a change.
    pub config: Option<ConfigStore>,
    /// Log sink, when one was opened.
    pub log: Option<LogHandle>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            app_id: String::new(),
            enabled: false,
            active_preset: None,
            rules: Vec::new(),
            presets: PresetStore::at_default_location(),
            connect_on_start: false,
            pipe: PipeSelector::Auto,
            config: None,
            log: None,
        }
    }
}

impl Options {
    /// Builds the options the application uses from the shared config file.
    pub fn from_config(
        config: &Config,
        config_store: Option<ConfigStore>,
        log: Option<LogHandle>,
    ) -> Self {
        let app_id = config.discord.app_id.trim().to_owned();
        Self {
            enabled: config.discord.enabled,
            connect_on_start: config.discord.enabled && !app_id.is_empty(),
            app_id,
            active_preset: config.discord.active_preset.clone(),
            rules: config.discord.rules.clone(),
            presets: PresetStore::at_default_location(),
            pipe: PipeSelector::Auto,
            config: config_store,
            log,
        }
    }
}

/// Why the service thread could not start.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    /// The manager thread could not be spawned.
    #[error("cannot start the discord worker: {0}")]
    Thread(#[from] std::io::Error),
}

/// A command the IPC thread posts to the manager.
#[derive(Debug, Clone)]
enum Command {
    Connect,
    Disconnect,
    SetPreset(Option<String>),
    SetRules(Vec<DiscordRule>),
    Shutdown,
}

/// One thing the manager reacts to.
#[derive(Debug)]
enum Event {
    Command(Command),
    Frame(Frame),
    ReadFailed(String),
}

/// Signal shared with every reader of the service.
#[derive(Debug)]
pub struct Shared {
    app_id: String,
    enabled: bool,
    inner: Mutex<Inner>,
}

#[derive(Debug)]
struct Inner {
    phase: Phase,
    detail: Option<String>,
    last_error: Option<String>,
    pipe: Option<String>,
    user: Option<Value>,
    foreground: Option<Foreground>,
    selection: Option<Selection>,
    active_preset: Option<String>,
    rules: Vec<DiscordRule>,
    failures: u32,
    frames: VecDeque<String>,
    started_at: Option<u64>,
    last_activity_at: Option<u64>,
}

impl Shared {
    fn new(options: &Options, active_preset: Option<String>) -> Self {
        let configured = !options.app_id.is_empty();
        Self {
            app_id: options.app_id.clone(),
            enabled: options.enabled,
            inner: Mutex::new(Inner {
                phase: Phase::Disconnected,
                detail: Some(if configured {
                    "отключено".to_owned()
                } else {
                    NOT_CONFIGURED.to_owned()
                }),
                last_error: None,
                pipe: None,
                user: None,
                foreground: None,
                selection: None,
                active_preset,
                rules: options.rules.clone(),
                failures: 0,
                frames: VecDeque::new(),
                started_at: None,
                last_activity_at: None,
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// Whether an application id is present.
    pub fn configured(&self) -> bool {
        !self.app_id.is_empty()
    }

    /// The application id as configured.
    pub fn app_id(&self) -> &str {
        &self.app_id
    }

    /// Records one frame in the diagnostic ring buffer.
    fn record(&self, direction: &str, frame: &Frame) {
        let mut inner = self.lock();
        if inner.frames.len() >= 100 {
            inner.frames.pop_front();
        }
        inner
            .frames
            .push_back(format!("{direction} {}", frame.summary()));
    }

    /// Clones the rules for one evaluation.
    fn rules(&self) -> Vec<DiscordRule> {
        self.lock().rules.clone()
    }

    /// Clones the pinned preset.
    fn active_preset(&self) -> Option<String> {
        self.lock().active_preset.clone()
    }

    /// Replaces the rules synchronously, so `getRules` answers the write it just acknowledged.
    fn set_rules(&self, rules: Vec<DiscordRule>) {
        self.lock().rules = rules;
    }

    /// Replaces the pinned preset synchronously.
    fn set_active_preset(&self, name: Option<String>) {
        self.lock().active_preset = name;
    }

    /// Marks the moment `connect` was accepted, so `getState` answers `connecting` at once.
    fn mark_connecting(&self) {
        let mut inner = self.lock();
        if inner.phase != Phase::Ready {
            inner.phase = Phase::Connecting;
            inner.detail = Some("подключение к Discord…".to_owned());
            inner.last_error = None;
        }
    }

    /// Marks an explicit disconnect.
    fn mark_disconnected(&self) {
        let mut inner = self.lock();
        inner.phase = Phase::Disconnected;
        inner.detail = Some("отключено".to_owned());
        inner.pipe = None;
        inner.user = None;
        inner.selection = None;
        inner.last_activity_at = None;
    }

    /// The answer to `discord.getState`.
    pub fn state(&self) -> Value {
        let inner = self.lock();
        let configured = !self.app_id.is_empty();
        let state = inner.phase.as_str();
        let message = match &inner.detail {
            Some(detail) => detail.clone(),
            None if !configured => NOT_CONFIGURED.to_owned(),
            None => match inner.phase {
                Phase::Disconnected => "отключено".to_owned(),
                Phase::Connecting => "подключение к Discord…".to_owned(),
                Phase::Ready => "подключено".to_owned(),
                Phase::Error => "ошибка подключения".to_owned(),
            },
        };

        json!({
            "ok": true,
            "state": state,
            "configured": configured,
            "enabled": self.enabled,
            "appId": self.app_id,
            "message": message,
            "lastError": inner.last_error,
            "pipe": inner.pipe,
            "activePreset": inner.active_preset,
            "effectivePreset": inner.selection.as_ref().map(|selection| &selection.preset),
            "presetSource": inner.selection.as_ref().map(|selection| selection.source),
            "ruleIndex": inner.selection.as_ref().and_then(|selection| selection.rule),
            "foreground": inner.foreground.as_ref().map(|foreground| json!({
                "exe": foreground.exe,
                "class": foreground.class,
                "title": foreground.title,
            })),
            "discordUser": inner.user,
            "failures": inner.failures,
            "startedAt": inner.started_at,
            "lastActivityAt": inner.last_activity_at,
            "rules": inner.rules.len(),
            "version": crate::VERSION,
            "frames": inner.frames.iter().collect::<Vec<_>>(),
        })
    }
}

/// A running Rich Presence service. Dropping it stops the worker without waiting for a join.
#[derive(Debug)]
pub struct Service {
    shared: Arc<Shared>,
    commands: Sender<Event>,
    worker: Option<JoinHandle<()>>,
    presets: PresetStore,
    config: Option<ConfigStore>,
}

impl Service {
    /// Starts the worker.
    ///
    /// # Errors
    ///
    /// [`ServiceError::Thread`] when the worker thread cannot be created.
    pub fn start(options: Options) -> Result<Self, ServiceError> {
        let active = options
            .active_preset
            .clone()
            .filter(|name| options.presets.load(name).is_ok());
        let connect_on_start = options.connect_on_start && !options.app_id.is_empty();
        let presets = options.presets.clone();
        let config = options.config.clone();
        let shared = Arc::new(Shared::new(&options, active));
        let (sender, receiver) = mpsc::channel();
        let events = sender.clone();

        let manager = Manager {
            shared: Arc::clone(&shared),
            options,
            receiver,
            events,
            connection: None,
            ready: false,
            desired: false,
            reader_stop: None,
            reader: None,
            awaiting_ready: None,
            next_attempt: None,
            backoff: Backoff::default(),
            last_poll: Instant::now(),
            last_applied: None,
            nonce: 0,
        };

        let worker = thread::Builder::new()
            .name("neb-discord".to_owned())
            .spawn(move || manager.run())?;

        let service = Self {
            shared,
            commands: sender,
            worker: Some(worker),
            presets,
            config,
        };

        if connect_on_start {
            let _ = service.handle().connect();
        }

        Ok(service)
    }

    /// A cloneable handle for the IPC server and tests.
    pub fn handle(&self) -> Handle {
        Handle {
            shared: Arc::clone(&self.shared),
            commands: self.commands.clone(),
            presets: self.presets.clone(),
            config: self.config.clone(),
        }
    }

    /// The current `discord.getState` document.
    pub fn state(&self) -> Value {
        self.shared.state()
    }

    /// Stops the worker and waits for it.
    pub fn shutdown(mut self) {
        self.stop();
    }

    fn stop(&mut self) {
        let _ = self.commands.send(Event::Command(Command::Shutdown));
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for Service {
    fn drop(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = self.commands.send(Event::Command(Command::Shutdown));
            // Do not block a destructor on the worker; the explicit `shutdown` is the joining path.
            drop(worker);
        }
    }
}

/// A cloneable command surface shared with the IPC server.
#[derive(Debug, Clone)]
pub struct Handle {
    shared: Arc<Shared>,
    commands: Sender<Event>,
    presets: PresetStore,
    config: Option<ConfigStore>,
}

impl Handle {
    /// `discord.getState`.
    pub fn state(&self) -> Value {
        self.shared.state()
    }

    /// Whether an application id is configured.
    pub fn configured(&self) -> bool {
        self.shared.configured()
    }

    /// `discord.connect`: honours an empty app id instead of pretending to connect.
    ///
    /// # Errors
    ///
    /// `"не настроено: укажите discord.appId"` when the id is empty.
    pub fn connect(&self) -> Result<(), String> {
        if !self.shared.configured() {
            return Err(format!("{NOT_CONFIGURED}: укажите discord.appId"));
        }

        self.shared.mark_connecting();
        self.send(Command::Connect)
    }

    /// `discord.disconnect`.
    ///
    /// # Errors
    ///
    /// The worker is gone.
    pub fn disconnect(&self) -> Result<(), String> {
        self.shared.mark_disconnected();
        self.send(Command::Disconnect)
    }

    /// `discord.listPresets`.
    pub fn scan_presets(&self) -> Scan {
        self.presets.scan()
    }

    /// `discord.savePreset`.
    ///
    /// # Errors
    ///
    /// [`StoreError`] for a preset Discord would refuse or a directory that cannot be written.
    pub fn save_preset(&self, preset: &Preset) -> Result<std::path::PathBuf, StoreError> {
        self.presets.save(preset)
    }

    /// Pins a preset (`Some`) or returns to the rules (`None`).
    ///
    /// The config is written first, so a failed save cannot leave the running service showing a
    /// preset the next start will not remember.
    ///
    /// # Errors
    ///
    /// A missing preset, a failed config save, or a dead worker.
    pub fn set_preset(&self, name: Option<String>) -> Result<(), String> {
        if let Some(name) = name.as_deref() {
            if name.trim().is_empty() {
                return Err("preset name is empty".to_owned());
            }
            self.presets
                .load(name)
                .map_err(|_| format!("no preset named '{name}'"))?;
        }

        self.persist(|discord| discord.active_preset = name.clone())?;
        self.shared.set_active_preset(name.clone());
        self.send(Command::SetPreset(name))
    }

    /// Replaces the rules and persists them.
    ///
    /// # Errors
    ///
    /// A rule with no pattern or preset, a failed config save, or a dead worker.
    pub fn set_rules(&self, rules: Vec<DiscordRule>) -> Result<(), String> {
        validate_rules(&rules)?;
        self.persist(|discord| discord.rules = rules.clone())?;
        self.shared.set_rules(rules.clone());
        self.send(Command::SetRules(rules))
    }

    /// The rules currently armed.
    pub fn rules(&self) -> Vec<DiscordRule> {
        self.shared.rules()
    }

    fn send(&self, command: Command) -> Result<(), String> {
        self.commands
            .send(Event::Command(command))
            .map_err(|_| "the discord worker is not running".to_owned())
    }

    fn persist(&self, change: impl FnOnce(&mut DiscordSettings)) -> Result<(), String> {
        let Some(store) = &self.config else {
            return Ok(());
        };

        let mut config = store.load_checked().map_err(|error| error.to_string())?;
        change(&mut config.discord);
        store.save(&config).map_err(|error| error.to_string())
    }
}

/// Rejects a rule list the service cannot evaluate predictably.
///
/// # Errors
///
/// A sentence naming the rule index and the empty field.
pub fn validate_rules(rules: &[DiscordRule]) -> Result<(), String> {
    for (index, rule) in rules.iter().enumerate() {
        if rule.pattern.trim().is_empty() {
            return Err(format!("rule {index} has an empty match pattern"));
        }
        if rule.preset.trim().is_empty() {
            return Err(format!("rule {index} has an empty preset"));
        }
    }
    Ok(())
}

struct Manager {
    shared: Arc<Shared>,
    options: Options,
    receiver: Receiver<Event>,
    events: Sender<Event>,
    connection: Option<Connection>,
    ready: bool,
    desired: bool,
    reader_stop: Option<Arc<AtomicBool>>,
    reader: Option<JoinHandle<()>>,
    awaiting_ready: Option<Instant>,
    next_attempt: Option<Instant>,
    backoff: Backoff,
    last_poll: Instant,
    last_applied: Option<String>,
    nonce: u64,
}

impl Manager {
    fn run(mut self) {
        loop {
            match self.receiver.recv_timeout(TICK) {
                Ok(Event::Command(Command::Shutdown))
                | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
                Ok(Event::Command(command)) => self.command(command),
                Ok(Event::Frame(frame)) => self.frame(frame),
                Ok(Event::ReadFailed(reason)) => {
                    if self.connection.is_some() {
                        self.fail(reason);
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }

            self.tick();
        }

        self.finish();
    }

    fn tick(&mut self) {
        let now = Instant::now();

        if self.ready && now.duration_since(self.last_poll) >= rules::POLL_INTERVAL {
            self.poll();
        }

        if let Some(since) = self.awaiting_ready {
            if now.duration_since(since) >= READY_TIMEOUT {
                self.fail("Discord did not answer the handshake with READY".to_owned());
                return;
            }
        }

        if self.desired && self.connection.is_none() && self.awaiting_ready.is_none() {
            let due = self.next_attempt.is_none_or(|deadline| now >= deadline);
            if due {
                self.connect();
            }
        }
    }

    fn command(&mut self, command: Command) {
        match command {
            Command::Connect => {
                if !self.shared.configured() {
                    self.desired = false;
                    self.set_disconnected(NOT_CONFIGURED.to_owned());
                    return;
                }
                self.desired = true;
                self.next_attempt = Some(Instant::now());
                self.connect();
            }
            Command::Disconnect => {
                self.desired = false;
                self.clear_activity();
                self.close_connection();
                self.backoff.reset();
                self.next_attempt = None;
                self.set_disconnected("отключено".to_owned());
            }
            Command::SetPreset(name) => {
                self.shared.lock().active_preset = name;
                self.last_applied = None;
                if self.ready {
                    self.poll();
                }
            }
            Command::SetRules(rules) => {
                self.shared.lock().rules = rules;
                self.last_applied = None;
                if self.ready {
                    self.poll();
                }
            }
            Command::Shutdown => {}
        }
    }

    fn connect(&mut self) {
        if self.connection.is_some() {
            return;
        }
        if !self.shared.configured() {
            self.desired = false;
            self.set_disconnected(NOT_CONFIGURED.to_owned());
            return;
        }

        self.set_phase(Phase::Connecting, Some("подключение к Discord…".to_owned()));
        self.log("info", "connecting to Discord");

        let connection = match self.options.pipe.open() {
            Ok(connection) => connection,
            Err(error) => {
                self.fail(error.to_string());
                return;
            }
        };

        let handshake = Frame::handshake(&self.options.app_id);
        self.shared.record("out", &handshake);
        if let Err(error) = connection.write_frame(&handshake) {
            self.fail(error.to_string());
            return;
        }

        let stop = Arc::new(AtomicBool::new(false));
        let reader = {
            let connection = connection.clone();
            let stop = Arc::clone(&stop);
            let events = self.events.clone();
            thread::Builder::new()
                .name("neb-discord-reader".to_owned())
                .spawn(move || read_loop(connection, stop, events))
        };

        let reader = match reader {
            Ok(reader) => reader,
            Err(error) => {
                self.fail(format!("cannot start the discord reader: {error}"));
                return;
            }
        };

        self.shared.lock().pipe = Some(connection.name().to_owned());
        self.connection = Some(connection);
        self.reader_stop = Some(stop);
        self.reader = Some(reader);
        self.awaiting_ready = Some(Instant::now());
    }

    fn frame(&mut self, frame: Frame) {
        self.shared.record("in ", &frame);

        match frame.op {
            Opcode::Ping => {
                let pong = Frame::pong(frame.payload.clone());
                let _ = self.send(&pong);
            }
            Opcode::Close => {
                let reason = frame
                    .payload
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("Discord closed the connection");
                self.fail(reason.to_owned());
            }
            Opcode::Frame => {
                if !self.ready {
                    if frame.event_name() == Some("READY") {
                        self.became_ready(&frame);
                    } else if frame.event_name() == Some("ERROR") {
                        self.fail(describe_error(&frame));
                    }
                } else if frame.event_name() == Some("ERROR") {
                    let reason = describe_error(&frame);
                    let mut inner = self.shared.lock();
                    inner.last_error = Some(reason.clone());
                    inner.detail = Some(format!("Discord отклонил запрос: {reason}"));
                }
            }
            Opcode::Handshake | Opcode::Pong => {}
        }
    }

    fn became_ready(&mut self, frame: &Frame) {
        let user = frame
            .payload
            .get("data")
            .and_then(|data| data.get("user"))
            .cloned();

        self.ready = true;
        self.awaiting_ready = None;
        self.backoff.reset();
        self.last_applied = None;
        self.last_poll = Instant::now()
            .checked_sub(rules::POLL_INTERVAL)
            .unwrap_or_else(Instant::now);

        {
            let mut inner = self.shared.lock();
            inner.phase = Phase::Ready;
            inner.detail = Some("подключено".to_owned());
            inner.last_error = None;
            inner.user = user;
            inner.failures = 0;
            inner.started_at = Some(activity::now_unix());
        }

        self.log("info", "Discord answered READY");
        self.poll();
    }

    fn poll(&mut self) {
        self.last_poll = Instant::now();

        let foreground = foreground::sample();
        let rules = self.shared.rules();
        let active = self.shared.active_preset();
        let selection = rules::select(&rules, foreground.as_ref(), active.as_deref());

        {
            let mut inner = self.shared.lock();
            inner.foreground = foreground;
            inner.selection = selection.clone();
        }

        if !self.ready {
            return;
        }

        self.apply(selection);
    }

    fn apply(&mut self, selection: Option<Selection>) {
        let Some(selection) = selection else {
            if self.last_applied.take().is_some() {
                self.clear_activity();
            }
            self.shared.lock().selection = None;
            return;
        };

        if self.last_applied.as_deref() == Some(selection.preset.as_str()) {
            self.shared.lock().selection = Some(selection);
            return;
        }

        let preset = match self.options.presets.load(&selection.preset) {
            Ok(preset) => preset,
            Err(error) => {
                let reason = format!("preset '{}' cannot be applied: {error}", selection.preset);
                self.log("warn", &reason);
                self.shared.lock().detail = Some(reason);
                if self.last_applied.take().is_some() {
                    self.clear_activity();
                }
                return;
            }
        };

        let activity = activity::build(&preset, activity::now_unix());
        self.nonce = self.nonce.wrapping_add(1);
        let frame = Frame::set_activity(
            std::process::id(),
            Some(&activity),
            &format!("neb-{}-{}", std::process::id(), self.nonce),
        );

        if self.send(&frame) {
            self.last_applied = Some(selection.preset.clone());
            let mut inner = self.shared.lock();
            inner.selection = Some(selection);
            inner.last_activity_at = Some(activity::now_unix());
            inner.detail = Some("подключено".to_owned());
        }
    }

    fn clear_activity(&mut self) {
        if !self.ready || self.connection.is_none() {
            self.last_applied = None;
            return;
        }

        self.nonce = self.nonce.wrapping_add(1);
        let frame = Frame::set_activity(
            std::process::id(),
            None,
            &format!("neb-clear-{}-{}", std::process::id(), self.nonce),
        );
        if self.send(&frame) {
            self.last_applied = None;
            let mut inner = self.shared.lock();
            inner.last_activity_at = None;
            inner.selection = None;
        }
    }

    fn send(&mut self, frame: &Frame) -> bool {
        let Some(connection) = &self.connection else {
            return false;
        };

        self.shared.record("out", frame);
        if let Err(error) = connection.write_frame(frame) {
            self.fail(error.to_string());
            return false;
        }
        true
    }

    fn fail(&mut self, reason: String) {
        self.close_connection();

        if !self.desired || !self.shared.configured() {
            self.set_disconnected(reason);
            return;
        }

        let delay = self.backoff.delay();
        self.next_attempt = Some(Instant::now() + delay);

        {
            let mut inner = self.shared.lock();
            inner.phase = Phase::Error;
            inner.detail = Some(reason.clone());
            inner.last_error = Some(reason.clone());
            inner.pipe = None;
            inner.user = None;
            inner.failures = inner.failures.saturating_add(1);
            inner.selection = None;
        }

        self.log(
            "warn",
            &format!(
                "Discord connection failed ({reason}); retry in {} s",
                delay.as_secs()
            ),
        );
    }

    fn close_connection(&mut self) {
        if let Some(stop) = self.reader_stop.take() {
            stop.store(true, Ordering::SeqCst);
        }
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        self.connection = None;
        self.ready = false;
        self.awaiting_ready = None;
        self.shared.lock().pipe = None;
    }

    fn set_disconnected(&mut self, detail: String) {
        self.close_connection();
        let mut inner = self.shared.lock();
        inner.phase = Phase::Disconnected;
        inner.detail = Some(detail);
        inner.user = None;
        inner.selection = None;
        inner.failures = 0;
        inner.last_activity_at = None;
    }

    fn set_phase(&mut self, phase: Phase, detail: Option<String>) {
        let mut inner = self.shared.lock();
        inner.phase = phase;
        inner.detail = detail;
    }

    fn finish(&mut self) {
        self.desired = false;
        if self.ready {
            self.clear_activity();
        }
        self.close_connection();
    }

    fn log(&self, level: &str, message: &str) {
        let Some(log) = &self.options.log else {
            return;
        };

        if level == "warn" {
            log.warn("discord", message);
        } else {
            log.info("discord", message);
        }
    }
}

/// The reader thread: poll one connection and forward frames until told to stop.
fn read_loop(connection: Connection, stop: Arc<AtomicBool>, events: Sender<Event>) {
    loop {
        match connection.read_frame(&stop) {
            Ok(Some(frame)) => {
                if events.send(Event::Frame(frame)).is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(PipeError::Stopped) => return,
            Err(error) => {
                let _ = events.send(Event::ReadFailed(error.to_string()));
                return;
            }
        }
    }
}

/// The Discord error document as one sentence.
fn describe_error(frame: &Frame) -> String {
    let data = frame.payload.get("data");
    let message = data
        .and_then(|data| data.get("message"))
        .and_then(Value::as_str)
        .unwrap_or("Discord returned an error");
    let code = data
        .and_then(|data| data.get("code"))
        .and_then(Value::as_i64);

    match code {
        Some(code) => format!("{message} (code {code})"),
        None => message.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::Preset;

    #[test]
    fn the_reconnect_ladder_steps_up_and_then_repeats() {
        let mut backoff = Backoff::default();

        let steps: Vec<u64> = (0..8).map(|_| backoff.delay().as_secs()).collect();

        assert_eq!(steps, vec![1, 2, 4, 8, 16, 30, 30, 30]);
        assert_eq!(backoff.failures(), 8);

        backoff.reset();
        assert_eq!(backoff.failures(), 0);
        assert_eq!(backoff.delay(), Duration::from_secs(1));
    }

    #[test]
    fn an_empty_application_id_reports_not_configured_and_never_connects() {
        let service = Service::start(Options::default()).expect("start");
        let state = service.state();

        assert_eq!(state["configured"], json!(false));
        assert_eq!(state["state"], json!("disconnected"));
        assert_eq!(state["message"], json!(NOT_CONFIGURED));
        assert_eq!(state["appId"], json!(""));
        assert_eq!(state["pipe"], Value::Null);
        assert_eq!(state["effectivePreset"], Value::Null);

        let error = service.handle().connect().expect_err("must refuse");
        assert!(error.contains(NOT_CONFIGURED), "{error}");
        assert_eq!(service.state()["state"], json!("disconnected"));

        service.shutdown();
    }

    #[test]
    fn rules_are_validated_by_index() {
        let rules = vec![
            DiscordRule {
                pattern: "exe:game.exe".to_owned(),
                preset: "Gaming".to_owned(),
                enabled: true,
            },
            DiscordRule {
                pattern: "  ".to_owned(),
                preset: "Other".to_owned(),
                enabled: true,
            },
        ];

        let error = validate_rules(&rules).expect_err("empty pattern must fail");
        assert!(error.contains("rule 1"), "{error}");

        let rules = vec![DiscordRule {
            pattern: "exe:game.exe".to_owned(),
            preset: String::new(),
            enabled: true,
        }];
        assert!(validate_rules(&rules).is_err());
    }

    #[test]
    fn a_preset_missing_from_the_store_is_not_applied() {
        let directory = tempfile::tempdir().expect("temp dir");
        let options = Options {
            app_id: String::new(),
            active_preset: Some("Missing".to_owned()),
            presets: PresetStore::new(directory.path()),
            ..Options::default()
        };

        let service = Service::start(options).expect("start");

        assert_eq!(service.state()["activePreset"], Value::Null);
        service.shutdown();
    }

    fn wait_until(timeout: Duration, predicate: impl Fn() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if predicate() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn the_handshake_and_set_activity_sequence_is_what_discord_expects() {
        let directory = tempfile::tempdir().expect("temp dir");
        let presets = PresetStore::new(directory.path());
        presets
            .save(&Preset {
                name: "Gaming".to_owned(),
                details: "Playing ProjectNeb".to_owned(),
                state: "Testing the IPC".to_owned(),
                ..Preset::default()
            })
            .expect("save preset");

        let fake = crate::fake::FakePipe::start(crate::fake::TEST_PIPE, Duration::from_millis(300));

        let service = Service::start(Options {
            app_id: "123456789012345678".to_owned(),
            enabled: true,
            active_preset: Some("Gaming".to_owned()),
            presets,
            connect_on_start: true,
            pipe: PipeSelector::Named(crate::fake::TEST_PIPE.to_owned()),
            ..Options::default()
        })
        .expect("start");

        assert!(
            wait_until(Duration::from_secs(2), || {
                service.state()["state"] == json!("connecting")
            }),
            "the service must report connecting while the handshake is unanswered: {}",
            service.state()
        );
        println!("CONNECTING_STATE={}", service.state());

        assert!(
            wait_until(Duration::from_secs(3), || {
                service.state()["state"] == json!("ready")
            }),
            "the service must become ready after READY: {}",
            service.state()
        );
        println!("READY_STATE={}", service.state());

        let our_pid = std::process::id().to_string();
        assert!(
            wait_until(Duration::from_secs(2), || {
                service.state()["frames"].as_array().is_some_and(|frames| {
                    frames.iter().any(|line| {
                        line.as_str().is_some_and(|line| {
                            line.contains("SET_ACTIVITY") && line.contains(&our_pid)
                        })
                    })
                })
            }),
            "SET_ACTIVITY must carry our pid: {}",
            service.state()
        );

        service.shutdown();
        let log = fake.finish();
        println!("FAKE_PIPE_FRAME_LOG\n{}", log.join("\n"));

        assert!(
            log[0].starts_with("recv op=0 HANDSHAKE"),
            "first frame: {}",
            log[0]
        );
        assert!(
            log[0].contains(r#""client_id":"123456789012345678""#),
            "handshake must carry the configured app id: {}",
            log[0]
        );
        assert!(
            log[1].starts_with("send op=1 FRAME") && log[1].contains(r#""evt":"READY""#),
            "second frame: {}",
            log[1]
        );
        assert!(
            log.iter().any(|line| {
                line.starts_with("recv op=1 FRAME")
                    && line.contains("SET_ACTIVITY")
                    && line.contains(&format!(r#""pid":{our_pid}"#))
            }),
            "a SET_ACTIVITY frame must carry our pid: {log:?}"
        );
        assert!(
            log.iter().any(|line| line.starts_with("send op=1 FRAME")),
            "the fake Discord must acknowledge the activity: {log:?}"
        );
    }

    #[test]
    fn a_saved_preset_is_visible_to_a_second_handle() {
        let directory = tempfile::tempdir().expect("temp dir");
        let presets = PresetStore::new(directory.path());
        let service = Service::start(Options {
            presets: presets.clone(),
            ..Options::default()
        })
        .expect("start");
        let handle = service.handle();

        handle
            .save_preset(&Preset::named("Saved"))
            .expect("save preset");

        assert_eq!(handle.scan_presets().presets, vec![Preset::named("Saved")]);
        service.shutdown();
    }
}
