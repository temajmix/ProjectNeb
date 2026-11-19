//! The application: configuration, the bar set, and the window loop that owns them.
//!
//! Everything here runs on the UI thread. Windows procedures never call into this module вЂ” they only
//! record what they want in [`window::take_pending`] вЂ” so the bar set can be mutated freely between
//! dispatches without any chance of re-entering itself.

use std::sync::Arc;
use std::time::{Duration, Instant};

use neb_core::config::{AnimationSettings, Config, ConfigStore, ShellSettings};
use neb_core::ipc::ServerHandle;
use neb_core::logging::{self, LogHandle};
use neb_core::strings::Language;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use crate::appbar::Edge;
use crate::bar::{self, WidgetSpec};
use crate::dock;
use crate::geometry::Monitor;
use crate::ipc::{self, Shared};
use crate::launcher;
use crate::metrics_feed::Metrics;
use crate::theme::Theme;
use crate::window::{self, BarSpec, Pending, WindowError};

/// How long `--report-memory` lets the process idle before it reports.
pub const REPORT_AFTER: Duration = Duration::from_secs(10);

/// What the process was asked to do.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Args {
    /// Report the idle working set after this long and exit; `--report-memory`.
    pub report_after: Option<Duration>,
}

impl Args {
    /// Parses the command line, without a dependency and with nothing to configure.
    ///
    /// # Errors
    ///
    /// [`ArgError`] for an unknown argument or a malformed duration.
    pub fn parse<I, S>(arguments: I) -> Result<Self, ArgError>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut args = Self::default();
        let mut report = false;
        let mut seconds: Option<u64> = None;
        let mut pending_value = false;

        for argument in arguments {
            let argument = argument.as_ref();

            if pending_value {
                seconds = Some(argument.parse().map_err(|_| ArgError::Value {
                    flag: "--seconds",
                    value: argument.to_owned(),
                })?);
                pending_value = false;
                continue;
            }

            match argument {
                "--report-memory" => report = true,
                "--seconds" => pending_value = true,
                other => return Err(ArgError::Unknown(other.to_owned())),
            }
        }

        if pending_value {
            return Err(ArgError::MissingValue { flag: "--seconds" });
        }

        if report {
            args.report_after = Some(seconds.map_or(REPORT_AFTER, Duration::from_secs));
        } else if seconds.is_some() {
            return Err(ArgError::MissingValue {
                flag: "--report-memory",
            });
        }

        Ok(args)
    }
}

/// Why the command line could not be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ArgError {
    /// An argument this build does not know.
    #[error("unknown argument '{0}'")]
    Unknown(String),
    /// A flag that takes a value was given a bad one.
    #[error("{flag} needs a number of seconds, got '{value}'")]
    Value {
        /// The flag that was being parsed.
        flag: &'static str,
        /// What was written instead of a number.
        value: String,
    },
    /// A flag was left without the value it needs.
    #[error("{flag} needs a value")]
    MissingValue {
        /// The flag that was left dangling.
        flag: &'static str,
    },
}

/// What one run produced.
#[derive(Debug, Clone, Copy)]
pub struct Report {
    /// Working set of this process at the end of the run, in bytes.
    pub working_set: Option<usize>,
    /// How long the process ran.
    pub elapsed: Duration,
    /// How many bars were laid out.
    pub bars: usize,
    /// How many dock windows were laid out.
    pub docks: usize,
    /// Whether the bars were left visible.
    pub visible: bool,
}

/// Why the shell could not start or could not keep running.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A window could not be created.
    #[error("{0}")]
    Window(#[from] WindowError),
}

/// Runs the shell until it is asked to quit.
///
/// # Errors
///
/// [`AppError`] when the controller window cannot be created; a bar that fails is logged and skipped,
/// because one bad monitor must not cost the user the other bars.
pub fn run(args: Args) -> Result<Report, AppError> {
    let started = Instant::now();
    become_per_monitor_dpi_aware();
    install_log();

    let mut app = App::new()?;

    if let Some(after) = args.report_after {
        let milliseconds = u32::try_from(after.as_millis()).unwrap_or(u32::MAX);
        window::arm_report_timer(milliseconds);
        crate::log_info!("shell", "reporting the working set after {after:?}");
    }

    window::run_loop(|pending| app.apply(pending));

    // The report is taken here, with the bars still on screen and the metrics worker still sampling:
    // anything measured after `shutdown` would describe a process that no longer exists in the state
    // the budget is about.
    let working_set = neb_core::metrics::working_set_bytes();
    let bars = app.bars.len();
    let docks = app.docks();
    let visible = app.visible;
    app.shutdown();

    Ok(Report {
        working_set,
        elapsed: started.elapsed(),
        bars,
        docks,
        visible,
    })
}

/// Tells Windows to hand out real pixels per monitor rather than a scaled bitmap.
///
/// A failure is not fatal: the bar still draws, only blurrier on a scaled display.
fn become_per_monitor_dpi_aware() {
    // SAFETY: the call takes an enum value and only affects this process's DPI model.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Starts the log, complaining on stderr when the directory cannot be written.
fn install_log() {
    let handle: Option<LogHandle> = match logging::init(&neb_core::paths::logs_dir()) {
        Ok(handle) => Some(handle),
        Err(error) => {
            eprintln!("neb-shell: {error}; continuing without a log file");
            None
        }
    };

    crate::install_log(handle);
}

/// The bars, the dock, the configuration and the workers.
struct App {
    store: ConfigStore,
    config: Config,
    theme: Theme,
    language: Language,
    bars: Vec<HWND>,
    pinned: Vec<dock::AppEntry>,
    dock: Option<HWND>,
    dock_worker: Option<dock::Worker>,
    metrics: Arc<Metrics>,
    shared: Arc<Shared>,
    visible: bool,
    server: Option<ServerHandle>,
    launcher: Option<HWND>,
    launcher_index: launcher::index::Cache,
    /// The window the launcher took the foreground from, so Esc can give it back.
    launcher_previous: isize,
    hotkey: Option<&'static str>,
    /// The bar's widget list, parsed once per configuration reload.
    widgets: Vec<WidgetSpec>,
    /// The user's motion preferences, already combined with the performance profile.
    animations: AnimationSettings,
    /// The dock's pointer/fullscreen poll period.
    dock_poll_ms: u32,
}

/// The parts of the resolved performance profile the shell applies.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Pacing {
    /// Motion, with the profile's switch folded into the user's `animations` section.
    animations: AnimationSettings,
    /// The dock's poll period, in milliseconds.
    dock_poll_ms: u32,
    /// The metrics worker's sample interval.
    metrics: Duration,
}

/// Resolves the profile into the values the bar, the dock and the metrics worker use.
fn pacing(config: &Config) -> Pacing {
    let performance = config.resolved_performance();

    Pacing {
        animations: performance.animations_with(config.animations),
        dock_poll_ms: u32::try_from(performance.dock_poll_ms).unwrap_or(u32::MAX),
        metrics: Duration::from_millis(performance.metrics_ms),
    }
}

impl App {
    fn new() -> Result<Self, AppError> {
        let store = ConfigStore::at_default_location();
        let config = store.load_checked().unwrap_or_else(|error| {
            // Defaults are the honest answer here: the bar can still draw, and the reason is logged.
            crate::log_warn!("shell", "config: {error}");
            Config::default()
        });

        let language = Language::from_tag(&config.language);
        let theme = Theme::load(config.shell.theme(), crate::log());
        let visible = config.shell.bar_enabled;
        let pinned = dock::catalog::load(&config);
        let widgets = parse_widgets(&config);
        let pacing = pacing(&config);

        let control = window::create_controller()?;
        let hotkey = launcher::window::register_hotkey(control);

        let metrics = Metrics::start();
        metrics.set_interval(pacing.metrics);
        let shared = Arc::new(Shared::new(Arc::clone(&metrics)));
        let server = match ipc::start(Arc::clone(&shared)) {
            Ok(server) => Some(server),
            Err(error) => {
                crate::log_warn!("ipc", "cannot listen on '{}': {error}", ipc::SHELL_PIPE);
                None
            }
        };

        let mut app = Self {
            store,
            config,
            theme,
            language,
            bars: Vec::new(),
            pinned,
            dock: None,
            dock_worker: None,
            metrics,
            shared,
            visible,
            server,
            launcher: None,
            launcher_index: launcher::index::Cache::new(),
            launcher_previous: 0,
            hotkey,
            widgets,
            animations: pacing.animations,
            dock_poll_ms: pacing.dock_poll_ms,
        };

        if app.visible {
            app.relayout();
        } else {
            app.rebuild_dock();
        }

        crate::log_info!(
            "shell",
            "started: {} bar(s), {} dock(s), theme '{}', language '{}', hotkey '{}'",
            app.bars.len(),
            app.docks(),
            app.theme.id,
            app.language.tag(),
            app.hotkey.unwrap_or("none")
        );

        Ok(app)
    }

    /// Applies what the window procedures asked for.
    ///
    /// The order matters: a fullscreen app that arrives in the same batch as a re-assert must be
    /// honoured first, or the re-assert would place the strip topmost over the game it just stepped
    /// out of the way of.
    fn apply(&mut self, pending: Pending) {
        if pending.reload() {
            self.reload();
        }

        if pending.rebuild() {
            self.relayout();
        }

        if let Some(visible) = pending.visible() {
            self.set_visible(visible);
        }

        if let Some(suspended) = pending.suspend() {
            self.suspend_all(suspended);
        }

        if pending.reassert() {
            self.reassert_all();
        }

        // The dock first adopts a fresh snapshot, then acts on the click that was made against the
        // tiles the user could actually see.
        if pending.dock_refresh() {
            self.refresh_dock();
        }

        if let Some(click) = pending.dock_click() {
            self.apply_dock_click(click);
        }

        // The launcher's flags are applied last: Enter sets both "launch" and "close" in the same
        // batch, and the window has to still exist when the path is read out of it.
        if let Some(slot) = pending.launcher_launch() {
            self.launch_launcher(slot);
        }

        if pending.launcher_index() {
            self.refresh_launcher_index();
        }

        if pending.launcher_toggle() {
            self.toggle_launcher();
        }

        if pending.launcher_open() {
            self.open_launcher();
        }

        if pending.launcher_close() {
            self.close_launcher();
        }
    }

    /// Re-asserts every bar's reservation, after the shell moved one of them.
    fn reassert_all(&self) {
        for hwnd in &self.bars {
            window::reassert(*hwnd);
        }
    }

    /// Steps every bar out of a fullscreen application's way, or back in front of it.
    fn suspend_all(&self, suspended: bool) {
        for hwnd in &self.bars {
            window::suspend(*hwnd, suspended);
        }
    }

    /// Re-reads `config.json` and the theme, then rebuilds the bars.
    ///
    /// A rebuild rather than an in-place update: the height, the edge and the language may all have
    /// changed, and every one of those is a window property.
    fn reload(&mut self) {
        match self.store.load_checked() {
            Ok(config) => self.config = config,
            Err(error) => {
                crate::log_warn!("shell", "reload: {error}");
                return;
            }
        }

        self.language = Language::from_tag(&self.config.language);
        self.theme = Theme::load(self.config.shell.theme(), crate::log());
        self.visible = self.config.shell.bar_enabled;
        self.pinned = dock::catalog::load(&self.config);
        self.widgets = parse_widgets(&self.config);

        let pacing = pacing(&self.config);
        self.animations = pacing.animations;
        self.dock_poll_ms = pacing.dock_poll_ms;
        self.metrics.set_interval(pacing.metrics);

        crate::log_info!(
            "shell",
            "reloaded: theme '{}', language '{}', visible {}, {} pinned app(s), {} widget(s), \
             animations {} at {:.2}x, dock poll {} ms, metrics every {} ms",
            self.theme.id,
            self.language.tag(),
            self.visible,
            self.pinned.len(),
            self.widgets.len(),
            if self.animations.enabled { "on" } else { "off" },
            self.animations.speed(),
            self.dock_poll_ms,
            pacing.metrics.as_millis()
        );

        self.relayout();
    }

    /// Destroys every bar and builds one per monitor again.
    fn relayout(&mut self) {
        self.destroy_bars();

        if !self.visible {
            self.shared.set_bars(0);
        } else {
            let monitors = crate::geometry::monitors();
            let shell = self.config.shell.clone();

            for monitor in &monitors {
                match self.create_bar(monitor, &shell) {
                    Ok(hwnd) => self.bars.push(hwnd),
                    Err(error) => crate::log_warn!("shell", "{}: {error}", monitor.device),
                }
            }

            self.publish_targets();
        }

        // The dock belongs to the same layout: a display change, a reload and a rebuild all end here,
        // so recreating it with the bars is what keeps it on a monitor that still exists.
        self.rebuild_dock();
    }

    /// Builds one bar for `monitor`.
    fn create_bar(&self, monitor: &Monitor, shell: &ShellSettings) -> Result<HWND, WindowError> {
        // A theme may name its own height; the configuration's value is the fallback, and the theme
        // range is already validated by `neb-core`.
        let height_dip = self.theme.bar_height.unwrap_or_else(|| shell.height());

        window::create_bar(BarSpec {
            monitor: monitor.clone(),
            edge: if shell.is_bottom() {
                Edge::Bottom
            } else {
                Edge::Top
            },
            height_dip,
            theme: self.theme.clone(),
            language: self.language,
            profile: shell.profile_name.clone(),
            widgets: self.widgets.clone(),
            animations: self.animations,
            host: crate::appbar::windows(),
        })
    }

    /// Shows or hides the bars, releasing the reserved edge while they are hidden.
    fn set_visible(&mut self, visible: bool) {
        if visible == self.visible {
            return;
        }

        self.visible = visible;

        if visible && self.bars.is_empty() {
            self.relayout();
            return;
        }

        for hwnd in &self.bars {
            if visible {
                window::set_bar_visible(*hwnd, true);
                window::set_edge(*hwnd, true);
            } else {
                // The edge goes back before the window does, so the desktop never reflows around a
                // bar the user can no longer see.
                window::set_edge(*hwnd, false);
                window::set_bar_visible(*hwnd, false);
            }
        }

        self.publish_targets();
        crate::log_info!("shell", "bars {}", if visible { "shown" } else { "hidden" });
    }

    /// Tells the metrics worker and the IPC surface which bars exist.
    fn publish_targets(&self) {
        let targets: Vec<isize> = self.bars.iter().map(|hwnd| hwnd.0 as isize).collect();

        self.metrics.set_targets(&targets);
        self.shared.set_bars(self.bars.len());
    }

    /// Releases every reserved edge and closes every bar window.
    fn destroy_bars(&mut self) {
        for hwnd in self.bars.drain(..) {
            window::destroy_bar(hwnd);
        }

        self.shared.set_bars(0);
        self.metrics.set_targets(&[]);
    }

    /// How many dock windows exist, for the report.
    fn docks(&self) -> usize {
        usize::from(self.dock.is_some())
    }

    /// Destroys the dock and stops the worker that feeds it.
    fn destroy_dock(&mut self) {
        if let Some(mut worker) = self.dock_worker.take() {
            worker.stop();
        }

        if let Some(hwnd) = self.dock.take() {
            dock::window::destroy_dock(hwnd);
        }
    }

    /// Builds the dock for the primary monitor, with the applications that are running now.
    ///
    /// The first snapshot is taken here, on the UI thread, so the strip is drawn with content on the
    /// first frame instead of appearing empty until the worker's first pass; a slow pass costs one
    /// startup frame, which is the only place that is acceptable.
    fn rebuild_dock(&mut self) {
        self.destroy_dock();

        if !self.config.shell.dock_enabled {
            return;
        }

        let Some(monitor) = primary_monitor() else {
            crate::log_warn!("dock", "no monitor to place the dock on");
            return;
        };

        let running = dock::running::enumerate();
        let items = dock::compose(&self.pinned, &running);
        let spec = dock::window::DockSpec {
            monitor,
            theme: self.theme.clone(),
            language: self.language,
            items,
            auto_hide: self.config.shell.dock_auto_hide,
            tile_dip: self.theme.dock_tile.unwrap_or(dock::layout::TILE_DIP),
            animations: self.animations,
            poll_ms: self.dock_poll_ms,
            host: dock::window::windows(),
        };

        match dock::window::create_dock(spec) {
            Ok(hwnd) => {
                self.dock = Some(hwnd);
                self.dock_worker = Some(dock::Worker::start(hwnd.0 as isize));
            }
            Err(error) => crate::log_warn!("dock", "cannot create the dock: {error}"),
        }
    }

    /// Adopts the newest running-application snapshot.
    fn refresh_dock(&self) {
        let (Some(hwnd), Some(worker)) = (self.dock, self.dock_worker.as_ref()) else {
            return;
        };

        let running = worker.latest();
        dock::window::set_items(hwnd, dock::compose(&self.pinned, &running));
    }

    /// Does what a dock click asked for.
    fn apply_dock_click(&self, click: dock::Click) {
        match click {
            dock::Click::Activate(target) => {
                dock::running::activate(target);
            }
            dock::Click::Launch(slot) => {
                let path = self
                    .dock
                    .and_then(|hwnd| dock::window::item(hwnd, slot))
                    .and_then(|item| item.path);

                match path {
                    Some(path) => {
                        dock::launch(&path);
                    }
                    None => crate::log_warn!("dock", "tile {slot} has nothing to launch"),
                }
            }
            dock::Click::Overflow => {
                let overflow = self
                    .dock
                    .and_then(dock::window::plan_of)
                    .map_or(0, |plan| plan.overflow);
                dock::open_launcher(overflow);
            }
            dock::Click::Nothing => {}
        }
    }

    /// The launcher hotkey: open the panel, or close it when it is already open.
    fn toggle_launcher(&mut self) {
        if self.launcher.is_some() {
            self.close_launcher();
        } else {
            self.open_launcher();
        }
    }

    /// Opens the launcher on the monitor the pointer is on.
    ///
    /// The index is handed over as it is right now: the entries from the cache when they are fresh,
    /// or whatever the last build produced while a worker looks again. The panel never waits for the
    /// walk.
    fn open_launcher(&mut self) {
        if let Some(hwnd) = self.launcher {
            launcher::window::force_foreground(hwnd);
            return;
        }

        let Some(monitor) = monitor_with_cursor() else {
            crate::log_warn!("launcher", "no monitor to place the launcher on");
            return;
        };

        let snapshot = self.launcher_index.open();
        self.launcher_previous = launcher::window::foreground();

        let spec = launcher::window::LauncherSpec {
            monitor,
            theme: self.theme.clone(),
            language: self.language,
            entries: snapshot.entries,
            building: snapshot.building,
            animations: self.animations,
        };

        match launcher::window::create_launcher(spec) {
            Ok(hwnd) => self.launcher = Some(hwnd),
            Err(error) => crate::log_warn!("launcher", "cannot open the panel: {error}"),
        }
    }

    /// Adopts a finished index build for the open panel.
    fn refresh_launcher_index(&mut self) {
        let Some(entries) = self.launcher_index.finished() else {
            return;
        };

        if let Some(hwnd) = self.launcher {
            launcher::window::set_entries(hwnd, entries, false);
        }
    }

    /// Launches the result in `slot`, which the window resolved from its own query.
    fn launch_launcher(&mut self, slot: usize) {
        let Some(hwnd) = self.launcher else {
            return;
        };

        let Some(path) = launcher::window::item(hwnd, slot) else {
            crate::log_warn!("launcher", "result {slot} is gone");
            return;
        };

        let name = launcher::window::name(hwnd, slot).unwrap_or_default();
        crate::log_info!("launcher", "launching '{name}' from '{}'", path.display());
        launcher::launch(&path);
    }

    /// Closes the panel and gives the foreground back to the window it took it from.
    fn close_launcher(&mut self) {
        let Some(hwnd) = self.launcher.take() else {
            return;
        };

        launcher::window::destroy_launcher(hwnd);

        let previous = std::mem::take(&mut self.launcher_previous);
        launcher::window::restore_foreground(previous);

        // The panel's Direct2D target, its glyph cache and the shell's shortcut caches are freed with
        // the window, but Windows keeps the freed pages of an ordinarily-running process resident.
        // The launcher is a transient surface; once it is gone the shell is idle again, so the pages
        // are handed back to the system instead of counting against the resident shell's budget.
        launcher::window::trim_working_set();

        crate::log_info!("launcher", "closed");
    }

    /// Stops the workers and closes the pipe.
    fn shutdown(&mut self) {
        self.close_launcher();
        self.destroy_dock();
        self.destroy_bars();
        self.metrics.stop();

        if let Some(control) = window::control() {
            launcher::window::unregister_hotkey(control);
        }

        if let Some(server) = self.server.take() {
            server.shutdown();
        }

        crate::log_info!("shell", "stopped");
    }
}

/// Parses the widget list and reports every entry that was left out, once per configuration load.
fn parse_widgets(config: &Config) -> Vec<WidgetSpec> {
    let (widgets, skipped) = bar::parse(&config.shell.bar.widgets);

    for kind in &skipped.unknown {
        crate::log_warn!("bar", "unknown widget type '{kind}', skipped");
    }

    if skipped.overflow > 0 {
        crate::log_warn!(
            "bar",
            "{} widget(s) over the limit of {}, skipped",
            skipped.overflow,
            bar::widgets::MAX_WIDGETS
        );
    }

    widgets
}

/// The monitor the origin is on, which is where the dock goes.
///
/// The dock is one strip, not one per monitor: it is where the taskbar is, and this desktop has a
/// primary display that every other window is placed against. A monitor that is switched off is not
/// in the enumeration, so the dock can never be left behind on it.
fn primary_monitor() -> Option<Monitor> {
    let monitors = crate::geometry::monitors();

    monitors
        .iter()
        .find(|monitor| {
            monitor.rect.left <= 0
                && 0 < monitor.rect.right
                && monitor.rect.top <= 0
                && 0 < monitor.rect.bottom
        })
        .or_else(|| monitors.first())
        .cloned()
}

/// The monitor the pointer is on, where the launcher opens, falling back to the primary display.
fn monitor_with_cursor() -> Option<Monitor> {
    let mut point = windows::Win32::Foundation::POINT::default();
    // SAFETY: `point` is a live out-parameter and the call reads the pointer position only.
    let positioned =
        unsafe { windows::Win32::UI::WindowsAndMessaging::GetCursorPos(&raw mut point) }.is_ok();

    if positioned {
        let monitors = crate::geometry::monitors();
        if let Some(monitor) = monitors.iter().find(|monitor| {
            monitor.rect.left <= point.x
                && point.x < monitor.rect.right
                && monitor.rect.top <= point.y
                && point.y < monitor.rect.bottom
        }) {
            return Some(monitor.clone());
        }
    }

    primary_monitor()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Args, ArgError> {
        Args::parse(arguments.iter().copied())
    }

    #[test]
    fn no_arguments_means_run_normally() {
        assert_eq!(parse(&[]), Ok(Args::default()));
        assert_eq!(Args::default().report_after, None);
    }

    #[test]
    fn the_memory_report_flag_uses_ten_seconds_by_default() {
        assert_eq!(
            parse(&["--report-memory"]),
            Ok(Args {
                report_after: Some(REPORT_AFTER),
            })
        );
        assert_eq!(REPORT_AFTER, Duration::from_secs(10));
    }

    #[test]
    fn the_seconds_flag_overrides_the_default() {
        assert_eq!(
            parse(&["--report-memory", "--seconds", "2"]),
            Ok(Args {
                report_after: Some(Duration::from_secs(2)),
            })
        );
    }

    #[test]
    fn a_bad_or_missing_value_is_a_typed_error() {
        assert!(matches!(
            parse(&["--report-memory", "--seconds", "soon"]),
            Err(ArgError::Value { .. })
        ));
        assert!(matches!(
            parse(&["--report-memory", "--seconds"]),
            Err(ArgError::MissingValue { flag: "--seconds" })
        ));
        assert!(matches!(
            parse(&["--seconds", "2"]),
            Err(ArgError::MissingValue {
                flag: "--report-memory"
            })
        ));
    }

    #[test]
    fn an_unknown_argument_is_reported_by_name() {
        assert_eq!(
            parse(&["--fly"]),
            Err(ArgError::Unknown("--fly".to_owned()))
        );
    }

    #[test]
    fn a_profile_sets_motion_the_dock_poll_and_the_metrics_interval() {
        let config = Config::default();
        let balanced = pacing(&config);
        assert!(balanced.animations.enabled);
        assert_eq!(balanced.dock_poll_ms, 100);
        assert_eq!(balanced.metrics, Duration::from_millis(1_000));

        let mut config = Config::default();
        config.performance.profile = neb_core::config::PerformanceProfile::Performance;
        let performance = pacing(&config);
        assert!(
            !performance.animations.enabled,
            "the profile turns motion off"
        );
        assert_eq!(performance.dock_poll_ms, 250);
        assert_eq!(performance.metrics, Duration::from_millis(2_000));

        config.performance.profile = neb_core::config::PerformanceProfile::Quality;
        let quality = pacing(&config);
        assert!(quality.animations.enabled);
        assert_eq!(quality.dock_poll_ms, 60);
        assert_eq!(quality.metrics, Duration::from_millis(500));
    }

    #[test]
    fn a_reload_is_just_a_second_pacing_call_with_the_new_config() {
        let mut config = Config::default();
        let before = pacing(&config);

        // What `shell.reload` does after re-reading a file that switched to performance.
        config.performance.profile = neb_core::config::PerformanceProfile::Performance;
        let after = pacing(&config);

        assert_ne!(before.dock_poll_ms, after.dock_poll_ms);
        assert!(before.animations.enabled && !after.animations.enabled);
        assert_ne!(before.metrics, after.metrics);
    }

    #[test]
    fn the_user_animation_switch_and_overrides_still_win() {
        let mut config = Config::default();
        config.animations.enabled = false;
        config.performance.animations = Some(true);
        assert!(
            !pacing(&config).animations.enabled,
            "the animations section can still switch motion off"
        );

        let mut config = Config::default();
        config.performance.profile = neb_core::config::PerformanceProfile::Quality;
        config.performance.animations = Some(false);
        assert!(
            !pacing(&config).animations.enabled,
            "an explicit performance.animations override wins over the profile"
        );
    }
}
