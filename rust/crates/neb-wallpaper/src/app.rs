//! The application: configuration, the surfaces, and the loop that owns them.
//!
//! Everything runs on one thread, which is what D3D11, DXGI and the window system all want. The
//! loop waits in the kernel for a message or the next deadline and then does at most three things
//! that could be due - hand a frame to the GPU, sample the pause rules, reconcile the windows with
//! the desktop. Nothing is polled faster than it needs to be, so a paused wallpaper is a sleeping
//! process rather than a hidden animation.

use std::sync::Arc;
use std::time::{Duration, Instant};

use neb_core::config::WallpaperSettings;
use neb_core::ipc::ServerHandle;
use neb_core::logging::{self, LogHandle};
use neb_core::metrics::working_set_bytes;
use neb_core::paths;
use windows::Win32::Foundation::{HINSTANCE, HWND};
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use crate::config;
use crate::desktop::attach::{self, Slot};
use crate::desktop::window::{self, Events, FrameTimer, Monitor, WindowError};
use crate::ipc::{self, Shared};
use crate::power::{self, PauseState};
use crate::render::scene::{Frame, Palette};
use crate::render::{RenderError, Renderer};

/// How often the pause inputs are sampled. One second is far below the latency a user notices when
/// a game goes fullscreen and high enough that the probe costs nothing.
pub const PAUSE_INTERVAL: Duration = Duration::from_secs(1);

/// How long a burst of display messages is folded into one reconciliation.
pub const DEBOUNCE: Duration = Duration::from_millis(500);

/// How long a surface whose device was lost waits before another attempt.
const RENDERER_RETRY: Duration = Duration::from_secs(5);

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
            args.report_after = Some(seconds.map_or(
                Duration::from_secs(config::REPORT_AFTER_SECONDS),
                Duration::from_secs,
            ));
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

/// What one run produced, for `--report-memory` and the log.
#[derive(Debug, Clone, Copy, Default)]
pub struct Report {
    /// Working set of this process at the end of the run, in bytes.
    pub working_set: Option<usize>,
    /// How long the process ran.
    pub elapsed: Duration,
    /// How many surfaces were on screen.
    pub monitors: usize,
    /// How many of them were inside the desktop.
    pub attached: usize,
    /// Frames presented, summed over the surfaces.
    pub frames: u64,
    /// Whether the pause rules had the scene stopped when the run ended.
    pub paused: bool,
}

/// Why the wallpaper could not start.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    /// A window could not be created; nothing can be drawn without one.
    #[error("{0}")]
    Window(#[from] WindowError),

    /// The desktop listed no monitor at all.
    #[error("the system reports no monitor")]
    NoMonitors,
}

/// One monitor's surface: its window, the swapchain and where it ended up.
#[derive(Debug)]
struct Surface {
    window: HWND,
    monitor: Monitor,
    slot: Slot,
    renderer: Option<Renderer>,
    attached: bool,
    frames: u64,
    retry_at: Option<Instant>,
}

impl Surface {
    /// The placeholder a surface is swapped out with when its window died.
    fn empty(monitor: Monitor) -> Self {
        Self {
            window: HWND::default(),
            monitor,
            slot: Slot::default(),
            renderer: None,
            attached: false,
            frames: 0,
            retry_at: None,
        }
    }
}

/// Everything the loop owns.
#[derive(Debug)]
struct App {
    control: HWND,
    instance: HINSTANCE,
    log: Option<LogHandle>,
    shared: Arc<Shared>,
    settings: WallpaperSettings,
    palette: Palette,
    surfaces: Vec<Surface>,
    /// The scene clock, in seconds since the first frame.
    time: f32,
    paused: bool,
    locked: bool,
    state: PauseState,
    next_frame: Instant,
    last_frame: Instant,
    next_pause: Instant,
    debounce: Option<Instant>,
    frames: u64,
    timer: Option<FrameTimer>,
    ipc: Option<ServerHandle>,
}

/// Runs the wallpaper until `--report-memory` expires or IPC stops it.
///
/// # Errors
///
/// [`AppError`] when the control window cannot be created or the system reports no monitor; a
/// surface that fails is logged and skipped, because one bad monitor must not cost the user the
/// others.
pub fn run(args: Args) -> Result<Report, AppError> {
    let started = Instant::now();
    become_per_monitor_dpi_aware();

    let log = install_log();
    if let Some(log) = &log {
        log.info(
            "wallpaper",
            &format!("neb-wallpaper {} starting", crate::VERSION),
        );
    }

    let settings = config::load(log.as_ref());
    let palette = Palette::from_colour(&settings.color);
    let instance = window::instance();
    let control = window::create_control(instance)?;

    let shared = Arc::new(Shared::new(control));
    shared.set_colour(&settings.color);

    // The pipe comes up before the first frame, so a dashboard that connects immediately finds a
    // wallpaper that is already drawing.
    let ipc = match ipc::start(Arc::clone(&shared)) {
        Ok(server) => {
            if let Some(log) = &log {
                log.info(
                    "wallpaper",
                    &format!("ipc listening on '{}'", ipc::WALLPAPER_PIPE),
                );
            }

            Some(server)
        }
        Err(error) => {
            if let Some(log) = &log {
                log.warn(
                    "wallpaper",
                    &format!("ipc is unavailable ({error}); the wallpaper still draws"),
                );
            }

            None
        }
    };

    power::register_session_notifications(control, log.as_ref());

    if let Some(log) = &log {
        log.info(
            "wallpaper",
            &format!("scene colours {}", palette.describe()),
        );
    }

    // The frame clock: a high-resolution timer, which is what keeps 30 fps at 30 fps. Without it the
    // loop still draws, just at the system clock tick's idea of thirty frames.
    let timer = FrameTimer::new();
    if timer.is_none() {
        if let Some(log) = &log {
            log.warn(
                "wallpaper",
                "no high-resolution timer; frames fall back to the system clock tick",
            );
        }
    }

    if let (Some(log), Some(path)) = (&log, power::hooks()) {
        log.warn(
            "wallpaper",
            &format!(
                "acceptance hooks are on: writing 'fullscreen' or 'battery' to '{}' forces that \
                 pause input",
                path.display()
            ),
        );
    }

    let now = Instant::now();
    let mut app = App {
        control,
        instance,
        log,
        shared,
        settings,
        palette,
        surfaces: Vec::new(),
        time: 0.0,
        paused: false,
        locked: false,
        state: PauseState::default(),
        next_frame: now,
        last_frame: now,
        next_pause: now,
        debounce: None,
        frames: 0,
        timer,
        ipc,
    };

    app.sync();
    if app.surfaces.is_empty() {
        app.shutdown();
        return Err(AppError::NoMonitors);
    }

    app.loop_until(started, args.report_after);

    let report = Report {
        working_set: working_set_bytes(),
        elapsed: started.elapsed(),
        monitors: app.surfaces.len(),
        attached: app
            .surfaces
            .iter()
            .filter(|surface| surface.attached)
            .count(),
        frames: app.frames,
        paused: app.paused,
    };

    // The console is not always there to print to - a wallpaper started by the dashboard has none -
    // so the number the budget is measured against goes into the log as well.
    if let Some(log) = &app.log {
        log.info("wallpaper", &describe(&report));
    }

    app.shutdown();
    Ok(report)
}

/// The report as one line, shared by the log and `--report-memory`.
pub fn describe(report: &Report) -> String {
    let memory = match report.working_set {
        Some(bytes) => format!(
            "working set {:.1} MiB ({} bytes)",
            bytes as f64 / (1024.0 * 1024.0),
            bytes
        ),
        None => "the working set could not be read".to_owned(),
    };

    format!(
        "{memory} after {:.1} s, {} surface(s), {} attached, {} frame(s){}",
        report.elapsed.as_secs_f64(),
        report.monitors,
        report.attached,
        report.frames,
        if report.paused { ", paused" } else { "" }
    )
}

impl App {
    /// Reconciles the surfaces with the monitors that exist right now: creates what is new, moves
    /// what changed, destroys what belongs to a monitor that is gone.
    fn sync(&mut self) {
        let monitors = window::monitors();
        if monitors.is_empty() {
            self.warn("the system reports no monitor; nothing to cover");
            return;
        }

        let mut kept = Vec::with_capacity(monitors.len());

        for monitor in monitors {
            let existing = self
                .surfaces
                .iter()
                .position(|surface| surface.monitor.device == monitor.device);

            match existing {
                Some(index) => {
                    let mut surface = self.surfaces.remove(index);
                    if self.reshape(&mut surface, monitor) {
                        kept.push(surface);
                    }
                }
                None => match self.create(&monitor) {
                    Some(surface) => kept.push(surface),
                    None => self.warn(&format!("'{}' has no surface", monitor.device)),
                },
            }
        }

        for surface in std::mem::take(&mut self.surfaces) {
            self.info(&format!(
                "monitor '{}' is gone, closing it",
                surface.monitor.device
            ));
            self.close(surface);
        }

        self.surfaces = kept;

        let attached = self
            .surfaces
            .iter()
            .filter(|surface| surface.attached)
            .count();
        self.shared.set_monitors(self.surfaces.len(), attached);

        if let Some(first) = self.surfaces.first() {
            self.shared
                .record_placement(&first.slot, first.window, first.attached);
        }
    }

    /// Moves an existing surface onto `monitor`; returns whether it is still usable.
    fn reshape(&mut self, surface: &mut Surface, monitor: Monitor) -> bool {
        if !window::is_alive(surface.window) {
            // Explorer restarted and took the child window with it.
            let dead = std::mem::replace(surface, Surface::empty(monitor.clone()));
            self.info(&format!(
                "'{}' lost its window; rebuilding it",
                surface.monitor.device
            ));
            self.close(dead);

            return match self.create(&monitor) {
                Some(fresh) => {
                    *surface = fresh;
                    true
                }
                None => false,
            };
        }

        let moved = surface.monitor.rect != monitor.rect;
        surface.monitor = monitor;

        if moved || !surface.attached {
            self.attach(surface);
        }

        match &mut surface.renderer {
            Some(renderer) => {
                let width = surface.monitor.rect.width() as u32;
                let height = surface.monitor.rect.height() as u32;

                if let Err(error) = renderer.resize(width, height) {
                    self.warn(&format!(
                        "'{}' could not resize its scene ({error}); rebuilding it",
                        surface.monitor.device
                    ));
                    surface.renderer = None;
                    surface.retry_at = Some(Instant::now() + RENDERER_RETRY);
                }
            }
            None => self.build_renderer(surface),
        }

        true
    }

    /// Creates the window and the scene for one monitor.
    fn create(&mut self, monitor: &Monitor) -> Option<Surface> {
        let rect = monitor.rect;
        let window = match window::create_surface(&monitor.device, rect, self.instance) {
            Ok(window) => window,
            Err(error) => {
                self.warn(&format!("'{}': {error}", monitor.device));
                return None;
            }
        };

        let mut surface = Surface {
            window,
            monitor: monitor.clone(),
            slot: Slot::default(),
            renderer: None,
            attached: false,
            frames: 0,
            retry_at: None,
        };

        self.attach(&mut surface);
        self.build_renderer(&mut surface);

        // The first frame goes out before the loop starts, so the window is never on screen empty.
        let time = self.time;
        let palette = self.palette;
        if let Err(error) = present(&mut surface, time, &palette) {
            self.warn(&format!(
                "'{}' could not present its first frame ({error})",
                surface.monitor.device
            ));
        }

        Some(surface)
    }

    /// Puts the surface inside the desktop, or at the bottom of the Z order when there is no slot.
    fn attach(&mut self, surface: &mut Surface) {
        let rect = surface.monitor.rect;

        match attach::attach(surface.window, rect) {
            Ok(slot) => {
                surface.attached = true;
                surface.slot = slot;
                self.info(&format!(
                    "'{}' hwnd 0x{:x} parent 0x{:x} {} at {},{} {}x{}",
                    surface.monitor.device,
                    attach::handle(surface.window),
                    attach::handle(window::parent_of(surface.window)),
                    slot.describe(),
                    rect.left,
                    rect.top,
                    rect.width(),
                    rect.height()
                ));
            }
            Err(error) => {
                surface.attached = false;
                surface.slot = Slot::default();
                let placed = attach::attach_as_bottom(surface.window, rect);

                self.warn(&format!(
                    "'{}' has no desktop slot ({}); hwnd 0x{:x} shown as the bottom-most window \
                     instead (placed={placed})",
                    surface.monitor.device,
                    error.reason(),
                    attach::handle(surface.window)
                ));
            }
        }

        self.shared
            .record_placement(&surface.slot, surface.window, surface.attached);
    }

    /// Creates the D3D11 scene for a surface, logging and giving up on failure.
    fn build_renderer(&mut self, surface: &mut Surface) {
        let width = surface.monitor.rect.width() as u32;
        let height = surface.monitor.rect.height() as u32;

        match Renderer::new(surface.window, width, height, self.log.as_ref()) {
            Ok(renderer) => {
                surface.renderer = Some(renderer);
                surface.retry_at = None;
            }
            Err(error) => {
                surface.renderer = None;
                surface.retry_at = Some(Instant::now() + RENDERER_RETRY);
                self.warn(&format!(
                    "'{}' has no scene ({error}); the window stays empty",
                    surface.monitor.device
                ));
            }
        }
    }

    /// Advances the clock and presents every surface.
    fn draw_frame(&mut self) {
        let now = Instant::now();
        let delta = now.duration_since(self.last_frame).as_secs_f32().min(0.5);
        self.last_frame = now;
        self.time += delta;

        let time = self.time;
        let palette = self.palette;
        let mut presented = 0u64;
        let mut lost: Option<(usize, RenderError)> = None;

        for (index, surface) in self.surfaces.iter_mut().enumerate() {
            if surface.renderer.is_none() {
                continue;
            }

            match present(surface, time, &palette) {
                Ok(()) => presented += 1,
                Err(error) => {
                    surface.renderer = None;
                    surface.retry_at = Some(now + RENDERER_RETRY);
                    lost = Some((index, error));
                }
            }
        }

        if let Some((index, error)) = lost {
            let device = self.surfaces[index].monitor.device.clone();
            self.warn(&format!(
                "'{device}' lost its scene ({error}); rebuilding it shortly"
            ));
        }

        // A surface that had no scene gets one attempt per retry interval, right here, so a device
        // reset recovers without waiting for a display change.
        let due = self.surfaces.iter().position(|surface| {
            surface.renderer.is_none() && surface.retry_at.is_some_and(|at| now >= at)
        });

        if let Some(index) = due {
            let mut surface = self.surfaces.remove(index);
            self.build_renderer(&mut surface);

            if present(&mut surface, time, &palette).is_ok() {
                presented += 1;
            }

            self.surfaces.insert(index, surface);
        }

        self.frames += presented;
        self.shared.add_frames(presented);
    }

    /// Samples the pause inputs and applies a changed decision.
    fn apply_pause(&mut self) {
        self.state = power::sample(self.log.as_ref(), self.locked);
        self.shared.set_state(self.state);

        let should = power::should_pause(self.state, &self.settings);
        if should == self.paused {
            return;
        }

        self.paused = should;
        self.shared.set_paused(should);
        self.info(&format!(
            "{} ({})",
            if should { "pausing" } else { "resuming" },
            self.state.describe()
        ));

        if !should {
            // The scene does not jump forward by the length of the pause.
            self.last_frame = Instant::now();
            self.next_frame = Instant::now();
        }
    }

    /// Re-reads `config.json` and rebuilds the palette without a restart.
    fn reload(&mut self) {
        self.settings = config::load(self.log.as_ref());
        self.palette = Palette::from_colour(&self.settings.color);
        self.shared.set_colour(&self.settings.color);
        self.info(&format!(
            "reloaded: colour {}, pause on fullscreen={}, pause on battery={}",
            config::normalise_colour(&self.settings.color),
            self.settings.pause_on_fullscreen,
            self.settings.pause_on_battery
        ));

        self.apply_pause();
    }

    /// The loop proper: wait for a message or the next deadline, then do what is due.
    fn loop_until(&mut self, started: Instant, report_after: Option<Duration>) {
        loop {
            let now = Instant::now();

            if self.debounce.is_some_and(|at| now >= at) {
                self.debounce = None;
                self.info("re-syncing with the desktop");
                self.sync();
            }

            if now >= self.next_pause {
                self.next_pause = now + PAUSE_INTERVAL;
                self.apply_pause();
            }

            if !self.paused && now >= self.next_frame {
                self.draw_frame();
                self.next_frame = now + frame_interval();
            }

            if let Some(after) = report_after {
                if started.elapsed() >= after {
                    self.info(&format!("reporting after {after:?}"));
                    return;
                }
            }

            let events = window::pump(self.timeout(now), self.timer.as_ref());
            if self.handle(events) {
                return;
            }
        }
    }

    /// How long the loop may sleep.
    fn timeout(&self, now: Instant) -> Duration {
        let mut deadline = self.next_pause;

        if let Some(at) = self.debounce {
            deadline = deadline.min(at);
        }

        if !self.paused {
            deadline = deadline.min(self.next_frame);
        }

        deadline
            .saturating_duration_since(now)
            .max(Duration::from_millis(1))
    }

    /// Reacts to one pass of the message queue; returns whether the loop must end.
    fn handle(&mut self, events: Events) -> bool {
        if events.stop || events.quit {
            self.info("stop requested");
            return true;
        }

        if events.taskbar_recreated {
            self.info("explorer restarted: dropping every surface and re-attaching shortly");
            for surface in std::mem::take(&mut self.surfaces) {
                self.close(surface);
            }

            self.shared.set_monitors(0, 0);
            self.schedule_sync();
        }

        if events.display_changed {
            self.info("display settings changed");
            self.schedule_sync();
        }

        if events.reload {
            self.reload();
        }

        if let Some(locked) = events.session_locked {
            self.locked = locked;
            self.info(if locked {
                "session locked"
            } else {
                "session unlocked"
            });
            self.apply_pause();
        }

        false
    }

    /// Folds a burst of desktop changes into one reconciliation half a second later.
    fn schedule_sync(&mut self) {
        self.debounce = Some(Instant::now() + DEBOUNCE);
        self.info(&format!("re-syncing in {} ms", DEBOUNCE.as_millis()));
    }

    /// Destroys one surface: the window leaves the desktop before it dies, so it cannot flash back.
    fn close(&mut self, surface: Surface) {
        window::release(surface.window);
        window::destroy(surface.window);
    }

    /// Stops the pipe, releases the surfaces and restores the user's own wallpaper.
    fn shutdown(&mut self) {
        if let Some(server) = self.ipc.take() {
            server.shutdown();
        }

        for surface in std::mem::take(&mut self.surfaces) {
            self.close(surface);
        }

        self.shared.set_monitors(0, 0);
        window::destroy(self.control);
        self.info("stopped");
    }

    fn info(&self, message: &str) {
        if let Some(log) = &self.log {
            log.info("wallpaper", message);
        }
    }

    fn warn(&self, message: &str) {
        if let Some(log) = &self.log {
            log.warn("wallpaper", message);
        }
    }
}

/// Presents one frame on one surface.
///
/// Free-standing rather than a method so the loop can hold `&mut self.surfaces` while it reads the
/// clock and the palette, and so the per-frame path allocates nothing: the uniforms are written into
/// one stack `Frame`.
fn present(surface: &mut Surface, time: f32, palette: &Palette) -> Result<(), RenderError> {
    let Some(renderer) = &mut surface.renderer else {
        return Ok(());
    };

    let (width, height) = renderer.size();
    let mut frame = Frame::default();
    Frame::fill(&mut frame, time, width, height, palette);
    renderer.render(&frame)?;
    surface.frames += 1;

    Ok(())
}

/// The frame interval the scene targets.
fn frame_interval() -> Duration {
    Duration::from_millis(1000 / u64::from(config::FRAMES_PER_SECOND))
}

/// Makes this process DPI aware *before* any window exists, so every rectangle is physical pixels.
fn become_per_monitor_dpi_aware() {
    // SAFETY: the call only changes this process's DPI awareness; failing is harmless.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Starts logging into the shared log directory; a broken log is not a reason to stop.
fn install_log() -> Option<LogHandle> {
    match paths::try_root() {
        Ok(_) => match logging::init(&paths::logs_dir()) {
            Ok(handle) => Some(handle),
            Err(error) => {
                eprintln!("neb-wallpaper: logging is unavailable ({error})");
                None
            }
        },
        Err(error) => {
            eprintln!("neb-wallpaper: no data directory ({error})");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_defaults_to_running_forever() {
        assert_eq!(Args::parse(Vec::<String>::new()), Ok(Args::default()));
        assert_eq!(Args::default().report_after, None);
    }

    #[test]
    fn report_memory_defaults_to_the_documented_twelve_seconds() {
        let args = Args::parse(["--report-memory"]).expect("valid");

        assert_eq!(args.report_after, Some(Duration::from_secs(12)));
        assert_eq!(config::REPORT_AFTER_SECONDS, 12);
    }

    #[test]
    fn report_memory_takes_an_explicit_number_of_seconds() {
        let args = Args::parse(["--report-memory", "--seconds", "3"]).expect("valid");

        assert_eq!(args.report_after, Some(Duration::from_secs(3)));
    }

    #[test]
    fn a_broken_command_line_is_refused_with_the_offending_word() {
        assert_eq!(
            Args::parse(["--explode"]),
            Err(ArgError::Unknown("--explode".to_owned()))
        );
        assert_eq!(
            Args::parse(["--report-memory", "--seconds", "soon"]),
            Err(ArgError::Value {
                flag: "--seconds",
                value: "soon".to_owned(),
            })
        );
        assert_eq!(
            Args::parse(["--seconds"]),
            Err(ArgError::MissingValue { flag: "--seconds" })
        );
        assert_eq!(
            Args::parse(["--seconds", "3"]),
            Err(ArgError::MissingValue {
                flag: "--report-memory"
            })
        );
    }

    #[test]
    fn the_frame_interval_is_thirty_a_second() {
        assert_eq!(frame_interval(), Duration::from_millis(33));
        assert_eq!(PAUSE_INTERVAL, Duration::from_secs(1));
        assert_eq!(DEBOUNCE, Duration::from_millis(500));
    }

    #[test]
    fn the_report_line_carries_the_memory_the_budget_is_measured_against() {
        let report = Report {
            working_set: Some(24 * 1024 * 1024),
            elapsed: Duration::from_secs(12),
            monitors: 1,
            attached: 1,
            frames: 360,
            paused: false,
        };

        let line = describe(&report);

        assert!(line.contains("24.0 MiB"), "{line}");
        assert!(
            line.contains("25165824 bytes"),
            "the raw number must survive: {line}"
        );
        assert!(line.contains("1 attached"), "{line}");
        assert!(line.contains("360 frame(s)"), "{line}");
        assert!(!line.contains("paused"), "{line}");
    }

    #[test]
    fn an_unreadable_working_set_is_said_not_guessed() {
        let report = Report {
            working_set: None,
            paused: true,
            ..Report::default()
        };

        let line = describe(&report);

        assert!(line.contains("could not be read"), "{line}");
        assert!(line.contains(", paused"), "{line}");
    }
}
