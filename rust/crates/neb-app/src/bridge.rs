//! The JSON bridge between the dashboard and this host.
//!
//! The dashboard posts `{type: "neb.invoke", id, payload: {method, params}}`; the host answers
//! `{type: "neb.invoke.result", id, ok, payload, error}`. Both the flat shape the plan names
//! (`{type, id, method, params}`) and the nested one are accepted, because the bundle in
//! `src/Neb.UI` is built against the nested shape and a future build may post the flat one.
//!
//! Everything a method does is decided by [`dispatch`], which touches no COM and no window: the
//! WebView only moves strings in and out, and the tests call `dispatch` directly. The one exception
//! is `app.openChangelog`, `app.openLogsFolder` and `wallpaper.pickFile`, which reach the shell; their
//! platform call is a field of [`Context`], so a test can answer for them without opening a browser
//! or a file dialog.
//!
//! Three of the four ProjectNeb processes answer on their own pipe (`ProjectNeb.supervisor`,
//! `ProjectNeb.zapret`, `ProjectNeb.discord`, `ProjectNeb.wm`, `ProjectNeb.wallpaper`). The bridge is
//! the only place that knows all of them: it forwards a module call with a two-second budget and
//! turns "nobody listens" into one sentence the dashboard can show, instead of an IPC type error.
//!
//! Two documents are pushed without being asked for: `neb.metrics` every second and `neb.modules`
//! next to it, the first immediately after the page says `neb.ui.ready`. The worker owns the CPU
//! sampler and the supervisor probe, so the page never polls for numbers.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::{Duration, SystemTime};

use neb_core::config::{Config, ConfigStore, DEFAULT_LANGUAGE, DEFAULT_THEME_ID, SCHEMA_VERSION};
use neb_core::ipc::IpcError;
use neb_core::license::{account as account_api, ids, loopback, store};
use neb_core::logging::{Level, LogHandle};
use neb_core::metrics::Sampler;
use serde_json::{Value, json};

use crate::themes::{self, ThemeDoc};

/// Message type the dashboard sends.
pub const INVOKE_TYPE: &str = "neb.invoke";

/// Message type every answer carries.
pub const RESULT_TYPE: &str = "neb.invoke.result";

/// Message the page sends once its listeners are ready; the answer is the first metrics push.
pub const READY_TYPE: &str = "neb.ui.ready";

/// Push carrying machine load and per-process working sets.
pub const METRICS_TYPE: &str = "neb.metrics";

/// Push carrying the module switches from the config.
pub const MODULES_TYPE: &str = "neb.modules";

/// Pipe the shell answers on.
pub const SHELL_PIPE: &str = "ProjectNeb.shell";

/// Pipe the supervisor answers on.
pub const SUPERVISOR_PIPE: &str = "ProjectNeb.supervisor";

/// Pipe the living wallpaper answers on.
pub const WALLPAPER_PIPE: &str = "ProjectNeb.wallpaper";

/// Pipe the DPI-bypass module answers on.
pub const ZAPRET_PIPE: &str = "ProjectNeb.zapret";

/// Pipe the Discord Rich Presence module answers on.
pub const DISCORD_PIPE: &str = "ProjectNeb.discord";

/// Pipe the tiling window manager answers on.
pub const WM_PIPE: &str = "ProjectNeb.wm";
/// Pipe the `windhawk.*` methods call.
pub const WINDHAWK_PIPE: &str = "ProjectNeb.windhawk";

/// The longest a bridge method may wait for another ProjectNeb process.
pub const IPC_TIMEOUT: Duration = Duration::from_secs(2);

/// How long a presence probe may take when the supervisor is not the one answering.
///
/// Five probes on the metrics path would otherwise cost five full seconds on a machine where
/// nothing runs; a quarter of a second apiece is enough for a running process to answer.
pub const PRESENCE_TIMEOUT: Duration = Duration::from_millis(250);

/// How long the dashboard waits for a restart it asked for before it gets an answer anyway.
pub const RESTART_TIMEOUT: Duration = Duration::from_secs(4);

/// How often `neb.metrics` and `neb.modules` are pushed.
pub const PUSH_INTERVAL: Duration = Duration::from_secs(1);

/// Maximum number of log lines `logs.tail` returns.
pub const TAIL_LINES: usize = 200;

/// The changelog the dashboard's link opens.
pub const CHANGELOG_URL: &str = "https://github.com/temajmix/ProjectNeb/blob/main/CHANGELOG.md";

/// How long `account.signIn` waits for the browser callback.
pub const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(180);

/// What a module call says when no process answers on the module's pipe.
pub const MODULE_DOWN: &str = "модуль не запущен";

/// What a supervisor call says when no supervisor answers.
pub const SUPERVISOR_DOWN: &str = "супервизор не запущен";

/// Version this binary reports to the UI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Opens a URL or a folder in the user's shell.
pub type Opener = fn(&str) -> Result<(), String>;

/// Asks the user for a wallpaper file; `None` means the dialog was cancelled.
pub type Picker = fn(Option<isize>) -> Result<Option<String>, String>;

/// Everything a method needs, with the parts that differ in tests passed in.
#[derive(Debug, Clone)]
pub struct Context {
    /// The `config.json` this window edits.
    pub store: ConfigStore,
    /// Folder `themes.save` writes into.
    pub themes_dir: PathBuf,
    /// Window the `window.*` methods act on; `None` in tests and before the window exists.
    pub window: Option<isize>,
    /// Pipe the `shell.*` methods call.
    pub shell_pipe: String,
    /// Pipe `updates.check`, `app.*` and `windows.*` call.
    pub supervisor_pipe: String,
    /// Pipe `wallpaper.getState` probes.
    pub wallpaper_pipe: String,
    /// Pipe the `zapret.*` methods call.
    pub zapret_pipe: String,
    /// Pipe the `discord.*` methods call.
    pub discord_pipe: String,
    /// Pipe the `wm.*` methods call.
    pub wm_pipe: String,
    /// Pipe the `windhawk.*` methods call.
    pub windhawk_pipe: String,
    /// How long one IPC call may take.
    pub timeout: Duration,
    /// How long `account.signIn` waits for the browser callback.
    pub sign_in_timeout: Duration,
    /// Name this installation reports as a licence device.
    pub device_name: String,
    /// Log the window writes to; `None` in tests and when the log file could not be opened.
    pub log: Option<LogHandle>,
    /// Opens the changelog and the logs folder.
    pub opener: Opener,
    /// Asks for a wallpaper file.
    pub picker: Picker,
}

impl Context {
    /// The production context: the real config, the real themes folder, one window.
    pub fn new(
        store: ConfigStore,
        themes_dir: PathBuf,
        window: Option<isize>,
        log: Option<LogHandle>,
    ) -> Self {
        Self {
            store,
            themes_dir,
            window,
            shell_pipe: SHELL_PIPE.to_owned(),
            supervisor_pipe: SUPERVISOR_PIPE.to_owned(),
            wallpaper_pipe: WALLPAPER_PIPE.to_owned(),
            zapret_pipe: ZAPRET_PIPE.to_owned(),
            discord_pipe: DISCORD_PIPE.to_owned(),
            wm_pipe: WM_PIPE.to_owned(),
            windhawk_pipe: WINDHAWK_PIPE.to_owned(),
            timeout: IPC_TIMEOUT,
            sign_in_timeout: SIGN_IN_TIMEOUT,
            device_name: default_device_name(),
            log,
            opener: shell_open,
            picker: pick_wallpaper_file,
        }
    }

    /// A context for a test: a private store, a private themes folder and pipes nobody listens on.
    #[cfg(test)]
    fn for_test(root: &std::path::Path) -> Self {
        let tag = std::process::id();
        let unused = |name: &str| format!("{name}.test.unused.{tag}");

        Self {
            store: ConfigStore::new(root.join("config.json")),
            themes_dir: root.join("themes"),
            window: None,
            shell_pipe: unused(SHELL_PIPE),
            supervisor_pipe: unused(SUPERVISOR_PIPE),
            wallpaper_pipe: unused(WALLPAPER_PIPE),
            zapret_pipe: unused(ZAPRET_PIPE),
            discord_pipe: unused(DISCORD_PIPE),
            wm_pipe: unused(WM_PIPE),
            windhawk_pipe: unused(WINDHAWK_PIPE),
            timeout: Duration::from_millis(500),
            sign_in_timeout: SIGN_IN_TIMEOUT,
            device_name: "test-device".to_owned(),
            log: None,
            opener: |_| Ok(()),
            picker: |_| Ok(None),
        }
    }

    /// Writes one line when a log was opened for this window.
    pub fn log(&self, level: Level, message: &str) {
        if let Some(log) = &self.log {
            log.log(level, "bridge", message);
        }
    }

    /// The data root, i.e. the folder `config.json` sits in.
    fn root(&self) -> Result<&Path, MethodError> {
        self.store
            .path()
            .parent()
            .ok_or_else(|| "the config file has no parent folder".to_owned())
    }
}

/// One call the dashboard made.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// Correlation id the answer must carry.
    pub id: String,
    /// Method name, e.g. `config.get`.
    pub method: String,
    /// Arguments; JSON `null` when the dashboard sent none.
    pub params: Value,
}

/// What one inbound web message turned out to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Inbound {
    /// A method call to answer.
    Invoke(Job),
    /// The page is listening; push the live documents now.
    Ready,
    /// A message this host does not act on (`neb.metrics` coming back from a page, for instance).
    Ignored,
    /// Not a message this host can answer; the id is kept when the sender can still be told.
    Malformed {
        /// Correlation id, when the malformed message carried one.
        id: Option<String>,
        /// Why it was refused, in words a log line and the dashboard can show.
        reason: String,
    },
}

/// Parses one `chrome.webview` message.
pub fn parse_message(text: &str) -> Inbound {
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => {
            return Inbound::Malformed {
                id: None,
                reason: format!("message is not valid json: {error}"),
            };
        }
    };

    let Value::Object(object) = value else {
        return Inbound::Malformed {
            id: None,
            reason: "message must be a json object".to_owned(),
        };
    };

    match object.get("type").and_then(Value::as_str) {
        Some(READY_TYPE) => return Inbound::Ready,
        Some(INVOKE_TYPE) => {}
        _ => return Inbound::Ignored,
    }

    let Some(id) = object.get("id").and_then(scalar_id) else {
        return Inbound::Malformed {
            id: None,
            reason: "neb.invoke needs a string id".to_owned(),
        };
    };

    let (method, params) = match object.get("payload").and_then(Value::as_object) {
        Some(payload) => (payload.get("method"), payload.get("params")),
        None => (object.get("method"), object.get("params")),
    };

    let method = match method.and_then(Value::as_str) {
        Some(method) if !method.trim().is_empty() => method.trim().to_owned(),
        _ => {
            return Inbound::Malformed {
                id: Some(id),
                reason: "neb.invoke needs a non-empty method".to_owned(),
            };
        }
    };

    Inbound::Invoke(Job {
        id,
        method,
        params: params.cloned().unwrap_or(Value::Null),
    })
}

/// Reads an id that a JSON client can correlate on: a string, or a number rendered back to text.
fn scalar_id(value: &Value) -> Option<String> {
    match value {
        Value::String(id) if !id.is_empty() => Some(id.clone()),
        Value::Number(id) => Some(id.to_string()),
        _ => None,
    }
}

/// One answer, ready to be posted back to the page.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    /// Correlation id from the call.
    pub id: String,
    /// Whether the method ran.
    pub ok: bool,
    /// The method's value; JSON `null` when there is nothing to return.
    pub payload: Value,
    /// Why the method did not run.
    pub error: Option<String>,
}

impl Reply {
    /// A successful answer.
    pub fn success(job: &Job, payload: Value) -> Self {
        Self {
            id: job.id.clone(),
            ok: true,
            payload,
            error: None,
        }
    }

    /// A failed answer: the message is the whole payload, so a page can show it.
    pub fn failure(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            ok: false,
            payload: Value::Null,
            error: Some(message.into()),
        }
    }

    /// Renders the `neb.invoke.result` message the dashboard listens for.
    pub fn to_message(&self) -> String {
        json!({
            "type": RESULT_TYPE,
            "id": self.id,
            "ok": self.ok,
            "payload": self.payload,
            "error": self.error,
        })
        .to_string()
    }
}

/// Runs one method.
///
/// Unknown methods are refused by name rather than ignored: a typo in the dashboard should show up
/// in the UI instead of a promise that never settles.
pub fn dispatch(context: &Context, job: &Job) -> Reply {
    match dispatch_inner(context, job) {
        Ok(payload) => Reply::success(job, payload),
        Err(error) => Reply::failure(&job.id, error),
    }
}

/// The error a method returns; the message goes to the page verbatim.
type MethodError = String;

fn dispatch_inner(context: &Context, job: &Job) -> Result<Value, MethodError> {
    let method = job.method.as_str();

    if method.starts_with("zapret.") {
        return zapret(context, method, &job.params);
    }

    if method.starts_with("discord.") {
        return discord(context, method, &job.params);
    }

    if method.starts_with("wm.") {
        return module_call(
            &context.wm_pipe,
            method,
            optional_params(&job.params),
            context.timeout,
        );
    }

    if method.starts_with("windows.") {
        return windows(context, method, &job.params);
    }

    if method.starts_with("windhawk.") {
        return module_call(
            &context.windhawk_pipe,
            method,
            optional_params(&job.params),
            context.timeout,
        );
    }

    if method.starts_with("account.") {
        return account(context, method);
    }

    match method {
        "config.get" => {
            let config = context
                .store
                .load_checked()
                .map_err(|error| format!("cannot read the config: {error}"))?;

            config_payload(&config)
        }
        "config.set" => set_config(context, &job.params),
        "themes.list" => Ok(list_themes(context)),
        "themes.save" => save_theme(context, &job.params),
        "shell.reload" => shell_call(context, "shell.reload"),
        "shell.getState" => Ok(shell_state(context)),
        "app.version" => Ok(json!({
            "version": VERSION,
            "schemaVersion": SCHEMA_VERSION,
            "pid": std::process::id(),
        })),
        "app.openChangelog" => open_changelog(context),
        "app.openLogsFolder" => open_logs_folder(context),
        "app.restartShell" => restart_child(context, "shell"),
        "app.quit" => supervisor_call(context, "app.quit", None, context.timeout),
        "updates.check" => updates(context),
        "updates.whatsNew" => whats_new(context),
        "logs.tail" => tail_logs(context),
        "license.getState" => license_state(context),
        "wallpaper.getSettings" => wallpaper_settings(context),
        "wallpaper.setSettings" => set_wallpaper_settings(context, &job.params),
        "wallpaper.restart" => restart_wallpaper(context),
        "wallpaper.pickFile" => pick_wallpaper(context),
        "window.minimize" => window_message(context, crate::window::MINIMIZE),
        "window.close" => window_message(context, crate::window::CLOSE),
        "window.toggleMaximize" => window_message(context, crate::window::TOGGLE_MAXIMIZE),
        other => Err(format!("unknown method '{other}'")),
    }
}

/// `config.set` payload: a full [`Config`], or one of the two patch shapes the dashboard sends.
///
/// The dashboard has three ways to save: a whole document (`{config: {...}}`), a dotted path
/// (`{path: "shell.barHeight", value: 56}`) and a section merge (`{section: "updates", values:
/// {"channel": "stable"}}`). All three land here, because a settings window that sends a patch but
/// is answered by replacing the file would wipe every field the patch did not name.
fn set_config(context: &Context, params: &Value) -> Result<Value, MethodError> {
    let document = config_document(context, params)?;
    let mut config = match &document {
        Some(document) => deserialize_config(document)?,
        None => context
            .store
            .load_checked()
            .map_err(|error| format!("cannot read the config: {error}"))?,
    };

    // The dashboard's `AppConfig` does not carry the account section, so a whole-document save
    // from an older page must not wipe the service URL or the device id.
    if document
        .as_ref()
        .and_then(Value::as_object)
        .is_some_and(|object| !object.contains_key("account"))
    {
        let stored = context
            .store
            .load_checked()
            .map_err(|error| format!("cannot read the config: {error}"))?;
        config.account = stored.account;
    }

    normalise(&mut config);

    context
        .store
        .save(&config)
        .map_err(|error| format!("cannot save the config: {error}"))?;

    config_payload(&config)
}

/// The config as the dashboard reads it: the persisted document plus the aliases its controls use.
///
/// `wallpaperEnabled` is the one field the dashboard expects next to the other shell switches while
/// the file keeps it in `wallpapers.enabled` (which is what `neb-wallpaper` and the supervisor
/// read). Reporting both keeps the Overview's toggle from claiming the wallpapers are on when the
/// process is not.
fn config_payload(config: &Config) -> Result<Value, MethodError> {
    let mut value = serde_json::to_value(config).map_err(|error| error.to_string())?;
    let wallpaper = config.wallpapers.enabled;

    if let Value::Object(root) = &mut value
        && let Some(Value::Object(shell)) = root.get_mut("shell")
    {
        shell.insert("wallpaperEnabled".to_owned(), json!(wallpaper));
    }

    Ok(value)
}

/// The document `config.set` should deserialize, or `None` when the call is a patch on the file.
fn config_document(context: &Context, params: &Value) -> Result<Option<Value>, MethodError> {
    if let Some(document) = params.get("config") {
        return Ok(Some(document.clone()));
    }

    if let Some(path) = params.get("path").and_then(Value::as_str) {
        let value = params
            .get("value")
            .ok_or_else(|| "'config.set' needs a 'value' next to the path".to_owned())?;
        let mut document = current_config(context)?;
        set_path(&mut document, path, value.clone())?;

        return Ok(Some(document));
    }

    if let (Some(section), Some(values)) = (
        params.get("section").and_then(Value::as_str),
        params.get("values"),
    ) {
        let Value::Object(values) = values else {
            return Err("'config.set' needs an object in 'values'".to_owned());
        };
        let mut document = current_config(context)?;
        let object = document
            .as_object_mut()
            .expect("the current config is an object");
        let target = object
            .entry(section.to_owned())
            .or_insert_with(|| json!({}));
        let Value::Object(target) = target else {
            return Err(format!(
                "'config.set' cannot merge into '{section}': it is not an object"
            ));
        };

        for (key, value) in values {
            target.insert(key.clone(), value.clone());
        }

        return Ok(Some(document));
    }

    if is_config_document(params) {
        return Ok(Some(params.clone()));
    }

    Err(
        "'config.set' needs a config object, a {path, value} patch or a {section, values} patch"
            .to_owned(),
    )
}

/// The config currently on disk, as a JSON object a patch can be applied to.
fn current_config(context: &Context) -> Result<Value, MethodError> {
    let config = context
        .store
        .load_checked()
        .map_err(|error| format!("cannot read the config: {error}"))?;

    serde_json::to_value(config).map_err(|error| format!("config.set: {error}"))
}

/// Whether `value` names at least one field of [`Config`], so it is a whole-document save.
fn is_config_document(value: &Value) -> bool {
    const SECTIONS: &[&str] = &[
        "schemaVersion",
        "language",
        "theme",
        "modules",
        "window",
        "wallpapers",
        "shell",
        "animations",
        "discord",
        "windowsMode",
        "windowManager",
        "zapret",
        "account",
    ];

    value
        .as_object()
        .is_some_and(|object| object.keys().any(|key| SECTIONS.contains(&key.as_str())))
}

/// Writes `value` at the dotted `path`, creating the intermediate objects a patch may name.
fn set_path(document: &mut Value, path: &str, value: Value) -> Result<(), MethodError> {
    let segments: Vec<&str> = path.split('.').filter(|part| !part.is_empty()).collect();
    let Some((last, parents)) = segments.split_last() else {
        return Err("'config.set' needs a non-empty path".to_owned());
    };

    let mut node = document;
    for segment in parents {
        let Value::Object(object) = node else {
            return Err(format!("'config.set' cannot descend into '{path}'"));
        };

        node = object
            .entry((*segment).to_owned())
            .or_insert_with(|| json!({}));
    }

    let Value::Object(object) = node else {
        return Err(format!("'config.set' cannot write into '{path}'"));
    };

    object.insert((*last).to_owned(), value);

    Ok(())
}

/// Deserialises a config document, naming the field on the way in.
///
/// `serde_json` alone says "invalid type: string, expected i32"; with the path added, the settings
/// page can tell the user which control it has to fix.
fn deserialize_config(document: &Value) -> Result<Config, MethodError> {
    let text = serde_json::to_string(document).map_err(|error| format!("config.set: {error}"))?;
    let mut deserializer = serde_json::Deserializer::from_str(&text);

    serde_path_to_error::deserialize::<_, Config>(&mut deserializer)
        .map_err(|error| format!("config.set: {}: {}", error.path(), error.inner()))
}

/// Fills in the fields the settings window must never write as nonsense.
///
/// The rules mirror what `neb-shell` itself does at read time (`ShellSettings::height`, `theme`),
/// so the file the user keeps is the file the bar obeys, not one that only happens to be clamped at
/// paint time.
fn normalise(config: &mut Config) {
    if config.schema_version == 0 {
        config.schema_version = SCHEMA_VERSION;
    }

    let language = config.language.trim();
    config.language = if language.is_empty() {
        DEFAULT_LANGUAGE.to_owned()
    } else {
        language.to_owned()
    };

    config.shell.bar_height = config.shell.height();
    config.shell.bar_position = if config.shell.is_bottom() {
        "bottom".to_owned()
    } else {
        "top".to_owned()
    };

    let theme_id = config.shell.theme_id.trim();
    config.shell.theme_id = if theme_id.is_empty() {
        DEFAULT_THEME_ID.to_owned()
    } else {
        theme_id.to_owned()
    };

    config.shell.profile_name = config
        .shell
        .profile_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_owned);

    config
        .shell
        .pinned_apps
        .retain(|path| !path.trim().is_empty());
    for path in &mut config.shell.pinned_apps {
        *path = path.trim().to_owned();
    }

    let window = &mut config.window;
    window.width = sane(window.width, crate::window::DEFAULT_WIDTH);
    window.height = sane(window.height, crate::window::DEFAULT_HEIGHT);
    window.left = window.left.filter(|value| value.is_finite());
    window.top = window.top.filter(|value| value.is_finite());

    let wallpapers = &mut config.wallpapers;
    if !crate::themes::is_hex_color(&wallpapers.color) {
        wallpapers.color = neb_core::config::DEFAULT_WALLPAPER_COLOR.to_owned();
    }
    wallpapers.rotate_minutes = wallpapers.rotate_minutes.clamp(1, 24 * 60);
    wallpapers.playlist.retain(|entry| !entry.trim().is_empty());
}

/// A finite, positive measurement, or `fallback`.
fn sane(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

/// `themes.list`: the shipped palettes, then the user's files.
fn list_themes(context: &Context) -> Value {
    let config = context.store.load();
    let shipped = themes::built_in();
    let themes: Vec<Value> = themes::list(&context.themes_dir)
        .into_iter()
        .map(|theme| {
            // A user file may reuse a shipped id; then it is the user's palette, not the build's.
            let built_in = theme.is_shipped(&shipped);
            let mut value = serde_json::to_value(&theme).unwrap_or(Value::Null);
            if let Value::Object(object) = &mut value {
                object.insert("builtIn".to_owned(), Value::Bool(built_in));
                // The dashboard's reader names the corner radius `radius`; the file keeps the C#
                // spelling. Both travel so an older and a newer bundle read the same file.
                if let Some(radius) = object.get("cornerRadius").cloned() {
                    object.insert("radius".to_owned(), radius);
                }
            }
            value
        })
        .collect();

    json!({
        "themes": themes,
        "active": config.shell.theme(),
        "dir": context.themes_dir.display().to_string(),
    })
}

/// `themes.save`: validates the document, then writes it next to the shell's other themes.
fn save_theme(context: &Context, params: &Value) -> Result<Value, MethodError> {
    let document = match params.get("theme") {
        Some(theme) => theme,
        None => params,
    };

    let theme: ThemeDoc = themes::parse_document(&flatten_theme(document), "")
        .map_err(|error| format!("themes.save: {error}"))?;

    let path = themes::save(&context.themes_dir, &theme)
        .map_err(|error| format!("cannot save the theme: {error}"))?;

    // Saving a palette in the Appearance screen is also choosing it: the bar reloads this id.
    let mut config = context
        .store
        .load_checked()
        .map_err(|error| format!("cannot read the config: {error}"))?;
    config.shell.theme_id = theme.id.clone();
    context
        .store
        .save(&config)
        .map_err(|error| format!("cannot save the config: {error}"))?;

    Ok(json!({
        "saved": path.display().to_string(),
        "theme": theme,
        "active": config.shell.theme_id,
    }))
}

/// Flattens the dashboard's theme document (`{colors: {...}, radius}`) into the on-disk shape.
///
/// The C# theme store and this crate's files keep the seven colours at the top level; the editor
/// groups them under `colors` and names the corner radius `radius`. Both spellings are accepted so
/// the same save button works against either host.
fn flatten_theme(document: &Value) -> Value {
    let Some(object) = document.as_object() else {
        return document.clone();
    };

    let Some(colors) = object.get("colors").and_then(Value::as_object) else {
        return document.clone();
    };

    let mut flat = object.clone();
    for (key, value) in colors {
        flat.entry(key.clone()).or_insert_with(|| value.clone());
    }

    if !flat.contains_key("cornerRadius")
        && let Some(radius) = object.get("radius")
    {
        flat.insert("cornerRadius".to_owned(), radius.clone());
    }

    Value::Object(flat)
}

/// Calls the shell and returns its state document.
fn shell_call(context: &Context, method: &str) -> Result<Value, MethodError> {
    neb_core::ipc::call(&context.shell_pipe, method, None, context.timeout)
        .map_err(|error| format!("{method}: {error}"))
}

/// `shell.getState`: the shell's own readout plus the presence flags the Overview needs.
///
/// A missing shell is not an error here: the dashboard has to paint "stopped" for the bar while
/// still saying which modules are up, so the document always has the documented keys.
fn shell_state(context: &Context) -> Value {
    let config = context.store.load();
    let supervisor = supervisor_state(context, PRESENCE_TIMEOUT);
    let presence = presence_of(context, supervisor.as_ref());

    let mut document =
        match neb_core::ipc::call(&context.shell_pipe, "shell.getState", None, context.timeout) {
            Ok(Value::Object(object)) => Value::Object(object),
            Ok(other) => json!({ "state": other }),
            Err(_) => json!({}),
        };

    let object = document
        .as_object_mut()
        .expect("the document above is always an object");

    object.insert("running".to_owned(), json!(presence.shell.running));
    object.insert("profile".to_owned(), json!(config.shell.profile_name));
    object.insert("shell".to_owned(), json!(presence.shell.running));
    object.insert("wallpaper".to_owned(), json!(presence.wallpaper.running));
    object.insert("zapret".to_owned(), json!(presence.zapret));
    object.insert("discord".to_owned(), json!(presence.discord));
    object.insert("wm".to_owned(), json!(presence.wm));
    object.insert("windowsMode".to_owned(), json!(presence.windows_mode));
    object.insert("modules".to_owned(), module_flags(&config));

    document
}

/// The `modules` object the dashboard reads (`bar`, `dock`, `launcher`, `wallpaper`).
fn module_flags(config: &Config) -> Value {
    json!({
        "bar": config.shell.bar_enabled,
        "dock": config.shell.dock_enabled,
        "launcher": config.shell.launcher_enabled,
        "wallpaper": config.wallpapers.enabled,
    })
}

/// `updates.check`: asks the supervisor for its state.
///
/// The supervisor has no update-check IPC method yet (the tray owns that worker), so this answers
/// with what the supervisor does publish: its child state and uptime, next to our own version.
fn updates(context: &Context) -> Result<Value, MethodError> {
    let supervisor = supervisor_call(context, "supervisor.getState", None, context.timeout)?;

    Ok(json!({
        "current": VERSION,
        "supervisor": supervisor,
    }))
}

/// `updates.whatsNew`: the notes the updater wrote for the version it downloaded.
fn whats_new(context: &Context) -> Result<Value, MethodError> {
    let path = context.root()?.join("updates").join("whats-new.json");

    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(error) => {
            return Err(format!("cannot read '{}': {error}", path.display()));
        }
    };

    // The updater and the C# build both write UTF-8; a BOM would make serde refuse the file.
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    serde_json::from_str(text).map_err(|error| {
        format!(
            "updates.whatsNew: '{}' is not valid json: {error}",
            path.display()
        )
    })
}

/// `logs.tail`: the last [`TAIL_LINES`] lines of the newest `logs\neb-*.log`.
fn tail_logs(context: &Context) -> Result<Value, MethodError> {
    let directory = context.root()?.join("logs");

    let entries = match std::fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(json!({
                "file": Value::Null,
                "path": directory.display().to_string(),
                "lines": [],
            }));
        }
        Err(error) => {
            return Err(format!("cannot list '{}': {error}", directory.display()));
        }
    };

    let mut newest: Option<(SystemTime, PathBuf)> = None;

    for entry in entries {
        let Ok(entry) = entry else {
            continue;
        };
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();

        if !name.starts_with("neb-") || !name.ends_with(".log") || !path.is_file() {
            continue;
        }

        let modified = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);

        if newest.as_ref().is_none_or(|(best, _)| modified > *best) {
            newest = Some((modified, path));
        }
    }

    let Some((_, path)) = newest else {
        return Ok(json!({
            "file": Value::Null,
            "path": directory.display().to_string(),
            "lines": [],
        }));
    };

    let bytes = std::fs::read(&path)
        .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text.lines().filter(|line| !line.is_empty()).collect();
    let start = lines.len().saturating_sub(TAIL_LINES);

    Ok(json!({
        "file": path.file_name().map(|name| name.to_string_lossy().into_owned()),
        "path": path.display().to_string(),
        "lines": lines[start..],
    }))
}

/// `wallpaper.getSettings`: the `wallpapers` section, plus whether the process is up.
fn wallpaper_settings(context: &Context) -> Result<Value, MethodError> {
    let config = context
        .store
        .load_checked()
        .map_err(|error| format!("cannot read the config: {error}"))?;

    Ok(wallpaper_document(
        &config.wallpapers,
        wallpaper_running(context),
    ))
}

/// The dashboard's `WallpaperSettings`: the config fields with a read-only `running` next to them.
fn wallpaper_document(settings: &neb_core::config::WallpaperSettings, running: bool) -> Value {
    let mut document = serde_json::to_value(settings).unwrap_or(Value::Null);

    if let Value::Object(object) = &mut document {
        object.insert("running".to_owned(), json!(running));
    }

    document
}

/// Whether the wallpaper process answers right now.
fn wallpaper_running(context: &Context) -> bool {
    neb_core::ipc::call(
        &context.wallpaper_pipe,
        "wallpaper.getState",
        None,
        PRESENCE_TIMEOUT,
    )
    .is_ok()
}

/// Keys `wallpaper.setSettings` accepts, which is exactly what the UI can paint.
const WALLPAPER_FIELDS: &[&str] = &[
    "enabled",
    "kind",
    "path",
    "color",
    "playlist",
    "rotateMinutes",
    "pauseOnFullscreen",
    "pauseOnBattery",
];

/// `wallpaper.setSettings`: one partial patch, validated, persisted, then applied.
///
/// A patch that changes what the process should draw restarts it, because `neb-wallpaper` reads its
/// settings once at start. A patch made while the feature is off only writes the file: nothing is
/// started behind the user's back.
fn set_wallpaper_settings(context: &Context, params: &Value) -> Result<Value, MethodError> {
    let patch = params.get("settings").unwrap_or(params);
    let Value::Object(patch) = patch else {
        return Err("'wallpaper.setSettings' needs an object".to_owned());
    };

    for key in patch.keys() {
        if !WALLPAPER_FIELDS.contains(&key.as_str()) {
            return Err(format!(
                "'wallpaper.setSettings' refuses unknown field '{key}'"
            ));
        }
    }

    let mut config = context
        .store
        .load_checked()
        .map_err(|error| format!("cannot read the config: {error}"))?;
    let previous = config.wallpapers.clone();

    let mut document = serde_json::to_value(&previous)
        .map_err(|error| format!("wallpaper.setSettings: {error}"))?;
    let Value::Object(fields) = &mut document else {
        return Err("wallpaper.setSettings: the wallpaper section is not an object".to_owned());
    };

    for (key, value) in patch {
        fields.insert(
            key.clone(),
            if key == "path" && value.as_str().is_some_and(str::is_empty) {
                Value::Null
            } else {
                value.clone()
            },
        );
    }

    config.wallpapers = serde_json::from_value(document)
        .map_err(|error| format!("wallpaper.setSettings: {error}"))?;

    normalise(&mut config);

    context
        .store
        .save(&config)
        .map_err(|error| format!("cannot save the config: {error}"))?;

    let changed = config.wallpapers != previous;
    let restart = changed && (config.wallpapers.enabled || previous.enabled);

    let running = if restart {
        restart_child(context, "wallpaper")
            .and_then(|answer| {
                answer
                    .get("running")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| {
                        "the supervisor did not say whether the wallpaper runs".to_owned()
                    })
            })
            .unwrap_or_else(|_| wallpaper_running(context))
    } else {
        wallpaper_running(context)
    };

    let mut document = wallpaper_document(&config.wallpapers, running);
    if let Value::Object(object) = &mut document {
        object.insert("restarted".to_owned(), json!(restart));
    }

    Ok(document)
}

/// `wallpaper.restart`: stops and starts the wallpaper through the supervisor.
fn restart_wallpaper(context: &Context) -> Result<Value, MethodError> {
    let answer = restart_child(context, "wallpaper")?;
    let running = answer
        .get("running")
        .and_then(Value::as_bool)
        .unwrap_or_else(|| wallpaper_running(context));

    Ok(json!({
        "ok": true,
        "name": "wallpaper",
        "running": running,
        "supervisor": answer,
    }))
}

/// `wallpaper.pickFile`: the host's own file dialog.
fn pick_wallpaper(context: &Context) -> Result<Value, MethodError> {
    let path =
        (context.picker)(context.window).map_err(|error| format!("wallpaper.pickFile: {error}"))?;

    Ok(json!({ "path": path }))
}

/// `app.openChangelog`: opens the documented page in the user's browser.
fn open_changelog(context: &Context) -> Result<Value, MethodError> {
    (context.opener)(CHANGELOG_URL)
        .map_err(|error| format!("cannot open the changelog: {error}"))?;

    Ok(json!({ "ok": true, "url": CHANGELOG_URL }))
}

/// `app.openLogsFolder`: opens `logs` in Explorer, creating it when it is not there yet.
fn open_logs_folder(context: &Context) -> Result<Value, MethodError> {
    let directory = context.root()?.join("logs");

    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("cannot create '{}': {error}", directory.display()))?;

    let path = directory.display().to_string();
    (context.opener)(&path).map_err(|error| format!("cannot open '{path}': {error}"))?;

    Ok(json!({ "ok": true, "path": path }))
}

/// `app.restartShell` and the wallpaper equivalent: one `app.restartChild` on the supervisor.
fn restart_child(context: &Context, name: &str) -> Result<Value, MethodError> {
    let timeout = context.timeout.max(RESTART_TIMEOUT);

    supervisor_call(
        context,
        "app.restartChild",
        Some(json!({ "name": name })),
        timeout,
    )
}

/// `vm.*`: forwards the call to the tiling window manager.
fn windows(context: &Context, method: &str, params: &Value) -> Result<Value, MethodError> {
    let params = match method {
        "windows.set" => optional_params(params),
        "windows.getState" | "windows.restore" => None,
        other => return Err(format!("unknown method '{other}'")),
    };

    supervisor_call(context, method, params, context.timeout)
}

/// `zapret.*`: the module's own document, or an honest "off" state while the module is disabled.
fn zapret(context: &Context, method: &str, params: &Value) -> Result<Value, MethodError> {
    let target = match method {
        "zapret.status" => "zapret.getState",
        other => other,
    };

    let call = module_call(
        &context.zapret_pipe,
        target,
        optional_params(params),
        context.timeout,
    );

    match call {
        Ok(value) => Ok(value),
        Err(error) if !context.store.load().zapret.enabled => {
            let empty = match target {
                "zapret.getState" => ok(zapret_off()),
                "zapret.listStrategies" => ok(zapret_strategies_off()),
                _ => return Err(error),
            };

            Ok(empty)
        }
        Err(error) => Err(error),
    }
}

/// What `zapret.getState` answers while the module is off: nothing installed, nothing running.
fn zapret_off() -> Value {
    json!({
        "ok": true,
        "enabled": false,
        "installed": false,
        "version": Value::Null,
        "path": Value::Null,
        "strategy": Value::Null,
        "running": false,
        "lastError": Value::Null,
        "admin": Value::Null,
    })
}

/// What `zapret.listStrategies` answers while the module is off.
fn zapret_strategies_off() -> Value {
    json!({
        "ok": true,
        "enabled": false,
        "strategies": [],
        "selected": Value::Null,
    })
}

/// Rolls a success value into the same shape as the module's own answers.
fn ok(value: Value) -> Value {
    match value {
        Value::Object(mut object) => {
            object.insert("ok".to_owned(), json!(true));
            Value::Object(object)
        }
        other => other,
    }
}

/// `discord.*`: forwards to the module, translating the dashboard's editor shapes.
///
/// The module speaks the on-disk preset (`largeImage`, `startTimestamp` as Unix seconds) and rule
/// (pattern + preset) shapes; the dashboard speaks `largeImageKey` and `startTimestamp` as a flag.
/// Both translations live here so neither side has to know the other's spelling.
fn discord(context: &Context, method: &str, params: &Value) -> Result<Value, MethodError> {
    let enabled = context.store.load().discord.enabled;

    let result = match method {
        "discord.getState" => module_call(
            &context.discord_pipe,
            "discord.getState",
            None,
            context.timeout,
        ),
        "discord.connect" => connect_discord(context, params),
        "discord.disconnect" => module_call(
            &context.discord_pipe,
            "discord.disconnect",
            None,
            context.timeout,
        ),
        "discord.getPresets" | "discord.listPresets" => module_call(
            &context.discord_pipe,
            "discord.listPresets",
            None,
            context.timeout,
        )
        .map(|value| presets_for_ui(&value)),
        "discord.savePreset" => module_call(
            &context.discord_pipe,
            "discord.savePreset",
            Some(json!({ "preset": preset_for_module(params) })),
            context.timeout,
        )
        .map(|value| preset_answer_for_ui(&value)),
        "discord.setPreset" => module_call(
            &context.discord_pipe,
            "discord.setPreset",
            Some(preset_name_for_module(params)),
            context.timeout,
        ),
        "discord.getRules" => module_call(
            &context.discord_pipe,
            "discord.getRules",
            None,
            context.timeout,
        ),
        "discord.setRules" => module_call(
            &context.discord_pipe,
            "discord.setRules",
            Some(rules_for_module(params)),
            context.timeout,
        ),
        other => module_call(
            &context.discord_pipe,
            other,
            optional_params(params),
            context.timeout,
        ),
    };

    match result {
        Ok(value) => Ok(value),
        Err(error) if !enabled => match method {
            "discord.getState" => Ok(json!({
                "ok": true,
                "state": "disconnected",
                "configured": false,
                "enabled": false,
                "appId": "",
                "message": "Discord Rich Presence выключен",
                "lastError": Value::Null,
                "activePreset": Value::Null,
                "rules": 0,
            })),
            "discord.getRules" => Ok(json!({ "ok": true, "rules": [] })),
            "discord.getPresets" | "discord.listPresets" => {
                Ok(json!({ "ok": true, "presets": [], "errors": [] }))
            }
            _ => Err(error),
        },
        Err(error) => Err(error),
    }
}

/// The editor's preset, rewritten into `neb-discord`'s on-disk shape.
fn preset_for_module(params: &Value) -> Value {
    let source = params.get("preset").unwrap_or(params);
    let text = |key: &str| {
        source
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned()
    };
    let pick = |first: &str, second: &str| {
        source
            .get(first)
            .or_else(|| source.get(second))
            .cloned()
            .unwrap_or_else(|| json!(""))
    };

    let timestamp = source
        .get("startTimestamp")
        .and_then(Value::as_bool)
        .unwrap_or(false);

    json!({
        "name": text("name"),
        "details": text("details"),
        "state": text("state"),
        "largeImage": pick("largeImage", "largeImageKey"),
        "largeText": pick("largeText", "largeImageText"),
        "smallImage": pick("smallImage", "smallImageKey"),
        "smallText": pick("smallText", "smallImageText"),
        "buttons": source.get("buttons").cloned().unwrap_or_else(|| json!([])),
        "startTimestamp": if timestamp { json!(unix_now()) } else { Value::Null },
    })
}

/// A preset read from the module, rewritten for the dashboard's editor.
fn preset_for_ui(value: &Value) -> Value {
    let Value::Object(source) = value else {
        return value.clone();
    };

    let mut object = source.clone();
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();

    object.insert("id".to_owned(), json!(name));
    for (module_key, ui_key) in [
        ("largeImage", "largeImageKey"),
        ("largeText", "largeImageText"),
        ("smallImage", "smallImageKey"),
        ("smallText", "smallImageText"),
    ] {
        if !object.contains_key(ui_key) {
            let value = object.get(module_key).cloned().unwrap_or_else(|| json!(""));
            object.insert(ui_key.to_owned(), value);
        }
    }

    let timestamp = object
        .get("startTimestamp")
        .is_some_and(|value| !value.is_null());
    object.insert("startTimestamp".to_owned(), json!(timestamp));

    Value::Object(object)
}

/// `discord.listPresets`'s answer, with the presets rewritten for the editor.
fn presets_for_ui(value: &Value) -> Value {
    let presets = value
        .get("presets")
        .and_then(Value::as_array)
        .map(|presets| presets.iter().map(preset_for_ui).collect::<Vec<_>>())
        .unwrap_or_default();

    json!({
        "ok": true,
        "presets": presets,
        "errors": value.get("errors").cloned().unwrap_or_else(|| json!([])),
    })
}

/// `discord.savePreset`'s answer, with the saved preset rewritten for the editor.
fn preset_answer_for_ui(value: &Value) -> Value {
    match value.get("preset") {
        Some(preset) => {
            let mut answer = value.clone();
            if let Value::Object(object) = &mut answer {
                object.insert("preset".to_owned(), preset_for_ui(preset));
            }
            answer
        }
        None => value.clone(),
    }
}

/// `discord.setPreset`: the editor names a preset by id (its name); the module reads `name`.
fn preset_name_for_module(params: &Value) -> Value {
    let name = ["name", "preset", "id", "presetId"]
        .iter()
        .find_map(|key| params.get(*key).and_then(Value::as_str));

    json!({ "name": name })
}

/// `discord.setRules`: the editor's rules, rewritten into the module's shape.
fn rules_for_module(params: &Value) -> Value {
    let body = params.get("rules").unwrap_or(params);
    let rules = body
        .as_array()
        .map(|rules| {
            rules
                .iter()
                .filter_map(|entry| {
                    let pattern = entry.get("pattern").and_then(Value::as_str)?;
                    let preset = ["preset", "presetId"]
                        .iter()
                        .find_map(|key| entry.get(*key).and_then(Value::as_str))
                        .unwrap_or_default();
                    let enabled = entry
                        .get("enabled")
                        .and_then(Value::as_bool)
                        .unwrap_or(true);

                    Some(json!({
                        "pattern": pattern,
                        "preset": preset,
                        "enabled": enabled,
                    }))
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    json!({ "rules": rules })
}

/// Seconds since the Unix epoch, for a preset that asks to show elapsed time.
fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// A supervisor call with the pipe's absence turned into one sentence.
fn supervisor_call(
    context: &Context,
    method: &str,
    params: Option<Value>,
    timeout: Duration,
) -> Result<Value, MethodError> {
    neb_core::ipc::call(&context.supervisor_pipe, method, params, timeout)
        .map_err(|error| transport_error(method, error, SUPERVISOR_DOWN))
}

/// A module call with the pipe's absence turned into one sentence.
fn module_call(
    pipe: &str,
    method: &str,
    params: Option<Value>,
    timeout: Duration,
) -> Result<Value, MethodError> {
    neb_core::ipc::call(pipe, method, params, timeout)
        .map_err(|error| transport_error(method, error, MODULE_DOWN))
}

/// Connects Discord, adopting an application id typed into the window first.
///
/// The module reads `discord.appId` from the config at start, so a new id is persisted and the child
/// is restarted before the connect call; an unchanged id just connects.
fn connect_discord(context: &Context, params: &Value) -> Result<Value, MethodError> {
    let typed = params
        .get("appId")
        .or_else(|| params.get("applicationId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .map(str::to_owned);

    if let Some(id) = typed {
        let mut config = context.store.load();
        if config.discord.app_id != id {
            config.discord.app_id = id;
            config.discord.enabled = true;
            context
                .store
                .save(&config)
                .map_err(|error| format!("config: {error}"))?;
            let restart = serde_json::json!({ "name": "discord" });
            let _ = supervisor_call(context, "app.restartChild", Some(restart), context.timeout);
        }
    }

    module_call(
        &context.discord_pipe,
        "discord.connect",
        optional_params(params),
        context.timeout,
    )
}

/// A connection that never reached a server reads as the module being off; anything a server said
/// is kept, because it is the module's own explanation and often names the fix.
fn transport_error(method: &str, error: IpcError, down: &str) -> String {
    match error {
        IpcError::Unavailable { .. } | IpcError::Timeout { .. } | IpcError::Closed { .. } => {
            down.to_owned()
        }
        other => format!("{method}: {other}"),
    }
}

/// `params` as an optional value, so `null` and `{}` travel as "no arguments".
fn optional_params(value: &Value) -> Option<Value> {
    match value {
        Value::Null => None,
        other => Some(other.clone()),
    }
}

/// Reads the supervisor's state, or `None` when nobody answers.
fn supervisor_state(context: &Context, timeout: Duration) -> Option<Value> {
    neb_core::ipc::call(
        &context.supervisor_pipe,
        "supervisor.getState",
        None,
        timeout,
    )
    .ok()
}

/// One child's presence, as either a supervisor state document or a probe reports it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct ChildPresence {
    /// Process id, when the reporter knew one.
    pid: Option<u32>,
    /// Whether the child is alive.
    running: bool,
}

/// What is up right now, across all the ProjectNeb processes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Presence {
    /// The bar.
    shell: ChildPresence,
    /// The living wallpaper.
    wallpaper: ChildPresence,
    /// The DPI-bypass module.
    zapret: bool,
    /// Discord Rich Presence.
    discord: bool,
    /// The tiling window manager.
    wm: bool,
    /// Whether the Windows replacement mode is on in the config.
    windows_mode: bool,
}

/// Reads a child from a supervisor document: the `children` map first, then the older top-level key.
fn child_presence(supervisor: Option<&Value>, name: &str) -> ChildPresence {
    let Some(state) = supervisor else {
        return ChildPresence::default();
    };

    let node = state
        .get("children")
        .and_then(|children| children.get(name))
        .or_else(|| state.get(name));

    let Some(node) = node else {
        return ChildPresence::default();
    };

    ChildPresence {
        pid: node
            .get("pid")
            .and_then(Value::as_u64)
            .and_then(|pid| u32::try_from(pid).ok()),
        running: node
            .get("running")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    }
}

/// What is up, preferring the supervisor's own view and probing the pipes only when it is absent.
fn presence_of(context: &Context, supervisor: Option<&Value>) -> Presence {
    let mut presence = Presence {
        windows_mode: context.store.load().windows_mode.enabled,
        ..Presence::default()
    };

    if let Some(state) = supervisor {
        presence.shell = child_presence(Some(state), "shell");
        presence.wallpaper = child_presence(Some(state), "wallpaper");
        presence.zapret = child_presence(Some(state), "zapret").running;
        presence.discord = child_presence(Some(state), "discord").running;
        presence.wm = child_presence(Some(state), "wm").running;

        return presence;
    }

    presence.shell = ChildPresence {
        pid: None,
        running: neb_core::ipc::call(&context.shell_pipe, "shell.ping", None, PRESENCE_TIMEOUT)
            .is_ok(),
    };

    presence.wallpaper = probe_wallpaper(context);
    presence.zapret =
        neb_core::ipc::call(&context.zapret_pipe, "zapret.ping", None, PRESENCE_TIMEOUT).is_ok();
    presence.discord = neb_core::ipc::call(
        &context.discord_pipe,
        "discord.getState",
        None,
        PRESENCE_TIMEOUT,
    )
    .is_ok();
    presence.wm =
        neb_core::ipc::call(&context.wm_pipe, "wm.getState", None, PRESENCE_TIMEOUT).is_ok();

    presence
}

/// The wallpaper's presence, from its own answer.
fn probe_wallpaper(context: &Context) -> ChildPresence {
    match neb_core::ipc::call(
        &context.wallpaper_pipe,
        "wallpaper.getState",
        None,
        PRESENCE_TIMEOUT,
    ) {
        Ok(state) => ChildPresence {
            pid: state
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
            running: true,
        },
        Err(_) => ChildPresence::default(),
    }
}

/// `neb.metrics`: the machine, then the processes the dashboard shows.
fn metrics_for(
    context: &Context,
    sample: &neb_core::metrics::Sample,
    supervisor: Option<&Value>,
) -> Value {
    let (shell, wallpaper, supervisor_pid) = match supervisor {
        Some(state) => (
            child_presence(Some(state), "shell"),
            child_presence(Some(state), "wallpaper"),
            state
                .get("pid")
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok()),
        ),
        None => (ChildPresence::default(), probe_wallpaper(context), None),
    };

    json!({
        "cpu": f64::from(sample.cpu),
        "ram": f64::from(sample.ram_percent()),
        "ramUsedMb": round(sample.ram_used_gb * 1024.0),
        "ramTotalMb": round(sample.ram_total_gb * 1024.0),
        "shell": process_metrics(shell),
        "wallpaper": process_metrics(wallpaper),
        "supervisor": process_only(supervisor_pid),
        "app": process_only(Some(std::process::id())),
    })
}

/// One process as the dashboard's memory card reads it.
///
/// `mb` is the documented field; `memoryMb` is the same number under the name older dashboard
/// builds read, so either bundle paints it.
fn process_metrics(presence: ChildPresence) -> Value {
    let bytes = presence
        .pid
        .and_then(neb_core::metrics::working_set_of_process);
    let mb = bytes.map(bytes_to_mb);

    json!({
        "pid": presence.pid,
        "running": presence.running,
        "mb": mb,
        "memoryMb": mb,
        "workingSetBytes": bytes,
    })
}

/// A process the dashboard only shows a memory number for.
fn process_only(pid: Option<u32>) -> Value {
    let bytes = pid.and_then(neb_core::metrics::working_set_of_process);
    let mb = bytes.map(bytes_to_mb);

    json!({
        "pid": pid,
        "mb": mb,
        "memoryMb": mb,
    })
}

/// Bytes as mebibytes, one decimal, the unit the cards show.
fn bytes_to_mb(bytes: usize) -> f64 {
    round(bytes as f64 / (1024.0 * 1024.0))
}

/// One decimal place, so the JSON does not carry a float's whole mantissa.
fn round(value: f64) -> f64 {
    (value * 10.0).round() / 10.0
}

/// `neb.modules`: the switches the Overview and the sidebar read, straight from the config.
fn modules_for(context: &Context, supervisor: Option<&Value>) -> Value {
    let config = context.store.load();
    let shell = child_presence(supervisor, "shell");

    json!({
        "wallpapers": config.wallpapers.enabled,
        "zapret": config.zapret.enabled,
        "windhawk": config.modules.windhawk,
        "discordRp": config.discord.enabled,
        "wm": config.window_manager.enabled,
        "running": shell.running,
        "profile": config.shell.profile_name,
        "modules": module_flags(&config),
    })
}

/// One of the two live documents, as the page receives it.
fn push_message(kind: &str, payload: Value) -> String {
    json!({
        "type": kind,
        "id": Value::Null,
        "ok": true,
        "payload": payload,
        "error": Value::Null,
    })
    .to_string()
}

/// Posts both live documents: the sampler advances once, the supervisor is asked once.
fn push<F>(context: &Context, sampler: &mut Sampler, respond: &F)
where
    F: Fn(String),
{
    let supervisor = supervisor_state(context, PRESENCE_TIMEOUT);
    let sample = sampler.sample();

    respond(push_message(
        METRICS_TYPE,
        metrics_for(context, &sample, supervisor.as_ref()),
    ));
    respond(push_message(
        MODULES_TYPE,
        modules_for(context, supervisor.as_ref()),
    ));
}

/// `window.*`: plain posted messages, answered with nothing.
fn window_message(context: &Context, message: u32) -> Result<Value, MethodError> {
    let hwnd = context
        .window
        .ok_or_else(|| "there is no window to act on".to_owned())?;

    crate::window::post(hwnd, message)
        .map_err(|error| format!("cannot post window message 0x{message:04X}: {error}"))?;

    Ok(Value::Null)
}

/// One command the bridge worker accepts: a call to answer, or the page announcing it is listening.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// A method call from the page.
    Invoke(Job),
    /// `neb.ui.ready`: push the live documents now instead of at the next tick.
    Ready,
}

/// A worker that turns jobs into replies and pushes the live documents off the UI thread.
///
/// `respond` is the WebView's `PostWebMessageAsJson`; it must be callable from this thread, which
/// WebView2 allows because the controller is agile. Keeping the dispatch here means a two-second IPC
/// timeout never freezes the page.
/// # Errors
///
/// [`std::io::Error`] when the thread cannot be started; the caller reports it and keeps the window.
pub fn spawn_worker<F>(context: Context, respond: F) -> std::io::Result<Sender<Command>>
where
    F: Fn(String) + Send + 'static,
{
    let (sender, receiver) = mpsc::channel::<Command>();

    std::thread::Builder::new()
        .name("neb-app-bridge".to_owned())
        .spawn(move || run_worker(context, receiver, respond))?;

    Ok(sender)
}

/// The worker loop: one job in, one message out, in order, with a push every second.
fn run_worker<F>(context: Context, receiver: Receiver<Command>, respond: F)
where
    F: Fn(String),
{
    let mut sampler = Sampler::new();

    loop {
        match receiver.recv_timeout(PUSH_INTERVAL) {
            Ok(Command::Invoke(job)) => {
                let reply = dispatch(&context, &job);

                if reply.ok {
                    context.log(Level::Debug, &format!("{} -> ok", job.method));
                } else {
                    context.log(
                        Level::Warn,
                        &format!(
                            "{} -> {}",
                            job.method,
                            reply.error.as_deref().unwrap_or("failed")
                        ),
                    );
                }

                respond(reply.to_message());
            }
            Ok(Command::Ready) => push(&context, &mut sampler, &respond),
            Err(RecvTimeoutError::Timeout) => push(&context, &mut sampler, &respond),
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }

    context.log(Level::Debug, "worker stopped");
}

// ---------- account / licence ----------

/// Name this installation reports as a licence device.
fn default_device_name() -> String {
    std::env::var("COMPUTERNAME")
        .ok()
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Windows PC".to_owned())
}

/// `account.*`: the sign-in flow, the local state and the explicit refresh.
fn account(context: &Context, method: &str) -> Result<Value, MethodError> {
    match method {
        "account.getState" => account_state(context),
        "account.signIn" => account_sign_in(context),
        "account.signOut" => account_sign_out(context),
        "account.refresh" => account_refresh(context),
        other => Err(format!("unknown method '{other}'")),
    }
}

/// The licence file next to the config, so a test root is a complete account sandbox.
fn licence_store(context: &Context) -> Result<store::LicenseStore, MethodError> {
    Ok(store::LicenseStore::new(
        context.root()?.join("license.json"),
    ))
}

fn account_api(context: &Context, config: &Config) -> Result<account_api::AccountApi, MethodError> {
    let base = config.account.api_base.trim();
    if base.is_empty() {
        return Err("ProjectNeb Pro не настроен: укажите account.apiBase в config.json".to_owned());
    }

    Ok(account_api::AccountApi::new(
        base,
        context.root()?.join("cache").join("jwks.json"),
    ))
}

/// Reads the record, if the file exists.
fn licence_record(
    store: &store::LicenseStore,
) -> Result<Option<store::StoredLicense>, MethodError> {
    store.load().map_err(|error| error.to_string())
}

/// `account.getState`: the local view only; no network call is made here.
fn account_state(context: &Context) -> Result<Value, MethodError> {
    let config = context
        .store
        .load_checked()
        .map_err(|error| format!("cannot read the config: {error}"))?;
    let record = licence_record(&licence_store(context)?)?;

    Ok(account_api::state_document(
        &config.account.api_base,
        record.as_ref(),
        account_api::now_seconds(),
    ))
}

/// Mints and persists the device id on first use, so one installation is one device.
fn ensure_device_id(
    context: &Context,
    config: &mut Config,
) -> Result<(String, String), MethodError> {
    if config.account.device_id.trim().is_empty() {
        config.account.device_id = ids::random_uuid().map_err(|error| error.to_string())?;
        context
            .store
            .save(config)
            .map_err(|error| format!("cannot save the device id: {error}"))?;
    }

    Ok((
        config.account.device_id.clone(),
        context.device_name.clone(),
    ))
}

/// `account.signIn`: loopback listener, browser, verified session, stored entitlement.
fn account_sign_in(context: &Context) -> Result<Value, MethodError> {
    let mut config = context
        .store
        .load_checked()
        .map_err(|error| format!("cannot read the config: {error}"))?;
    let api = account_api(context, &config)?;
    let (device_id, device_name) = ensure_device_id(context, &mut config)?;

    let listener = loopback::Loopback::bind().map_err(|error| error.to_string())?;
    let state = ids::random_token(24).map_err(|error| error.to_string())?;
    let return_url = listener.callback_url(&state);
    let start_url = api.start_url(&return_url);

    (context.opener)(&start_url)
        .map_err(|error| format!("cannot open the browser for sign-in: {error}"))?;

    context.log(
        Level::Info,
        &format!("account.signIn: waiting on 127.0.0.1:{}", listener.port()),
    );

    let session = listener
        .wait(&state, context.sign_in_timeout)
        .map_err(|error| error.to_string())?;

    let now = account_api::now_seconds();
    let session_claims = api
        .verify_session(&session, now)
        .map_err(|error| format!("the session token did not verify: {error}"))?;

    let store = licence_store(context)?;
    let mut record = licence_record(&store)?.unwrap_or_default();

    let me = api.me(&session);
    let mut profile = me
        .as_ref()
        .ok()
        .map(|me| me.profile.clone())
        .unwrap_or_else(|| store::Profile {
            sub: Some(session_claims.sub.clone()),
            name: None,
            avatar: None,
        });
    if profile.sub.is_none() {
        profile.sub = Some(session_claims.sub.clone());
    }

    record.apply_sign_in(&session, profile, now);

    match api.refresh(&session, &device_id, &device_name) {
        Ok((token, claims)) => record.apply_entitlement(&token, &claims, now),
        Err(error) => {
            // A free account is a successful sign-in too; the worker refuses a refresh with
            // `upgrade_required`. Any other failure leaves the account signed in but without an
            // entitlement, which is honest and recoverable with «Обновить права».
            record.clear_entitlement();
            context.log(
                Level::Warn,
                &format!("account.signIn: no entitlement: {error}"),
            );
        }
    }

    store.save(&record).map_err(|error| error.to_string())?;

    Ok(account_api::state_document(
        &config.account.api_base,
        Some(&record),
        now,
    ))
}

/// `account.signOut`: wipe the stored tokens.
fn account_sign_out(context: &Context) -> Result<Value, MethodError> {
    let store = licence_store(context)?;
    store.wipe().map_err(|error| error.to_string())?;

    let config = context.store.load();
    Ok(account_api::state_document(
        &config.account.api_base,
        None,
        account_api::now_seconds(),
    ))
}

/// `account.refresh`: ask the worker for a new entitlement with the stored session.
fn account_refresh(context: &Context) -> Result<Value, MethodError> {
    let mut config = context
        .store
        .load_checked()
        .map_err(|error| format!("cannot read the config: {error}"))?;
    let api = account_api(context, &config)?;

    let store = licence_store(context)?;
    let mut record = licence_record(&store)?
        .ok_or_else(|| "вы не вошли в аккаунт / not signed in".to_owned())?;
    let session = record
        .session
        .clone()
        .filter(|token| !token.is_empty())
        .ok_or_else(|| "вы не вошли в аккаунт / not signed in".to_owned())?;

    let (device_id, device_name) = ensure_device_id(context, &mut config)?;
    let now = account_api::now_seconds();

    let (token, claims) = api
        .refresh(&session, &device_id, &device_name)
        .map_err(|error| error.to_string())?;

    record.apply_entitlement(&token, &claims, now);
    store.save(&record).map_err(|error| error.to_string())?;

    Ok(account_api::state_document(
        &config.account.api_base,
        Some(&record),
        now,
    ))
}

/// `license.getState`: what other modules gate on, read-only and network-free.
fn license_state(context: &Context) -> Result<Value, MethodError> {
    let store = licence_store(context)?;
    let record = licence_record(&store)?;

    Ok(account_api::license_document(
        record.as_ref(),
        account_api::now_seconds(),
    ))
}

// ---------- the platform edges ----------

/// Opens `target` with the shell's default handler.
fn shell_open(target: &str) -> Result<(), String> {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    // The operating system only reads the strings during the call, so they live on this stack.
    let operation = wide("open");
    let file = wide(target);

    // SAFETY: both strings are NUL-terminated and outlive the call; no window is needed for a URL
    // or a folder, and the returned value is a shell status code rather than an owned handle.
    let code = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    let status = code.0 as isize;
    if status > 32 {
        Ok(())
    } else {
        Err(format!("ShellExecuteW returned {status}"))
    }
}

/// Asks for a wallpaper file with the classic open dialog.
fn pick_wallpaper_file(window: Option<isize>) -> Result<Option<String>, String> {
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR,
        OFN_PATHMUSTEXIST, OPENFILENAMEW,
    };

    const FILTER: &str = "Изображения и видео\0\
*.png;*.jpg;*.jpeg;*.webp;*.bmp;*.gif;*.avif;*.mp4;*.webm;*.mov;*.m4v;*.mkv;*.avi;*.html;*.htm\0\
Все файлы\0*.*\0\0";

    let mut buffer = [0u16; 1024];
    let mut filter = wide(FILTER);
    let title = wide("Выберите обои / Choose a wallpaper");

    let mut options = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: HWND(window.unwrap_or_default() as *mut _),
        lpstrFilter: windows::core::PCWSTR(filter.as_mut_ptr()),
        nFilterIndex: 1,
        lpstrFile: windows::core::PWSTR(buffer.as_mut_ptr()),
        nMaxFile: buffer.len() as u32,
        lpstrTitle: windows::core::PCWSTR(title.as_ptr()),
        Flags: OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR,
        ..OPENFILENAMEW::default()
    };

    // SAFETY: every pointer in `options` refers to a live local buffer or string that outlives the
    // call, and the structure declares its own size.
    let chosen = unsafe { GetOpenFileNameW(&mut options) };
    if chosen.as_bool() {
        let length = buffer
            .iter()
            .position(|unit| *unit == 0)
            .unwrap_or(buffer.len());

        return Ok(Some(String::from_utf16_lossy(&buffer[..length])));
    }

    // SAFETY: reads the dialog's thread-local error, which is zero when the user simply cancelled.
    let error = unsafe { CommDlgExtendedError() }.0;
    if error == 0 {
        Ok(None)
    } else {
        Err(format!("the file dialog failed (error 0x{error:08X})"))
    }
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use neb_core::ipc::{Request, Response};
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn context() -> (tempfile::TempDir, Context) {
        let dir = tempfile::tempdir().expect("temp dir");
        let context = Context::for_test(dir.path());
        (dir, context)
    }

    fn job(method: &str, params: Value) -> Job {
        Job {
            id: "1".to_owned(),
            method: method.to_owned(),
            params,
        }
    }

    static NEXT_PIPE: AtomicUsize = AtomicUsize::new(0);

    /// Serves `handler` on a private pipe and returns its name.
    fn stub<H>(tag: &str, handler: H) -> (String, neb_core::ipc::ServerHandle)
    where
        H: Fn(Request) -> Response + Send + Sync + 'static,
    {
        let pipe = format!(
            "ProjectNeb.bridge.test.{tag}.{}.{}",
            std::process::id(),
            NEXT_PIPE.fetch_add(1, Ordering::SeqCst)
        );

        let handle = neb_core::ipc::serve(&pipe, handler).expect("the stub pipe starts");
        (pipe, handle)
    }

    /// A stub that answers every call with one document.
    fn answering(result: Value) -> impl Fn(Request) -> Response + Send + Sync + 'static {
        move |request: Request| Response::success(request.id, result.clone())
    }

    #[test]
    fn the_nested_message_shape_is_understood() {
        let inbound = parse_message(
            r#"{"type":"neb.invoke","id":"7","payload":{"method":"config.get","params":{}}}"#,
        );

        assert_eq!(
            inbound,
            Inbound::Invoke(Job {
                id: "7".to_owned(),
                method: "config.get".to_owned(),
                params: json!({}),
            })
        );
    }

    #[test]
    fn the_flat_message_shape_the_plan_names_is_understood() {
        let inbound =
            parse_message(r#"{"type":"neb.invoke","id":"2","method":"app.version","params":null}"#);

        assert_eq!(
            inbound,
            Inbound::Invoke(Job {
                id: "2".to_owned(),
                method: "app.version".to_owned(),
                params: Value::Null,
            })
        );
    }

    #[test]
    fn a_numeric_id_is_answered_on_the_same_value() {
        let inbound = parse_message(r#"{"type":"neb.invoke","id":42,"method":"app.version"}"#);

        match inbound {
            Inbound::Invoke(job) => assert_eq!(job.id, "42"),
            other => panic!("expected an invoke, got {other:?}"),
        }
    }

    #[test]
    fn malformed_json_is_reported_not_panicked_on() {
        match parse_message("{not json") {
            Inbound::Malformed { id, reason } => {
                assert_eq!(id, None);
                assert!(reason.contains("not valid json"), "{reason}");
            }
            other => panic!("expected malformed, got {other:?}"),
        }

        for text in ["[1,2,3]", "\"hello\"", "null"] {
            assert!(
                matches!(parse_message(text), Inbound::Malformed { .. }),
                "{text} must be refused"
            );
        }
    }

    #[test]
    fn a_broken_invoke_keeps_the_id_so_the_page_can_settle() {
        match parse_message(r#"{"type":"neb.invoke","id":"9","payload":{}}"#) {
            Inbound::Malformed { id, reason } => {
                assert_eq!(id.as_deref(), Some("9"));
                assert!(reason.contains("method"), "{reason}");
            }
            other => panic!("expected malformed, got {other:?}"),
        }
    }

    #[test]
    fn a_message_without_an_id_cannot_be_answered() {
        assert!(matches!(
            parse_message(r#"{"type":"neb.invoke","payload":{"method":"config.get"}}"#),
            Inbound::Malformed { id: None, .. }
        ));
    }

    #[test]
    fn the_ready_message_is_its_own_inbound_kind() {
        assert_eq!(parse_message(r#"{"type":"neb.ui.ready"}"#), Inbound::Ready);
    }

    #[test]
    fn a_message_of_another_type_is_ignored() {
        assert_eq!(
            parse_message(r#"{"type":"neb.metrics","payload":{}}"#),
            Inbound::Ignored
        );
    }

    #[test]
    fn an_unknown_method_is_refused_by_name() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("config.explode", Value::Null));

        assert!(!reply.ok);
        assert!(
            reply
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("config.explode")
        );
        assert!(
            !reply.to_message().contains("payload\":{"),
            "the failure carries no payload"
        );
    }

    #[test]
    fn config_get_answers_with_the_defaults_of_a_fresh_profile() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("config.get", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["language"], json!(DEFAULT_LANGUAGE));
        assert_eq!(reply.payload["shell"]["themeId"], json!(DEFAULT_THEME_ID));
        assert_eq!(
            reply.payload["shell"]["wallpaperEnabled"],
            json!(false),
            "the dashboard's alias mirrors wallpapers.enabled"
        );
        assert!(
            !context.store.path().exists(),
            "reading must not create the file"
        );
    }

    #[test]
    fn config_set_round_trips_through_the_store() {
        let (_dir, context) = context();
        let mut document = serde_json::to_value(Config::default()).expect("render");
        document["shell"]["barHeight"] = json!(56);
        document["language"] = json!("en");

        let reply = dispatch(&context, &job("config.set", json!({ "config": document })));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["shell"]["barHeight"], json!(56));

        let saved = context.store.load_checked().expect("readable");
        assert_eq!(saved.shell.bar_height, 56);
        assert_eq!(saved.language, "en");
    }

    #[test]
    fn config_set_accepts_a_dotted_path_patch() {
        let (_dir, context) = context();

        let language = dispatch(
            &context,
            &job("config.set", json!({ "path": "language", "value": "en" })),
        );
        assert!(language.ok, "{language:?}");
        assert_eq!(language.payload["language"], json!("en"));

        let height = dispatch(
            &context,
            &job(
                "config.set",
                json!({ "path": "shell.barHeight", "value": 50 }),
            ),
        );
        assert!(height.ok, "{height:?}");
        assert_eq!(height.payload["shell"]["barHeight"], json!(50));

        let saved = context.store.load_checked().expect("readable");
        assert_eq!(saved.language, "en");
        assert_eq!(saved.shell.bar_height, 50);
        assert_eq!(
            saved.shell.theme_id, DEFAULT_THEME_ID,
            "a patch must not wipe the fields it did not name"
        );
    }

    #[test]
    fn config_set_accepts_a_section_patch_and_keeps_unknown_sections() {
        let (_dir, context) = context();

        let reply = dispatch(
            &context,
            &job(
                "config.set",
                json!({ "section": "updates", "values": { "channel": "stable", "autoUpdate": false } }),
            ),
        );
        assert!(reply.ok, "{reply:?}");

        let reply = dispatch(
            &context,
            &job("config.set", json!({ "path": "language", "value": "en" })),
        );
        assert!(reply.ok, "{reply:?}");

        let saved: Value = serde_json::from_slice(
            &std::fs::read(context.store.path()).expect("the config is written"),
        )
        .expect("json");
        assert_eq!(saved["updates"]["channel"], json!("stable"));
        assert_eq!(saved["updates"]["autoUpdate"], json!(false));
        assert_eq!(saved["language"], json!("en"));
    }

    #[test]
    fn config_set_refuses_a_patch_with_no_value_or_path() {
        let (_dir, context) = context();

        for params in [
            json!({ "path": "language" }),
            json!({ "path": "", "value": 1 }),
        ] {
            let reply = dispatch(&context, &job("config.set", params.clone()));
            assert!(!reply.ok, "{params} must be refused");
        }
    }

    #[test]
    fn config_set_clamps_nonsense_the_way_the_shell_reads_it() {
        let (_dir, context) = context();
        let mut document = serde_json::to_value(Config::default()).expect("render");
        document["shell"]["barHeight"] = json!(5_000);
        document["shell"]["barPosition"] = json!("sideways");
        document["shell"]["themeId"] = json!("  ");
        document["wallpapers"]["color"] = json!("rebeccapurple");
        document["wallpapers"]["rotateMinutes"] = json!(-3);

        let reply = dispatch(&context, &job("config.set", json!(document)));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["shell"]["barHeight"], json!(120));
        assert_eq!(reply.payload["shell"]["barPosition"], json!("top"));
        assert_eq!(reply.payload["shell"]["themeId"], json!(DEFAULT_THEME_ID));
        assert_eq!(
            reply.payload["wallpapers"]["color"],
            json!(neb_core::config::DEFAULT_WALLPAPER_COLOR)
        );
        assert_eq!(reply.payload["wallpapers"]["rotateMinutes"], json!(1));
    }

    #[test]
    fn config_set_refuses_a_payload_that_is_not_a_config() {
        let (_dir, context) = context();

        for params in [Value::Null, json!([]), json!("config")] {
            let reply = dispatch(&context, &job("config.set", params.clone()));
            assert!(!reply.ok, "{params} must be refused");
            assert!(
                reply.error.unwrap_or_default().contains("config object"),
                "the refusal must name what is wrong"
            );
        }
    }

    #[test]
    fn config_set_refuses_wrong_field_types_without_writing() {
        let (_dir, context) = context();

        let reply = dispatch(
            &context,
            &job("config.set", json!({ "shell": { "barHeight": "tall" } })),
        );

        assert!(!reply.ok);
        let error = reply.error.clone().unwrap_or_default();
        assert!(
            error.contains("barHeight"),
            "the error must name the field, got '{error}'"
        );
        assert!(
            !context.store.path().exists(),
            "a refused config must not be written"
        );
    }

    #[test]
    fn themes_list_offers_the_shipped_palettes_first() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("themes.list", Value::Null));

        assert!(reply.ok, "{reply:?}");
        let themes = reply.payload["themes"].as_array().expect("array");
        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0]["id"], json!("liquid-glass"));
        assert_eq!(themes[0]["builtIn"], json!(true));
        assert_eq!(reply.payload["active"], json!(DEFAULT_THEME_ID));
    }

    #[test]
    fn themes_save_writes_a_file_the_list_then_reads_back() {
        let (_dir, context) = context();
        let document = json!({
            "id": "sunset",
            "name": "Sunset",
            "background": "#120A0B",
            "surface": "#2A1518",
            "stroke": "#40202A",
            "text": "#FFF1EA",
            "textDim": "#C9A9A1",
            "accent": "#FF8A5C",
            "accent2": "#FF4D6D",
            "cornerRadius": 18
        });

        let saved = dispatch(&context, &job("themes.save", json!({ "theme": document })));
        assert!(saved.ok, "{saved:?}");
        assert!(context.themes_dir.join("sunset.json").is_file());

        let listed = dispatch(&context, &job("themes.list", Value::Null));
        let themes = listed.payload["themes"].as_array().expect("array");
        assert_eq!(themes.len(), 3);
        assert_eq!(themes[2]["id"], json!("sunset"));
        assert_eq!(themes[2]["builtIn"], json!(false));
    }

    #[test]
    fn themes_save_accepts_the_editors_nested_colours_and_activates_them() {
        let (_dir, context) = context();
        let document = json!({
            "id": "custom",
            "name": "Custom",
            "colors": {
                "background": "#111111",
                "surface": "#222222",
                "stroke": "#333333",
                "text": "#EEEEEE",
                "textDim": "#AAAAAA",
                "accent": "#FF8800",
                "accent2": "#FF0088"
            },
            "radius": 14
        });

        let reply = dispatch(&context, &job("themes.save", json!({ "theme": document })));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["theme"]["cornerRadius"], json!(14.0));
        assert_eq!(reply.payload["active"], json!("custom"));

        let config = context.store.load_checked().expect("readable");
        assert_eq!(config.shell.theme_id, "custom", "saving a theme selects it");

        let listed = dispatch(&context, &job("themes.list", Value::Null));
        let themes = listed.payload["themes"].as_array().expect("array");
        let custom = themes
            .iter()
            .find(|theme| theme["id"] == json!("custom"))
            .expect("the saved theme is listed");
        assert_eq!(custom["radius"], json!(14.0));
        assert_eq!(custom["accent"], json!("#FF8800"));
    }

    #[test]
    fn themes_save_refuses_a_colour_it_cannot_draw() {
        let (_dir, context) = context();
        let document = json!({
            "id": "broken",
            "name": "Broken",
            "background": "blue",
            "surface": "#2A1518",
            "stroke": "#40202A",
            "text": "#FFF1EA",
            "textDim": "#C9A9A1",
            "accent": "#FF8A5C",
            "accent2": "#FF4D6D",
            "cornerRadius": 18
        });

        let reply = dispatch(&context, &job("themes.save", document));

        assert!(!reply.ok);
        assert!(reply.error.unwrap_or_default().contains("background"));
        assert!(!context.themes_dir.join("broken.json").exists());
    }

    #[test]
    fn app_version_is_this_crates_version() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("app.version", Value::Null));

        assert_eq!(reply.payload["version"], json!(VERSION));
        assert_eq!(reply.payload["schemaVersion"], json!(SCHEMA_VERSION));
    }

    #[test]
    fn app_open_changelog_opens_the_documented_page() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("app.openChangelog", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["url"], json!(CHANGELOG_URL));
        assert_eq!(reply.payload["ok"], json!(true));
    }

    #[test]
    fn an_opener_failure_is_reported() {
        let (_dir, mut context) = context();
        context.opener = |_| Err("no shell".to_owned());

        let reply = dispatch(&context, &job("app.openChangelog", Value::Null));
        assert!(!reply.ok);
        assert!(reply.error.unwrap_or_default().contains("no shell"));

        let reply = dispatch(&context, &job("app.openLogsFolder", Value::Null));
        assert!(!reply.ok);
        assert!(reply.error.unwrap_or_default().contains("no shell"));
    }

    #[test]
    fn app_open_logs_folder_creates_the_folder_and_opens_it() {
        let (_dir, mut context) = context();
        context.opener = |_| Ok(());

        let reply = dispatch(&context, &job("app.openLogsFolder", Value::Null));

        assert!(reply.ok, "{reply:?}");
        let root = context.root().expect("root");
        assert!(root.join("logs").is_dir(), "the folder must exist");
        assert!(reply.payload["path"].as_str().is_some());
    }

    #[test]
    fn a_missing_shell_is_reported_with_the_method_and_the_pipe() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("shell.reload", Value::Null));

        assert!(!reply.ok);
        let error = reply.error.unwrap_or_default();
        assert!(error.contains("shell.reload"), "{error}");
        assert!(error.contains(&context.shell_pipe), "{error}");
    }

    #[test]
    fn shell_get_state_still_answers_when_the_shell_is_not_running() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("shell.getState", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["running"], json!(false));
        assert_eq!(reply.payload["shell"], json!(false));
        assert_eq!(reply.payload["wallpaper"], json!(false));
        assert_eq!(reply.payload["zapret"], json!(false));
        assert_eq!(reply.payload["discord"], json!(false));
        assert_eq!(reply.payload["wm"], json!(false));
        assert_eq!(
            reply.payload["windowsMode"],
            json!(Config::default().windows_mode.enabled)
        );
        assert_eq!(reply.payload["modules"]["bar"], json!(true));
        assert_eq!(reply.payload["modules"]["wallpaper"], json!(false));
    }

    #[test]
    fn shell_get_state_reports_the_supervisors_presence_flags() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub(
            "supervisor-presence",
            answering(json!({
                "pid": 4242,
                "children": {
                    "shell": { "pid": 11, "running": true },
                    "wallpaper": { "pid": Value::Null, "running": false },
                    "zapret": { "running": true },
                    "discord": { "running": false },
                    "wm": { "running": true }
                },
                "shell": { "pid": 11, "running": true },
                "wallpaper": { "pid": Value::Null, "running": false },
                "uptimeS": 9
            })),
        );
        context.supervisor_pipe = pipe;

        let reply = dispatch(&context, &job("shell.getState", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["shell"], json!(true));
        assert_eq!(reply.payload["wallpaper"], json!(false));
        assert_eq!(reply.payload["zapret"], json!(true));
        assert_eq!(reply.payload["discord"], json!(false));
        assert_eq!(reply.payload["wm"], json!(true));
        assert_eq!(reply.payload["running"], json!(true));
        server.shutdown();
    }

    #[test]
    fn a_restart_shell_asks_the_supervisor_for_the_shell() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("restart-child", |request: Request| {
            let name = request
                .params
                .as_ref()
                .and_then(|params| params.get("name"))
                .and_then(Value::as_str)
                .unwrap_or_default();

            assert_eq!(request.method, "app.restartChild");
            Response::success(
                request.id,
                json!({ "name": name, "running": true, "pid": 21, "applied": true }),
            )
        });
        context.supervisor_pipe = pipe;

        let reply = dispatch(&context, &job("app.restartShell", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["name"], json!("shell"));
        assert_eq!(reply.payload["running"], json!(true));
        server.shutdown();
    }

    #[test]
    fn app_quit_asks_the_supervisor_to_stop() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("app-quit", |request: Request| {
            assert_eq!(request.method, "app.quit");
            Response::success(request.id, json!({ "ok": true }))
        });
        context.supervisor_pipe = pipe;

        let reply = dispatch(&context, &job("app.quit", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["ok"], json!(true));
        server.shutdown();
    }

    #[test]
    fn restart_and_quit_report_a_missing_supervisor() {
        let (_dir, context) = context();

        for method in ["app.restartShell", "app.quit"] {
            let reply = dispatch(&context, &job(method, Value::Null));
            assert!(!reply.ok, "{method} without a supervisor must fail");
            assert_eq!(reply.error.as_deref(), Some(SUPERVISOR_DOWN));
        }
    }

    #[test]
    fn whats_new_is_null_when_the_updater_wrote_nothing() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("updates.whatsNew", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload, Value::Null);
    }

    #[test]
    fn whats_new_reads_the_notes_the_updater_wrote() {
        let (dir, context) = context();
        let folder = dir.path().join("updates");
        std::fs::create_dir_all(&folder).expect("updates folder");
        std::fs::write(
            folder.join("whats-new.json"),
            r#"{"version":"0.2.0","notes":"- fixes","changelogUrl":"https://example.invalid/log","shown":false}"#,
        )
        .expect("notes");

        let reply = dispatch(&context, &job("updates.whatsNew", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["version"], json!("0.2.0"));
        assert_eq!(reply.payload["notes"], json!("- fixes"));
        assert_eq!(
            reply.payload["changelogUrl"],
            json!("https://example.invalid/log")
        );

        // A BOM, which the C# build writes, must not hide the file either.
        std::fs::write(
            folder.join("whats-new.json"),
            "\u{feff}{\"version\":\"0.3.0\",\"notes\":\"bom\"}",
        )
        .expect("notes with a bom");

        let reply = dispatch(&context, &job("updates.whatsNew", Value::Null));
        assert_eq!(reply.payload["version"], json!("0.3.0"));
    }

    #[test]
    fn whats_new_refuses_a_broken_file_instead_of_pretending_it_is_empty() {
        let (dir, context) = context();
        let folder = dir.path().join("updates");
        std::fs::create_dir_all(&folder).expect("updates folder");
        std::fs::write(folder.join("whats-new.json"), "{not json").expect("notes");

        let reply = dispatch(&context, &job("updates.whatsNew", Value::Null));

        assert!(!reply.ok);
        assert!(reply.error.unwrap_or_default().contains("not valid json"));
    }

    #[test]
    fn logs_tail_reads_the_newest_file_and_keeps_the_last_two_hundred_lines() {
        let (dir, context) = context();
        let logs = dir.path().join("logs");
        std::fs::create_dir_all(&logs).expect("logs folder");
        std::fs::write(logs.join("neb-20260101.log"), "old\n").expect("old log");
        std::thread::sleep(Duration::from_millis(20));

        let body: String = (1..=250).map(|line| format!("line {line}\n")).collect();
        std::fs::write(logs.join("neb-20260102.log"), body).expect("new log");

        let reply = dispatch(&context, &job("logs.tail", json!({ "lines": 200 })));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["file"], json!("neb-20260102.log"));
        let lines = reply.payload["lines"].as_array().expect("lines");
        assert_eq!(lines.len(), TAIL_LINES);
        assert_eq!(lines[0], json!("line 51"));
        assert_eq!(lines[TAIL_LINES - 1], json!("line 250"));
        assert!(reply.payload["path"].as_str().is_some());
    }

    #[test]
    fn logs_tail_answers_empty_when_nothing_was_logged() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("logs.tail", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["file"], Value::Null);
        assert_eq!(reply.payload["lines"], json!([]));
    }

    #[test]
    fn logs_tail_skips_directories_named_like_logs() {
        let (dir, context) = context();
        std::fs::create_dir_all(dir.path().join("logs")).expect("logs folder");
        std::fs::write(dir.path().join("logs").join("neb-1.log"), "file").expect("log");
        // A directory named like a log is not a file and must be skipped, not crash the reader.
        std::fs::create_dir_all(dir.path().join("logs").join("neb-2.log")).expect("dir");

        let reply = dispatch(&context, &job("logs.tail", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["file"], json!("neb-1.log"));
    }

    #[test]
    fn wallpaper_settings_come_from_the_config_and_report_running() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("wallpaper.getSettings", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["enabled"], json!(false));
        assert_eq!(reply.payload["kind"], json!("Color"));
        assert_eq!(
            reply.payload["color"],
            json!(neb_core::config::DEFAULT_WALLPAPER_COLOR)
        );
        assert_eq!(reply.payload["running"], json!(false));
    }

    #[test]
    fn wallpaper_settings_are_persisted_one_field_at_a_time() {
        let (_dir, context) = context();

        for (field, value) in [
            ("enabled", json!(true)),
            ("color", json!("#112233")),
            ("rotateMinutes", json!(30)),
            ("pauseOnBattery", json!(false)),
        ] {
            let reply = dispatch(
                &context,
                &job("wallpaper.setSettings", json!({ field: value })),
            );
            assert!(reply.ok, "{field}: {reply:?}");
            assert_eq!(reply.payload[field], value, "{field}");
        }

        let saved = context.store.load_checked().expect("readable");
        assert!(saved.wallpapers.enabled);
        assert_eq!(saved.wallpapers.color, "#112233");
        assert_eq!(saved.wallpapers.rotate_minutes, 30);
        assert!(!saved.wallpapers.pause_on_battery);
    }

    #[test]
    fn wallpaper_set_settings_refuses_an_unknown_field() {
        let (_dir, context) = context();

        let reply = dispatch(
            &context,
            &job("wallpaper.setSettings", json!({ "scale": "fill" })),
        );

        assert!(!reply.ok);
        assert!(reply.error.unwrap_or_default().contains("scale"));
        assert!(!context.store.path().exists());
    }

    #[test]
    fn wallpaper_restart_uses_the_supervisor() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("wallpaper-restart", |request: Request| {
            assert_eq!(request.params.as_ref().unwrap()["name"], json!("wallpaper"));
            Response::success(request.id, json!({ "name": "wallpaper", "running": true }))
        });
        context.supervisor_pipe = pipe;

        let reply = dispatch(&context, &job("wallpaper.restart", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["ok"], json!(true));
        assert_eq!(reply.payload["running"], json!(true));
        server.shutdown();
    }

    #[test]
    fn wallpaper_pick_file_answers_with_the_dialog_result_or_a_cancel() {
        let (_dir, mut context) = context();
        context.picker = |_| Ok(Some(r"C:\pictures\sea.png".to_owned()));

        let reply = dispatch(&context, &job("wallpaper.pickFile", Value::Null));
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["path"], json!(r"C:\pictures\sea.png"));

        context.picker = |_| Ok(None);
        let reply = dispatch(&context, &job("wallpaper.pickFile", Value::Null));
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["path"], Value::Null);

        context.picker = |_| Err("no dialog".to_owned());
        let reply = dispatch(&context, &job("wallpaper.pickFile", Value::Null));
        assert!(!reply.ok);
        assert!(reply.error.unwrap_or_default().contains("no dialog"));
    }

    #[test]
    fn windows_methods_pass_through_to_the_supervisor() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("windows", |request: Request| {
            let result = match request.method.as_str() {
                "windows.getState" => {
                    json!({ "enabled": true, "toggles": { "desktopIcons": true } })
                }
                "windows.set" => json!({
                    "enabled": true,
                    "toggles": request.params.as_ref().unwrap()["toggles"].clone(),
                }),
                "windows.restore" => json!({ "enabled": false, "toggles": {} }),
                other => json!({ "unexpected": other }),
            };

            Response::success(request.id, result)
        });
        context.supervisor_pipe = pipe;

        let state = dispatch(&context, &job("windows.getState", Value::Null));
        assert!(state.ok, "{state:?}");
        assert_eq!(state.payload["enabled"], json!(true));

        let set = dispatch(
            &context,
            &job(
                "windows.set",
                json!({ "toggles": { "desktopIcons": true, "searchBox": false } }),
            ),
        );
        assert!(set.ok, "{set:?}");
        assert_eq!(set.payload["toggles"]["desktopIcons"], json!(true));
        assert_eq!(set.payload["toggles"]["searchBox"], json!(false));

        let restore = dispatch(&context, &job("windows.restore", Value::Null));
        assert!(restore.ok, "{restore:?}");
        assert_eq!(restore.payload["enabled"], json!(false));
        server.shutdown();
    }

    #[test]
    fn windows_methods_report_a_missing_supervisor() {
        let (_dir, context) = context();

        for method in ["windows.getState", "windows.set", "windows.restore"] {
            let reply = dispatch(&context, &job(method, json!({ "toggles": {} })));
            assert!(!reply.ok, "{method} without a supervisor must fail");
            assert_eq!(reply.error.as_deref(), Some(SUPERVISOR_DOWN));
        }
    }

    #[test]
    fn each_module_pipe_is_a_passthrough() {
        for (module, pipe_field) in [
            ("zapret", "zapret_pipe"),
            ("discord", "discord_pipe"),
            ("wm", "wm_pipe"),
        ] {
            let (_dir, mut context) = context();
            let (pipe, server) = stub(module, answering(json!({ "marker": module })));

            match pipe_field {
                "zapret_pipe" => context.zapret_pipe = pipe,
                "discord_pipe" => context.discord_pipe = pipe,
                _ => context.wm_pipe = pipe,
            }

            let method = format!("{module}.getState");
            let reply = dispatch(&context, &job(&method, Value::Null));

            assert!(reply.ok, "{method}: {reply:?}");
            assert_eq!(reply.payload["marker"], json!(module), "{method}");
            server.shutdown();
        }
    }

    #[test]
    fn a_missing_module_pipe_is_reported_as_not_running() {
        for method in [
            "zapret.start",
            "zapret.selfTest",
            "discord.connect",
            "discord.disconnect",
            "discord.savePreset",
            "wm.getState",
            "wm.enable",
        ] {
            let (_dir, context) = context();
            let reply = dispatch(&context, &job(method, Value::Null));

            // The read-only methods answer an honest "off" document while the module is disabled;
            // an action that needs a live module has to say that nobody answers.
            assert!(!reply.ok, "{method} without a module must fail");
            assert_eq!(reply.error.as_deref(), Some(MODULE_DOWN), "{method}");
        }
    }

    #[test]
    fn an_enabled_zapret_reports_a_missing_pipe() {
        let (_dir, context) = context();
        let mut config = Config::default();
        config.zapret.enabled = true;
        context.store.save(&config).expect("save");

        for method in ["zapret.getState", "zapret.status", "zapret.listStrategies"] {
            let reply = dispatch(&context, &job(method, Value::Null));
            assert!(!reply.ok, "{method} without a module must fail");
            assert_eq!(reply.error.as_deref(), Some(MODULE_DOWN), "{method}");
        }
    }

    #[test]
    fn a_disabled_zapret_answers_an_empty_state_instead_of_an_error() {
        let (_dir, context) = context();

        let status = dispatch(&context, &job("zapret.status", Value::Null));
        assert!(status.ok, "{status:?}");
        assert_eq!(status.payload["installed"], json!(false));
        assert_eq!(status.payload["running"], json!(false));

        let list = dispatch(&context, &job("zapret.listStrategies", Value::Null));
        assert!(list.ok, "{list:?}");
        assert_eq!(list.payload["strategies"], json!([]));
    }

    #[test]
    fn zapret_status_asks_the_module_for_its_state() {
        let (_dir, mut context) = context();
        context.store.save(&Config::default()).expect("save");
        let mut config = context.store.load_checked().expect("config");
        config.zapret.enabled = true;
        context.store.save(&config).expect("save");

        let (pipe, server) = stub("zapret-status", |request: Request| {
            assert_eq!(request.method, "zapret.getState");
            Response::success(request.id, json!({ "ok": true, "installed": true }))
        });
        context.zapret_pipe = pipe;

        let reply = dispatch(&context, &job("zapret.status", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["installed"], json!(true));
        server.shutdown();
    }

    #[test]
    fn discord_presets_are_translated_for_the_editor() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("discord-presets", |request: Request| {
            assert_eq!(request.method, "discord.listPresets");
            Response::success(
                request.id,
                json!({
                    "ok": true,
                    "presets": [{
                        "name": "Gaming",
                        "details": "In game",
                        "state": "Ranked",
                        "largeImage": "https://example.invalid/a.png",
                        "largeText": "Neb",
                        "smallImage": "",
                        "smallText": "",
                        "buttons": [],
                        "startTimestamp": 1700000000u64
                    }],
                    "errors": []
                }),
            )
        });
        context.discord_pipe = pipe;

        let reply = dispatch(&context, &job("discord.getPresets", Value::Null));

        assert!(reply.ok, "{reply:?}");
        let preset = &reply.payload["presets"][0];
        assert_eq!(preset["id"], json!("Gaming"));
        assert_eq!(
            preset["largeImageKey"],
            json!("https://example.invalid/a.png")
        );
        assert_eq!(preset["startTimestamp"], json!(true));
        server.shutdown();
    }

    #[test]
    fn discord_set_preset_sends_the_name_the_module_reads() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("discord-set-preset", |request: Request| {
            assert_eq!(request.method, "discord.setPreset");
            assert_eq!(request.params.as_ref().unwrap()["name"], json!("Gaming"));
            Response::success(request.id, json!({ "ok": true, "activePreset": "Gaming" }))
        });
        context.discord_pipe = pipe;

        let reply = dispatch(
            &context,
            &job(
                "discord.setPreset",
                json!({ "id": "Gaming", "presetId": "Gaming" }),
            ),
        );

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["activePreset"], json!("Gaming"));
        server.shutdown();
    }

    #[test]
    fn discord_save_preset_translates_the_editor_fields() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("discord-save-preset", |request: Request| {
            let preset = &request.params.as_ref().unwrap()["preset"];
            assert_eq!(request.method, "discord.savePreset");
            assert_eq!(preset["name"], json!("Gaming"));
            assert_eq!(preset["largeImage"], json!("asset-key"));
            assert_eq!(preset["largeText"], json!("Neb"));
            assert_eq!(preset["smallImage"], json!("badge"));
            assert_eq!(preset["smallText"], json!("Beta"));
            assert!(preset["startTimestamp"].as_u64().is_some());

            Response::success(
                request.id,
                json!({ "ok": true, "preset": preset, "path": "C:\\presets\\gaming.json" }),
            )
        });
        context.discord_pipe = pipe;

        let reply = dispatch(
            &context,
            &job(
                "discord.savePreset",
                json!({
                    "preset": {
                        "id": "Gaming",
                        "name": "Gaming",
                        "details": "In game",
                        "state": "Ranked",
                        "largeImageKey": "asset-key",
                        "largeImageText": "Neb",
                        "smallImageKey": "badge",
                        "smallImageText": "Beta",
                        "buttons": [],
                        "startTimestamp": true
                    }
                }),
            ),
        );

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["preset"]["id"], json!("Gaming"));
        assert_eq!(reply.payload["preset"]["largeImageKey"], json!("asset-key"));
        server.shutdown();
    }

    #[test]
    fn discord_set_rules_translates_the_editor_shape() {
        let (_dir, mut context) = context();
        let (pipe, server) = stub("discord-set-rules", |request: Request| {
            let rules = request.params.as_ref().unwrap()["rules"]
                .as_array()
                .expect("rules");
            assert_eq!(rules[0]["pattern"], json!("game.exe"));
            assert_eq!(rules[0]["preset"], json!("Gaming"));
            assert_eq!(rules[0]["enabled"], json!(true));
            assert!(
                rules[0].get("match").is_none(),
                "the editor-only key is not forwarded"
            );

            Response::success(request.id, json!({ "ok": true, "rules": rules }))
        });
        context.discord_pipe = pipe;

        let reply = dispatch(
            &context,
            &job(
                "discord.setRules",
                json!({
                    "rules": [{
                        "id": "exe:game.exe:0",
                        "match": "exe",
                        "pattern": "game.exe",
                        "presetId": "Gaming"
                    }]
                }),
            ),
        );

        assert!(reply.ok, "{reply:?}");
        server.shutdown();
    }

    #[test]
    fn a_disabled_discord_answers_empty_documents() {
        let (_dir, context) = context();

        let state = dispatch(&context, &job("discord.getState", Value::Null));
        assert!(state.ok, "{state:?}");
        assert_eq!(state.payload["state"], json!("disconnected"));
        assert_eq!(state.payload["enabled"], json!(false));

        let rules = dispatch(&context, &job("discord.getRules", Value::Null));
        assert!(rules.ok, "{rules:?}");
        assert_eq!(rules.payload["rules"], json!([]));

        let presets = dispatch(&context, &job("discord.getPresets", Value::Null));
        assert!(presets.ok, "{presets:?}");
        assert_eq!(presets.payload["presets"], json!([]));
    }

    #[test]
    fn metrics_have_the_documented_shape() {
        let (_dir, context) = context();
        let mut sampler = Sampler::new();
        let sample = sampler.sample();

        let metrics = metrics_for(&context, &sample, None);

        assert!(metrics["cpu"].as_f64().is_some(), "{metrics}");
        assert!(metrics["ram"].as_f64().is_some(), "{metrics}");
        assert_eq!(metrics["shell"]["running"], json!(false));
        assert_eq!(metrics["shell"]["pid"], Value::Null);
        assert_eq!(metrics["shell"]["mb"], Value::Null);
        assert_eq!(metrics["wallpaper"]["running"], json!(false));
        assert_eq!(metrics["supervisor"]["mb"], Value::Null);
        assert!(
            metrics["app"]["mb"].as_f64().is_some_and(|mb| mb > 0.0),
            "{metrics}"
        );
    }

    #[test]
    fn metrics_use_the_supervisors_child_state() {
        let (_dir, mut context) = context();
        let pid = std::process::id();
        let (pipe, server) = stub(
            "supervisor-metrics",
            answering(json!({
                "pid": pid,
                "children": {
                    "shell": { "pid": pid, "running": true },
                    "wallpaper": { "pid": u32::MAX, "running": false }
                },
                "shell": { "pid": pid, "running": true },
                "wallpaper": { "pid": u32::MAX, "running": false },
                "uptimeS": 1
            })),
        );
        context.supervisor_pipe = pipe;

        let supervisor = supervisor_state(&context, PRESENCE_TIMEOUT);
        let mut sampler = Sampler::new();
        let sample = sampler.sample();
        let metrics = metrics_for(&context, &sample, supervisor.as_ref());

        assert_eq!(metrics["shell"]["pid"], json!(pid));
        assert_eq!(metrics["shell"]["running"], json!(true));
        assert!(
            metrics["shell"]["mb"].as_f64().is_some_and(|mb| mb > 0.0),
            "{metrics}"
        );
        assert_eq!(metrics["wallpaper"]["pid"], json!(u32::MAX));
        assert_eq!(metrics["wallpaper"]["running"], json!(false));
        assert_eq!(metrics["wallpaper"]["mb"], Value::Null);
        assert!(
            metrics["supervisor"]["mb"]
                .as_f64()
                .is_some_and(|mb| mb > 0.0),
            "{metrics}"
        );
        server.shutdown();
    }

    #[test]
    fn modules_come_from_the_config() {
        let (_dir, context) = context();
        let mut config = Config::default();
        config.wallpapers.enabled = true;
        config.zapret.enabled = true;
        config.modules.windhawk = true;
        config.discord.enabled = true;
        config.window_manager.enabled = true;
        context.store.save(&config).expect("save");

        let modules = modules_for(&context, None);

        assert_eq!(modules["wallpapers"], json!(true));
        assert_eq!(modules["zapret"], json!(true));
        assert_eq!(modules["windhawk"], json!(true));
        assert_eq!(modules["discordRp"], json!(true));
        assert_eq!(modules["wm"], json!(true));
        assert_eq!(modules["running"], json!(false));
        assert_eq!(modules["modules"]["bar"], json!(true));
    }

    #[test]
    fn the_window_methods_need_a_window() {
        let (_dir, context) = context();

        for method in ["window.minimize", "window.close", "window.toggleMaximize"] {
            let reply = dispatch(&context, &job(method, Value::Null));
            assert!(!reply.ok, "{method} without a window must be refused");
            assert!(reply.error.unwrap_or_default().contains("no window"));
        }
    }

    #[test]
    fn a_reply_serialises_to_the_documented_envelope() {
        let reply = Reply::success(
            &Job {
                id: "5".to_owned(),
                method: "app.version".to_owned(),
                params: Value::Null,
            },
            json!({ "version": "1.0.0" }),
        );

        let value: Value = serde_json::from_str(&reply.to_message()).expect("json");

        assert_eq!(value["type"], json!(RESULT_TYPE));
        assert_eq!(value["id"], json!("5"));
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["payload"]["version"], json!("1.0.0"));
        assert_eq!(value["error"], Value::Null);
    }

    #[test]
    fn a_failed_reply_carries_the_message_and_no_payload() {
        let reply = Reply::failure("6", "boom");
        let value: Value = serde_json::from_str(&reply.to_message()).expect("json");

        assert_eq!(value["ok"], json!(false));
        assert_eq!(value["error"], json!("boom"));
        assert_eq!(value["payload"], Value::Null);
    }

    #[test]
    fn the_worker_answers_every_job_on_its_own_thread() {
        let (_dir, context) = context();
        let (tx, rx) = mpsc::channel::<String>();
        let sender = spawn_worker(context, move |message| {
            let _ = tx.send(message);
        })
        .expect("a thread can be started in the test");

        sender
            .send(Command::Invoke(job("app.version", Value::Null)))
            .expect("the worker must accept a job");
        drop(sender);

        let message = rx.recv_timeout(Duration::from_secs(3)).expect("an answer");
        let value: Value = serde_json::from_str(&message).expect("json");

        assert_eq!(value["type"], json!(RESULT_TYPE));
        assert_eq!(value["ok"], json!(true));
        assert_eq!(value["payload"]["version"], json!(VERSION));
    }

    #[test]
    fn the_ready_message_pushes_both_live_documents() {
        let (_dir, context) = context();
        let (tx, rx) = mpsc::channel::<String>();
        let sender = spawn_worker(context, move |message| {
            let _ = tx.send(message);
        })
        .expect("a thread can be started in the test");

        sender.send(Command::Ready).expect("the page is ready");

        let mut kinds = Vec::new();
        while kinds.len() < 2 {
            let message = rx
                .recv_timeout(Duration::from_secs(3))
                .expect("a live document");
            let value: Value = serde_json::from_str(&message).expect("json");
            kinds.push(value["type"].as_str().unwrap_or_default().to_owned());

            if value["type"] == json!(METRICS_TYPE) {
                assert!(value["payload"]["app"]["mb"].as_f64().is_some(), "{value}");
            }
        }

        assert!(kinds.contains(&METRICS_TYPE.to_owned()), "{kinds:?}");
        assert!(kinds.contains(&MODULES_TYPE.to_owned()), "{kinds:?}");
        drop(sender);
    }

    // ---------- account / licence ----------

    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};

    /// A local stand-in for the worker: JWKS, `/api/me`, refresh and a test-only session endpoint.
    struct StubApi {
        base: String,
        session: String,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl StubApi {
        fn start(plan: &str) -> Self {
            let seed = neb_core::license::testing::test_seed();
            let kid = neb_core::license::testing::TEST_KID;
            let now = account_api::now_seconds();
            let session =
                neb_core::license::testing::session_token(&seed, kid, "42", now, now + 86_400);
            let license = neb_core::license::testing::license_token(
                &seed,
                kid,
                "42",
                plan,
                now,
                now + 900,
                now + 30 * 86_400,
            );
            let jwks = neb_core::license::testing::jwks_document(&seed, kid).to_string();
            let until = account_api::iso_from_unix(now + 30 * 86_400);

            let listener = TcpListener::bind("127.0.0.1:0").expect("bind the stub");
            let port = listener.local_addr().expect("addr").port();
            listener.set_nonblocking(true).expect("nonblocking");

            let base = format!("http://127.0.0.1:{port}");
            let stop = Arc::new(AtomicBool::new(false));
            let stopped = stop.clone();
            let session_for_server = session.clone();
            let plan = plan.to_owned();

            let thread = std::thread::spawn(move || {
                while !stopped.load(AtomicOrdering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            serve_stub(stream, &jwks, &session_for_server, &license, &plan, &until);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            });

            Self {
                base,
                session,
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for StubApi {
        fn drop(&mut self) {
            self.stop.store(true, AtomicOrdering::SeqCst);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    fn serve_stub(
        mut stream: TcpStream,
        jwks: &str,
        session: &str,
        license: &str,
        plan: &str,
        until: &str,
    ) {
        let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut request = Vec::new();
        let mut buffer = [0u8; 4096];
        let header_end = loop {
            let read = stream.read(&mut buffer).unwrap_or(0);
            if read == 0 {
                return;
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(position) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
        };
        let head = String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
        let expected = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length:"))
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        while request.len() < header_end + expected {
            let read = stream.read(&mut buffer).unwrap_or(0);
            if read == 0 {
                break;
            }
            request.extend_from_slice(&buffer[..read]);
        }

        let first_line = String::from_utf8_lossy(&request)
            .lines()
            .next()
            .unwrap_or_default()
            .to_owned();
        let target = first_line.split_whitespace().nth(1).unwrap_or("/");
        let path = target.split('?').next().unwrap_or("/");

        let (status, body) = match path {
            "/api/keys/jwks" => (200, jwks.to_owned()),
            "/test/session" => (200, json!({ "session": session }).to_string()),
            "/api/me" => (
                200,
                json!({
                    "user": { "discordId": "42", "username": "Tester", "avatar": "avatar-hash" },
                    "plan": plan,
                    "until": until,
                })
                .to_string(),
            ),
            "/api/license/refresh" if plan == "pro" => (
                200,
                json!({
                    "token": license,
                    "expiresAt": until,
                    "expiresIn": 900,
                    "plan": plan,
                    "until": until,
                    "devices": [],
                })
                .to_string(),
            ),
            "/api/license/refresh" => (
                403,
                json!({ "error": { "code": "upgrade_required", "message": "Pro is required" } })
                    .to_string(),
            ),
            _ => (404, json!({ "error": { "code": "not_found" } }).to_string()),
        };

        let response = format!(
            "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\n\
             connection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(response.as_bytes());
    }

    /// Extracts the `return` value from the start URL, asks the stub for its session, and hits the
    /// loopback callback. Runs on its own thread so the blocking `wait` in `signIn` can accept.
    fn simulated_browser(start_url: &str) -> Result<(), String> {
        let query = start_url
            .split_once('?')
            .map(|(_, query)| query)
            .ok_or_else(|| "start URL has no query".to_owned())?;
        let return_value = query
            .split('&')
            .find_map(|part| part.strip_prefix("return="))
            .ok_or_else(|| "start URL has no return value".to_owned())?;
        let callback = neb_core::license::http::percent_decode(return_value);

        let (scheme, rest) = start_url
            .split_once("://")
            .ok_or_else(|| "start URL is not absolute".to_owned())?;
        let authority = rest.split('/').next().unwrap_or_default();
        let session_url = format!("{scheme}://{authority}/test/session");

        let response =
            neb_core::license::http::get(&session_url, &[]).map_err(|error| error.to_string())?;
        let value: Value =
            serde_json::from_slice(&response.body).map_err(|error| error.to_string())?;
        let session = value["session"]
            .as_str()
            .ok_or_else(|| "the stub returned no session".to_owned())?;

        let target = format!(
            "{callback}&session={}",
            neb_core::license::http::percent_encode(session)
        );
        let _ = neb_core::license::http::get(&target, &[]);
        Ok(())
    }

    fn simulated_browser_async(start_url: &str) -> Result<(), String> {
        let start_url = start_url.to_owned();
        std::thread::spawn(move || {
            let _ = simulated_browser(&start_url);
        });
        Ok(())
    }

    fn configure_account(context: &Context, base: &str) {
        let mut config = Config::default();
        config.account.api_base = base.to_owned();
        context
            .store
            .save(&config)
            .expect("save the account config");
    }

    #[test]
    fn a_whole_config_save_without_an_account_section_keeps_the_service_url() {
        let (_dir, context) = context();
        configure_account(&context, "https://api.projectneb.example");
        let mut stored = context.store.load_checked().expect("config");
        stored.account.device_id = "device-1".to_owned();
        context.store.save(&stored).expect("seed");

        // The dashboard's own AppConfig shape has no `account` key.
        let mut document = serde_json::to_value(Config::default()).expect("render");
        document.as_object_mut().expect("object").remove("account");
        document["language"] = json!("en");

        let reply = dispatch(&context, &job("config.set", json!({ "config": document })));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(
            reply.payload["account"]["apiBase"],
            json!("https://api.projectneb.example")
        );
        assert_eq!(reply.payload["account"]["deviceId"], json!("device-1"));
        assert_eq!(reply.payload["language"], json!("en"));
    }

    #[test]
    fn the_account_section_defaults_to_an_honest_not_configured_state() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("account.getState", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["configured"], json!(false));
        assert_eq!(reply.payload["signedIn"], json!(false));
        assert_eq!(reply.payload["plan"], json!("free"));
        assert_eq!(reply.payload["stale"], json!(false));

        let config = dispatch(&context, &job("config.get", Value::Null));
        assert_eq!(config.payload["account"]["apiBase"], json!(""));
    }

    #[test]
    fn sign_in_without_an_api_base_is_refused_with_a_hint() {
        let (_dir, context) = context();

        let reply = dispatch(&context, &job("account.signIn", Value::Null));

        assert!(!reply.ok);
        assert!(reply.error.unwrap_or_default().contains("account.apiBase"));
    }

    #[test]
    fn account_sign_in_stores_an_entitlement_and_sign_out_wipes_it() {
        let (_dir, mut context) = context();
        let stub = StubApi::start("pro");
        configure_account(&context, &stub.base);
        context.opener = simulated_browser_async;

        let reply = dispatch(&context, &job("account.signIn", Value::Null));
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["signedIn"], json!(true));
        assert_eq!(reply.payload["plan"], json!("pro"));
        assert_eq!(reply.payload["name"], json!("Tester"));
        assert_eq!(reply.payload["avatar"], json!("avatar-hash"));
        assert_eq!(reply.payload["stale"], json!(false));
        assert_eq!(reply.payload["configured"], json!(true));

        // The tokens are on disk only inside a DPAPI blob.
        let licence_path = context.store.path().parent().unwrap().join("license.json");
        let bytes = std::fs::read_to_string(&licence_path).expect("licence file");
        assert!(
            !bytes.contains(&stub.session),
            "the session must be encrypted"
        );
        assert!(bytes.contains("\"v\": 1"), "{bytes}");

        let state = dispatch(&context, &job("license.getState", Value::Null));
        assert!(state.ok, "{state:?}");
        assert_eq!(state.payload["plan"], json!("pro"));
        assert_eq!(state.payload["valid"], json!(true));

        let signed_out = dispatch(&context, &job("account.signOut", Value::Null));
        assert!(signed_out.ok, "{signed_out:?}");
        assert_eq!(signed_out.payload["signedIn"], json!(false));
        assert_eq!(signed_out.payload["plan"], json!("free"));
        assert!(!licence_path.exists(), "sign-out removes the tokens");
    }

    #[test]
    fn an_expired_entitlement_is_pro_for_thirty_days_and_then_free() {
        let (_dir, mut context) = context();
        let stub = StubApi::start("pro");
        configure_account(&context, &stub.base);
        context.opener = simulated_browser_async;

        let signed_in = dispatch(&context, &job("account.signIn", Value::Null));
        assert!(signed_in.ok, "{signed_in:?}");

        let store =
            store::LicenseStore::new(context.store.path().parent().unwrap().join("license.json"));
        let now = account_api::now_seconds();

        let mut record = store.load().expect("load").expect("a record");
        record.expires_at = Some(now - 10);
        record.refreshed_at = now;
        store.save(&record).expect("save an expired token");

        let stale = dispatch(&context, &job("account.getState", Value::Null));
        assert!(stale.ok, "{stale:?}");
        assert_eq!(stale.payload["plan"], json!("pro"));
        assert_eq!(stale.payload["stale"], json!(true));

        record.refreshed_at = now - 31 * 86_400;
        store.save(&record).expect("save an old cache");

        let expired = dispatch(&context, &job("account.getState", Value::Null));
        assert!(expired.ok, "{expired:?}");
        assert_eq!(expired.payload["plan"], json!("free"));
        assert_eq!(expired.payload["stale"], json!(false));
        assert_eq!(expired.payload["until"], Value::Null);
    }

    #[test]
    fn a_signed_in_free_account_stays_free() {
        let (_dir, mut context) = context();
        let stub = StubApi::start("free");
        configure_account(&context, &stub.base);
        context.opener = simulated_browser_async;

        let reply = dispatch(&context, &job("account.signIn", Value::Null));

        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["signedIn"], json!(true));
        assert_eq!(reply.payload["plan"], json!("free"));
        assert_eq!(reply.payload["stale"], json!(false));
    }

    #[test]
    fn refresh_needs_a_session_and_renews_the_entitlement() {
        let (_dir, mut context) = context();
        let stub = StubApi::start("pro");
        configure_account(&context, &stub.base);

        let before = dispatch(&context, &job("account.refresh", Value::Null));
        assert!(!before.ok, "no session yet");
        assert!(before.error.unwrap_or_default().contains("not signed in"));

        context.opener = simulated_browser_async;
        assert!(dispatch(&context, &job("account.signIn", Value::Null)).ok);

        let reply = dispatch(&context, &job("account.refresh", Value::Null));
        assert!(reply.ok, "{reply:?}");
        assert_eq!(reply.payload["plan"], json!("pro"));

        let config = context.store.load_checked().expect("config");
        assert!(
            !config.account.device_id.trim().is_empty(),
            "the device id is minted on first use and kept"
        );
    }
}
