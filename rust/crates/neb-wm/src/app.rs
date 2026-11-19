//! The resident process: a hidden message window, the public event hooks, the hotkeys and the IPC
//! server.
//!
//! Everything that touches a window runs on this one thread, inside message dispatch. The hook
//! callback runs there too (out-of-context hooks are delivered with the message queue), so it does
//! no Win32 work: it posts a message to the hidden window and returns. That keeps a burst of window
//! events from blocking the application that generated them.
//!
//! State lives in two process-wide cells because the window procedure and the IPC handler are
//! plain `extern "system"` and `Fn` values that cannot capture: [`ENGINE`] is the model behind a
//! mutex, and [`RULES`] is compiled once at startup. The IPC thread only mutates the model and
//! posts [`WM_APP_COMMAND`]; the Win32 calls happen here, in order, on the message loop.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use neb_core::config::Config;
use neb_core::logging::{self, LogHandle};
use neb_core::theme::ThemeTokens;
use serde_json::Value;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_SHIFT, RegisterHotKey, UnregisterHotKey,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, EVENT_OBJECT_DESTROY,
    EVENT_OBJECT_HIDE, EVENT_OBJECT_LOCATIONCHANGE, EVENT_OBJECT_SHOW, EVENT_SYSTEM_FOREGROUND,
    GetMessageW, HWND_MESSAGE, KillTimer, MSG, PostMessageW, PostQuitMessage, RegisterClassW,
    SetTimer, TranslateMessage, WM_APP, WM_CLOSE, WM_DESTROY, WM_HOTKEY, WM_TIMER, WNDCLASSW,
    WS_OVERLAPPED,
};
use windows::core::PCWSTR;

use crate::engine::{Engine, Outcome};
use crate::ipc;
use crate::model::{Window, WindowId};
use crate::rect::Direction;
use crate::rules::RuleSet;
use crate::win::{self, Candidate, Hooks, Win32Ops};

/// Hidden window class.
const CLASS_NAME: &str = "ProjectNeb.Wm.Host";

/// Title of the hidden window, for debugging tools.
const WINDOW_TITLE: &str = "ProjectNeb Window Manager";

/// Posted by the hook callback; `wparam` is the event id and `lparam` the window handle.
const WM_APP_EVENT: u32 = WM_APP + 1;

/// Posted by the IPC thread after a model change; the loop applies the new state.
const WM_APP_COMMAND: u32 = WM_APP + 2;

/// Posted by `wm.quit` or `--quit-after`; ends the loop.
const WM_APP_QUIT: u32 = WM_APP + 3;

/// Timer id of the relayout debounce.
const TIMER_RELAYOUT: usize = 1;

/// How long the manager waits after a location event before it re-tiles.
const RELAYOUT_DELAY_MS: u32 = 90;

/// How long location and hide events are ignored after the manager moved windows itself.
///
/// The animation is 176 ms; the margin covers a slow application applying an asynchronous
/// `SetWindowPos` after the loop has already finished.
const SUPPRESSION_MS: u64 = 600;

/// The manager's model, shared with the IPC thread.
static ENGINE: OnceLock<Mutex<Engine>> = OnceLock::new();

/// The compiled rules, immutable after startup.
static RULES: OnceLock<RuleSet> = OnceLock::new();

/// The log handle, used by the window procedure and the hotkey handler.
static LOG: OnceLock<LogHandle> = OnceLock::new();

/// The hidden window's handle, as a plain integer.
static HOST: AtomicIsize = AtomicIsize::new(0);

/// Wall-clock millisecond until which location/hide events are ignored.
static SUPPRESS_UNTIL: AtomicU64 = AtomicU64::new(0);

/// The registered hotkey ids and what they do.
static KEYS: OnceLock<Vec<(i32, Hotkey)>> = OnceLock::new();

/// What one hotkey asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Hotkey {
    /// Move focus to the neighbour in a direction.
    Focus(Direction),
    /// Swap the focused window with its neighbour.
    Move(Direction),
    /// Switch to a workspace.
    Workspace(usize),
    /// Move the focused window to a workspace.
    MoveToWorkspace(usize),
    /// Toggle the float state.
    Float,
    /// Cycle the layout.
    Layout,
}

/// The command line the binary understands.
#[derive(Debug, Clone)]
pub struct Args {
    /// Configuration file.
    pub config: PathBuf,
    /// Where the log lives; defaults to `logs` next to the configuration.
    pub log_dir: Option<PathBuf>,
    /// Stop cleanly after this many seconds; the acceptance harness uses it.
    pub quit_after: Option<u64>,
    /// Ask a running manager a question instead of starting one.
    pub call: Option<Call>,
}

/// One `--call` invocation.
#[derive(Debug, Clone)]
pub struct Call {
    /// IPC method, e.g. `wm.getState`.
    pub method: String,
    /// Optional JSON parameters.
    pub params: Option<Value>,
}

impl Args {
    /// Parses the command line, without the program name.
    pub fn parse(arguments: &[String]) -> Result<Self, String> {
        let mut args = Self {
            config: default_config_path(),
            log_dir: None,
            quit_after: None,
            call: None,
        };
        let mut index = 0;

        while index < arguments.len() {
            let argument = arguments[index].as_str();
            match argument {
                "--config" => {
                    index += 1;
                    args.config = PathBuf::from(
                        arguments
                            .get(index)
                            .ok_or_else(|| "--config needs a path".to_owned())?,
                    );
                }
                "--log-dir" => {
                    index += 1;
                    args.log_dir = Some(PathBuf::from(
                        arguments
                            .get(index)
                            .ok_or_else(|| "--log-dir needs a path".to_owned())?,
                    ));
                }
                "--quit-after" => {
                    index += 1;
                    let value = arguments
                        .get(index)
                        .ok_or_else(|| "--quit-after needs a number of seconds".to_owned())?;
                    args.quit_after = Some(
                        value
                            .parse()
                            .map_err(|_| format!("'{value}' is not a number of seconds"))?,
                    );
                }
                "--call" => {
                    index += 1;
                    let method = arguments
                        .get(index)
                        .ok_or_else(|| "--call needs an IPC method".to_owned())?
                        .clone();
                    args.call = Some(Call {
                        method,
                        params: None,
                    });
                }
                "--params" => {
                    index += 1;
                    let text = arguments
                        .get(index)
                        .ok_or_else(|| "--params needs JSON".to_owned())?;
                    let value = serde_json::from_str(text)
                        .map_err(|error| format!("--params is not JSON: {error}"))?;

                    match args.call.as_mut() {
                        Some(call) => call.params = Some(value),
                        None => return Err("--params is only valid with --call".to_owned()),
                    }
                }
                other => return Err(format!("unknown argument '{other}'")),
            }

            index += 1;
        }

        Ok(args)
    }
}

/// The configuration the running manager reads, or `Err` with a readable reason.
fn load_config(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
    // The C# build writes `config.json` with a UTF-8 BOM and the installer ships one; every
    // other reader strips it, so a byte order mark must not stop the manager from starting.
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    serde_json::from_str(text)
        .map_err(|error| format!("cannot parse '{}': {error}", path.display()))
}

/// Starts the manager and runs until the message loop ends.
pub fn run(args: Args) -> Result<(), String> {
    let config = load_config(&args.config)?;
    let settings = config.window_manager.clone();

    let log_dir = args.log_dir.clone().unwrap_or_else(|| {
        args.config
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("logs")
    });
    let log = logging::init(&log_dir)
        .map_err(|error| format!("cannot start logging in '{}': {error}", log_dir.display()))?;

    let accent = ThemeTokens::load(config.shell.theme(), Some(&log)).accent;
    let (rules, warnings) = RuleSet::compile(&settings.rules);
    for warning in &warnings {
        neb_core::log_warn!(log, "wm", "{}", warning);
    }
    let _ = RULES.set(rules);

    let engine = Engine::new(&settings, accent);
    let enabled = engine.enabled;
    let layout = engine.layout;
    let workspaces = engine.model.workspaces();
    let _ = ENGINE.set(Mutex::new(engine));
    let _ = LOG.set(log.clone());

    let (hwnd, _class) = create_host_window()?;
    HOST.store(hwnd.0 as isize, Ordering::SeqCst);

    // SAFETY: `event_callback` is a plain `extern "system"` function that lives for the whole
    // process, which is exactly what `SetWinEventHook` requires.
    let hooks = unsafe { Hooks::install(Some(event_callback)) }?;

    let keys = register_hotkeys(hwnd, &log);
    let _ = KEYS.set(keys);

    let server = neb_core::ipc::serve(ipc::PIPE, ipc::handle)
        .map_err(|error| format!("cannot serve '{}': {error}", ipc::PIPE))?;

    win::begin_high_resolution_timer();

    neb_core::log_info!(
        log,
        "wm",
        "started: enabled={enabled}, layout={}, workspaces={workspaces}, pipe={}",
        layout.as_str(),
        ipc::PIPE
    );

    if enabled {
        with_engine(|engine| {
            refresh(engine, &log);
            relayout(engine, &log);
        });
    } else {
        neb_core::log_info!(log, "wm", "management is disabled in the configuration");
    }

    if let Some(seconds) = args.quit_after {
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(seconds));
            post_quit();
        });
    }

    let exit_code = message_loop();
    neb_core::log_info!(log, "wm", "message loop ended ({exit_code})");

    // The shutdown contract: whichever way the process ends, every managed window gets its
    // rectangle and decorations back before the process disappears.
    with_engine(|engine| {
        if !engine.model.is_empty() {
            suppress();
            let ops = Win32Ops::new(engine.accent);
            let outcome = engine.disable(&ops);
            report(&log, "shutdown", &outcome);
        }
    });

    for (id, _) in KEYS.get().into_iter().flatten() {
        // SAFETY: the ids belong to this thread and this window; a failed unregister is harmless
        // because the process is ending.
        unsafe {
            let _ = UnregisterHotKey(Some(hwnd), *id);
        }
    }

    // SAFETY: the window belongs to this process and is destroyed once.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
    drop(hooks);
    server.shutdown();

    Ok(())
}

/// Runs the message loop until `WM_QUIT`; returns the process exit code.
fn message_loop() -> i32 {
    let mut message = MSG::default();

    loop {
        // SAFETY: `message` is a live `MSG` and no filter is requested.
        let result = unsafe { GetMessageW(&raw mut message, None, 0, 0) };

        if result.0 <= 0 {
            return message.wParam.0 as i32;
        }

        // SAFETY: `message` was filled by `GetMessageW` above.
        unsafe {
            let _ = TranslateMessage(&raw const message);
            DispatchMessageW(&raw const message);
        }
    }
}

/// The hidden window's procedure: it owns the hotkeys, the debounce timer and the posted events.
unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_HOTKEY => {
            on_hotkey(wparam.0 as i32);
            LRESULT(0)
        }
        WM_APP_EVENT => {
            on_event(wparam.0 as u32, WindowId(lparam.0));
            LRESULT(0)
        }
        WM_APP_COMMAND => {
            on_command();
            LRESULT(0)
        }
        WM_APP_QUIT => {
            // SAFETY: posting WM_QUIT to this thread's queue is the documented way to leave.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_TIMER if wparam.0 == TIMER_RELAYOUT => {
            // SAFETY: the timer belongs to this window and is killed exactly once.
            unsafe {
                let _ = KillTimer(Some(hwnd), TIMER_RELAYOUT);
            }
            on_relayout();
            LRESULT(0)
        }
        WM_CLOSE | WM_DESTROY => {
            // SAFETY: as above.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => {
            // SAFETY: the default procedure handles every message this window does not.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

/// The out-of-context event callback: it only posts; all work happens in the message loop.
unsafe extern "system" fn event_callback(
    _hook: windows::Win32::UI::Accessibility::HWINEVENTHOOK,
    event: u32,
    hwnd: HWND,
    id_object: i32,
    id_child: i32,
    _thread: u32,
    _time: u32,
) {
    // Only whole top-level windows; a caret or a menu inside one is not ours to move.
    if id_object != 0 || id_child != 0 || hwnd.0.is_null() {
        return;
    }

    let host = HOST.load(Ordering::SeqCst);
    if host == 0 {
        return;
    }

    // SAFETY: the host window is a live message-only window owned by this thread; `PostMessageW`
    // copies the two integers and returns immediately.
    unsafe {
        let _ = PostMessageW(
            Some(HWND(host as *mut core::ffi::c_void)),
            WM_APP_EVENT,
            WPARAM(event as usize),
            LPARAM(hwnd.0 as isize),
        );
    }
}

/// Handles one posted window event.
fn on_event(event: u32, id: WindowId) {
    let Some(log) = LOG.get() else {
        return;
    };

    match event {
        EVENT_OBJECT_SHOW => {
            with_engine(|engine| {
                if !engine.enabled || engine.model.contains(id) {
                    return;
                }

                if let Some(candidate) = win::candidate(id) {
                    manage_candidate(engine, &candidate, log);
                    mark_dirty();
                }
            });
        }
        EVENT_OBJECT_HIDE => {
            if suppressed() {
                return;
            }

            // Minimising hides the window too. Forgetting it there would capture the minimised
            // rectangle as "original" and restore the window to -32000 on the next event, so a
            // minimised window stays in the model and simply is not tiled.
            if win::is_iconic(id) {
                return;
            }

            with_engine(|engine| {
                if engine.model.contains(id) {
                    engine.model.forget(id);
                    mark_dirty();
                }
            });
        }
        EVENT_OBJECT_DESTROY => {
            with_engine(|engine| {
                if engine.model.contains(id) {
                    engine.model.forget(id);
                    mark_dirty();
                }
            });
        }
        EVENT_OBJECT_LOCATIONCHANGE => {
            if suppressed() {
                return;
            }

            with_engine(|engine| {
                if engine.enabled && engine.model.contains(id) {
                    mark_dirty();
                }
            });
        }
        EVENT_SYSTEM_FOREGROUND => {
            with_engine(|engine| {
                if engine.model.contains(id) {
                    engine.model.focus(id);
                }
            });
        }
        _ => {}
    }
}

/// Handles a model change requested by IPC.
fn on_command() {
    let Some(log) = LOG.get() else {
        return;
    };

    with_engine(|engine| {
        if engine.enabled {
            if engine.model.is_empty() {
                refresh(engine, log);
            }
            relayout(engine, log);
        } else {
            suppress();
            let ops = Win32Ops::new(engine.accent);
            let outcome = engine.disable(&ops);
            report(log, "disable", &outcome);
        }
    });
}

/// Re-tiles after the debounce timer.
fn on_relayout() {
    let Some(log) = LOG.get() else {
        return;
    };

    with_engine(|engine| {
        if engine.enabled {
            relayout(engine, log);
        }
    });
}

/// Handles a registered hotkey.
fn on_hotkey(id: i32) {
    let Some(log) = LOG.get() else {
        return;
    };
    let Some(hotkey) = KEYS
        .get()
        .and_then(|keys| keys.iter().find(|(key, _)| *key == id))
        .map(|(_, hotkey)| *hotkey)
    else {
        return;
    };

    with_engine(|engine| {
        if !engine.enabled {
            return;
        }

        let area = win::work_area();

        match hotkey {
            Hotkey::Focus(direction) => {
                let ops = Win32Ops::new(engine.accent);
                if let Some(id) = engine.focus_direction(&ops, area, direction) {
                    neb_core::log_debug!(log, "wm", "focus {direction:?} -> {id}");
                }
            }
            Hotkey::Move(direction) => {
                if engine.move_direction(area, direction) {
                    relayout(engine, log);
                }
            }
            Hotkey::Workspace(index) => {
                engine.model.set_active(index);
                relayout(engine, log);
                neb_core::log_info!(log, "wm", "workspace {} active", index + 1);
            }
            Hotkey::MoveToWorkspace(index) => {
                if let Some(focused) = engine.model.focused() {
                    engine.model.set_workspace(focused, index);
                    relayout(engine, log);
                    neb_core::log_info!(log, "wm", "window {focused} -> workspace {}", index + 1);
                }
            }
            Hotkey::Float => {
                if engine.toggle_float() {
                    relayout(engine, log);
                }
            }
            Hotkey::Layout => {
                let layout = engine.cycle_layout();
                relayout(engine, log);
                neb_core::log_info!(log, "wm", "layout {layout:?}");
            }
        }
    });
}

/// Manages one candidate, applying its rule.
fn manage_candidate(engine: &mut Engine, candidate: &Candidate, log: &LogHandle) {
    let decision = RULES
        .get()
        .and_then(|rules| rules.decide(&candidate.facts))
        .unwrap_or_default();

    if decision.ignore {
        neb_core::log_debug!(log, "wm", "ignored by a rule: {}", candidate.facts.title);
        return;
    }

    let rule = RULES
        .get()
        .and_then(|rules| rules.rule_index(&candidate.facts));

    let mut window = Window::new(candidate.id, candidate.facts.clone(), candidate.bounds);
    window.workspace = decision
        .workspace
        .unwrap_or(engine.model.active())
        .min(engine.model.workspaces() - 1);
    window.floating = decision.float.unwrap_or(false);
    window.no_border = decision.no_border.unwrap_or(false);
    window.rule = rule;

    let workspace = window.workspace + 1;
    let title = window.title.clone();
    let exe = window.exe.clone();
    let ops = Win32Ops::new(engine.accent);

    match engine.manage(&ops, window) {
        Ok(()) => neb_core::log_info!(
            log,
            "wm",
            "managing '{}' ({exe}) on workspace {workspace}",
            title
        ),
        Err(error) => neb_core::log_warn!(log, "wm", "cannot manage '{title}': {error}"),
    }
}

/// Enumerates existing windows and manages every candidate that is not in the model yet.
fn refresh(engine: &mut Engine, log: &LogHandle) {
    for candidate in win::enumerate_candidates() {
        if engine.model.contains(candidate.id) {
            continue;
        }

        manage_candidate(engine, &candidate, log);
    }
}

/// Shows the active workspace, hides the rest and applies the layout.
fn relayout(engine: &mut Engine, log: &LogHandle) {
    suppress();
    let ops = Win32Ops::new(engine.accent);

    let visibility = engine.sync_visibility(&ops);
    report(log, "visibility", &visibility);

    let area = win::work_area();
    let outcome = engine.apply(&ops, area);
    report(log, "layout", &outcome);
}

/// Logs an outcome and its animation measurement.
fn report(log: &LogHandle, stage: &str, outcome: &Outcome) {
    if outcome.positioned > 0 {
        neb_core::log_debug!(
            log,
            "wm",
            "{stage}: {} window(s) placed",
            outcome.positioned
        );
    }
    for refusal in &outcome.refused {
        neb_core::log_warn!(
            log,
            "wm",
            "{stage}: {} unmanaged after a failure: {}",
            refusal.id,
            refusal.message
        );
    }
    for refusal in &outcome.decoration_errors {
        neb_core::log_warn!(
            log,
            "wm",
            "{stage}: decoration of {} failed: {}",
            refusal.id,
            refusal.message
        );
    }
    if let Some(animation) = outcome.animation {
        neb_core::log_debug!(
            log,
            "wm",
            "{stage}: {} window(s) animated in {} frame(s) over {} ms",
            animation.windows,
            animation.frames,
            animation.duration_ms
        );
    }
}

/// Creates the message-only window that receives posted events and hotkeys.
fn create_host_window() -> Result<(HWND, Vec<u16>), String> {
    let class = win::wide(CLASS_NAME);
    let title = win::wide(WINDOW_TITLE);

    // SAFETY: the module handle is only used to identify the registering module.
    let module = unsafe { GetModuleHandleW(None) }
        .map_err(|error| format!("cannot read this module's handle: {error}"))?;
    let instance = windows::Win32::Foundation::HINSTANCE(module.0);

    let window_class = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        hInstance: instance,
        lpszClassName: PCWSTR(class.as_ptr()),
        ..WNDCLASSW::default()
    };

    // SAFETY: `window_class` is fully initialised and its string pointers outlive the call.
    let atom = unsafe { RegisterClassW(&raw const window_class) };
    if atom == 0 {
        return Err("cannot register the manager's window class".to_owned());
    }

    // SAFETY: the class is registered above, the strings outlive the call, and `HWND_MESSAGE`
    // asks for a message-only window that is never shown.
    let hwnd = unsafe {
        CreateWindowExW(
            windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPED,
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|error| format!("cannot create the manager's message window: {error}"))?;

    Ok((hwnd, class))
}

/// Registers the hotkey table, logging every conflict instead of failing startup.
fn register_hotkeys(hwnd: HWND, log: &LogHandle) -> Vec<(i32, Hotkey)> {
    let mut registered = Vec::new();

    for (id, modifiers, vk, hotkey) in hotkey_table() {
        // SAFETY: the id is in the manager's own range and the window belongs to this thread.
        match unsafe { RegisterHotKey(Some(hwnd), id, HOT_KEY_MODIFIERS(modifiers), vk) } {
            Ok(()) => registered.push((id, hotkey)),
            Err(error) => neb_core::log_warn!(
                log,
                "wm",
                "hotkey {id} (modifiers 0x{modifiers:x}, vk 0x{vk:02x}) is already taken: {error}"
            ),
        }
    }

    registered
}

/// The hotkey table: id, modifier flags, virtual-key code and action.
fn hotkey_table() -> Vec<(i32, u32, u32, Hotkey)> {
    let alt = MOD_ALT.0;
    let both = MOD_ALT.0 | MOD_SHIFT.0;
    let mut table = Vec::with_capacity(41);

    let arrows = [
        (Direction::Left, 0x25u32),
        (Direction::Right, 0x27),
        (Direction::Up, 0x26),
        (Direction::Down, 0x28),
    ];

    for (offset, (direction, vk)) in arrows.into_iter().enumerate() {
        let id = 1 + offset as i32;
        table.push((id, alt, vk, Hotkey::Focus(direction)));
        table.push((11 + offset as i32, both, vk, Hotkey::Move(direction)));
    }

    for workspace in 0..9usize {
        let vk = 0x31 + workspace as u32;
        table.push((21 + workspace as i32, alt, vk, Hotkey::Workspace(workspace)));
        table.push((
            31 + workspace as i32,
            both,
            vk,
            Hotkey::MoveToWorkspace(workspace),
        ));
    }

    table.push((40, alt, 0x46, Hotkey::Float));
    table.push((41, alt, 0x54, Hotkey::Layout));

    table
}

/// Marks the layout dirty; the debounce timer collapses a burst of events into one pass.
fn mark_dirty() {
    let host = HOST.load(Ordering::SeqCst);
    if host == 0 {
        return;
    }

    // SAFETY: the window is alive for the whole process; re-setting the timer restarts its delay.
    unsafe {
        SetTimer(
            Some(HWND(host as *mut core::ffi::c_void)),
            TIMER_RELAYOUT,
            RELAYOUT_DELAY_MS,
            None,
        );
    }
}

/// Ignores location and hide events for the next [`SUPPRESSION_MS`].
fn suppress() {
    SUPPRESS_UNTIL.store(now_ms().saturating_add(SUPPRESSION_MS), Ordering::SeqCst);
}

/// Whether the manager is currently ignoring self-inflicted window movement.
fn suppressed() -> bool {
    now_ms() < SUPPRESS_UNTIL.load(Ordering::SeqCst)
}

/// Wall-clock milliseconds, used only for the suppression window.
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

/// Runs `f` with the engine locked, or returns `None` before startup.
pub fn with_engine<T>(f: impl FnOnce(&mut Engine) -> T) -> Option<T> {
    let lock = ENGINE.get()?;
    let mut engine = lock.lock().unwrap_or_else(|poison| poison.into_inner());

    Some(f(&mut engine))
}

/// Asks the message loop to apply the current model.
pub fn post_command() {
    post(WM_APP_COMMAND);
}

/// Asks the message loop to stop.
pub fn post_quit() {
    post(WM_APP_QUIT);
}

/// Posts `message` to the hidden window, if it exists.
fn post(message: u32) {
    let host = HOST.load(Ordering::SeqCst);
    if host == 0 {
        return;
    }

    // SAFETY: the handle is a live message-only window; the two payload words are zero.
    unsafe {
        let _ = PostMessageW(
            Some(HWND(host as *mut core::ffi::c_void)),
            message,
            WPARAM(0),
            LPARAM(0),
        );
    }
}

/// The default configuration file, when `--config` is not given.
fn default_config_path() -> PathBuf {
    neb_core::paths::try_root()
        .map(|root| root.join("config.json"))
        .unwrap_or_else(|_| PathBuf::from("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_hotkey_table_has_no_duplicate_ids_and_covers_the_documented_keys() {
        let table = hotkey_table();
        let mut ids: Vec<i32> = table.iter().map(|(id, ..)| *id).collect();
        ids.sort_unstable();
        ids.dedup();

        assert_eq!(ids.len(), table.len(), "hotkey ids must be unique");
        assert_eq!(
            table.len(),
            4 * 2 + 9 * 2 + 2,
            "four arrows, nine digits and two extras"
        );
        assert!(
            table
                .iter()
                .any(|(_, _, vk, hotkey)| *vk == 0x46 && *hotkey == Hotkey::Float),
            "Alt+F floats"
        );
        assert!(
            table
                .iter()
                .any(|(_, _, vk, hotkey)| *vk == 0x54 && *hotkey == Hotkey::Layout),
            "Alt+T cycles the layout"
        );
        assert!(
            !table.iter().any(|(_, _, vk, _)| *vk == 0x20),
            "Alt+Space must be left to the launcher"
        );
    }

    #[test]
    fn the_command_line_parses_a_config_and_a_call() {
        let args = Args::parse(&[
            "--config".to_owned(),
            r"C:\tmp\config.json".to_owned(),
            "--quit-after".to_owned(),
            "3".to_owned(),
        ])
        .expect("valid arguments");

        assert_eq!(args.config, PathBuf::from(r"C:\tmp\config.json"));
        assert_eq!(args.quit_after, Some(3));
        assert!(args.call.is_none());

        let call = Args::parse(&[
            "--call".to_owned(),
            "wm.setLayout".to_owned(),
            "--params".to_owned(),
            r#"{"layout":"grid"}"#.to_owned(),
        ])
        .expect("valid call");

        assert_eq!(
            call.call.as_ref().map(|call| call.method.as_str()),
            Some("wm.setLayout")
        );
        assert_eq!(
            call.call.and_then(|call| call.params),
            Some(serde_json::json!({ "layout": "grid" }))
        );
    }

    #[test]
    fn the_command_line_rejects_bad_input() {
        assert!(Args::parse(&["--config".to_owned()]).is_err());
        assert!(Args::parse(&["--quit-after".to_owned(), "soon".to_owned()]).is_err());
        assert!(Args::parse(&["--params".to_owned(), "{}".to_owned()]).is_err());
        assert!(Args::parse(&["--wat".to_owned()]).is_err());
    }

    #[test]
    fn a_bom_written_by_the_installer_does_not_stop_the_manager() {
        let path = std::env::temp_dir().join(format!("neb-wm-config-{}.json", std::process::id()));
        std::fs::write(
            &path,
            "\u{feff}{\"windowManager\":{\"enabled\":true,\"layout\":\"grid\"}}",
        )
        .expect("write the config");

        let config = load_config(&path).expect("a BOM must not make the config unreadable");
        assert!(config.window_manager.enabled);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn suppression_expires_after_the_window() {
        SUPPRESS_UNTIL.store(0, Ordering::SeqCst);
        assert!(!suppressed());

        suppress();
        assert!(
            suppressed(),
            "the manager ignores its own movement right after a pass"
        );

        SUPPRESS_UNTIL.store(now_ms().saturating_sub(1), Ordering::SeqCst);
        assert!(
            !suppressed(),
            "and listens again when the window has passed"
        );
    }
}
