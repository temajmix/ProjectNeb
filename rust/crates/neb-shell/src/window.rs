//! Windows: the bar windows, the controller that owns the loop, and the messages they exchange.
//!
//! There is one UI thread and two kinds of window on it:
//!
//! * a **bar** per monitor — the visible strip, which paints itself and owns its text;
//! * a **controller** — never shown, never painted, and the stable target for everything that has no
//!   business being tied to a monitor: the metrics feed, the IPC requests, the display-change
//!   debounce and the memory report timer.
//!
//! Nothing in a window procedure reaches into the application state. A procedure touches only its own
//! window's state and, when something global has to happen, records it in [`take_pending`] for the
//! message loop to apply between dispatches. That is what keeps `&mut App` unique and the paint path
//! free of surprises.
//!
//! A bar's own state is reached through [`StateGuard`], which refuses a second borrow of the same
//! window: `SetWindowPos` delivers `WM_SIZE` before it returns and `ABM_SETPOS` delivers
//! `ABN_POSCHANGED`, so without that refusal a message could borrow the state its own sender is
//! already holding.

use std::cell::Cell;
use std::ops::{Deref, DerefMut};
use std::sync::{Arc, OnceLock};

use neb_core::strings::Language;
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetClientRect, GetMessageW, GetWindowLongPtrW, HTCLIENT, IsWindowVisible, KillTimer, MSG,
    PostMessageW, RegisterClassW, RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE, SendMessageW,
    SetTimer, SetWindowLongPtrW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WM_APP, WM_CLOSE,
    WM_DESTROY, WM_DISPLAYCHANGE, WM_DPICHANGED, WM_ERASEBKGND, WM_HOTKEY, WM_NCCREATE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_SIZE, WM_TIMER, WNDCLASSW, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::appbar::{self, Edge, EdgeHost};
use crate::geometry::{Monitor, Rect};
use crate::render::{Layouts, PaintOutcome, RenderError, Renderer, TWO_LINE_MIN_HEIGHT};
use crate::theme::Theme;
use crate::widgets::{BarText, ClockText, MetricsText};

/// Class name of a bar window.
pub const BAR_CLASS: &str = "ProjectNeb.NativeBar";

/// Class name of the controller window.
pub const CONTROL_CLASS: &str = "ProjectNeb.NativeBarControl";

/// A metrics sample arrived: `wParam` is the CPU share, `lParam` the memory share.
pub const WM_METRICS: u32 = WM_APP + 1;
/// The display layout changed and the bars should be rebuilt after the debounce.
pub const WM_REBUILD: u32 = WM_APP + 2;
/// The configuration was re-read and the bars should adopt it.
pub const WM_RELOAD: u32 = WM_APP + 3;
/// `wParam` is `1` to show the bars and `0` to hide them.
pub const WM_VISIBILITY: u32 = WM_APP + 4;

/// Timer ids.
const TIMER_CLOCK: usize = 1;
const TIMER_REBUILD: usize = 2;
const TIMER_REPORT: usize = 3;

/// How long two display changes have to be apart before the bars are rebuilt.
pub const REBUILD_DEBOUNCE_MS: u32 = 500;

/// Shortest clock interval: below this the timer would fire twice for one second boundary.
const MIN_CLOCK_INTERVAL_MS: u32 = 50;

/// The window procedure shape `RegisterClassW` wants.
type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

/// Why a window could not be created.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// `RegisterClassW` refused the class.
    #[error("cannot register the window class '{class}'")]
    Class {
        /// The class that was refused.
        class: &'static str,
    },
    /// `CreateWindowExW` refused the window.
    #[error("cannot create a '{class}' window: {source}")]
    Create {
        /// The class that was asked for.
        class: &'static str,
        /// The refusal.
        #[source]
        source: windows::core::Error,
    },
    /// Direct2D refused to draw on the new window.
    #[error("{0}")]
    Render(#[from] RenderError),
    /// The window was created but came back without its state, or with a borrow already live.
    #[error("the '{0}' window has no state")]
    NoState(&'static str),
}

/// Work the message loop applies between dispatches, recorded by window procedures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pending {
    reload: bool,
    rebuild: bool,
    visible: Option<bool>,
    reassert: bool,
    suspend: Option<bool>,
    dock_click: Option<crate::dock::Click>,
    dock_refresh: bool,
    launcher_toggle: bool,
    launcher_open: bool,
    launcher_close: bool,
    launcher_index: bool,
    launcher_launch: Option<usize>,
    quit: bool,
}

impl Pending {
    /// Nothing to do.
    pub const fn none() -> Self {
        Self {
            reload: false,
            rebuild: false,
            visible: None,
            reassert: false,
            suspend: None,
            dock_click: None,
            dock_refresh: false,
            launcher_toggle: false,
            launcher_open: false,
            launcher_close: false,
            launcher_index: false,
            launcher_launch: None,
            quit: false,
        }
    }

    /// Whether anything is waiting.
    pub fn is_empty(self) -> bool {
        self == Self::none()
    }

    /// Whether the loop should leave.
    pub fn quit(self) -> bool {
        self.quit
    }

    /// Whether the configuration should be re-read.
    pub fn reload(self) -> bool {
        self.reload
    }

    /// Whether the bars should be rebuilt.
    pub fn rebuild(self) -> bool {
        self.rebuild
    }

    /// The visibility that was requested, if any.
    pub fn visible(self) -> Option<bool> {
        self.visible
    }

    /// Whether the reserved strip should be re-asserted on every bar.
    pub fn reassert(self) -> bool {
        self.reassert
    }

    /// Whether a fullscreen app arrived (`Some(true)`) or left (`Some(false)`).
    pub fn suspend(self) -> Option<bool> {
        self.suspend
    }

    /// What a click on the dock asked for, if anything.
    pub fn dock_click(self) -> Option<crate::dock::Click> {
        self.dock_click
    }

    /// Records a dock click for the loop to apply.
    pub fn set_dock_click(&mut self, click: crate::dock::Click) {
        self.dock_click = Some(click);
    }

    /// Whether a new running-application snapshot is waiting.
    pub fn dock_refresh(self) -> bool {
        self.dock_refresh
    }

    /// Records that a snapshot is waiting.
    pub fn set_dock_refresh(&mut self, waiting: bool) {
        self.dock_refresh = waiting;
    }

    /// Whether the launcher hotkey asked for a toggle.
    pub fn launcher_toggle(self) -> bool {
        self.launcher_toggle
    }

    /// Records a launcher toggle.
    pub fn set_launcher_toggle(&mut self) {
        self.launcher_toggle = true;
    }

    /// Whether the dock's overflow tile asked for the launcher.
    pub fn launcher_open(self) -> bool {
        self.launcher_open
    }

    /// Records an explicit open request.
    pub fn set_launcher_open(&mut self) {
        self.launcher_open = true;
    }

    /// Whether the launcher asked to be closed.
    pub fn launcher_close(self) -> bool {
        self.launcher_close
    }

    /// Records a close request.
    pub fn set_launcher_close(&mut self) {
        self.launcher_close = true;
    }

    /// Whether the index worker finished a build.
    pub fn launcher_index(self) -> bool {
        self.launcher_index
    }

    /// Records that the index worker finished.
    pub fn set_launcher_index(&mut self) {
        self.launcher_index = true;
    }

    /// The result slot the launcher asked to launch, if any.
    pub fn launcher_launch(self) -> Option<usize> {
        self.launcher_launch
    }

    /// Records a launch request.
    pub fn set_launcher_launch(&mut self, slot: usize) {
        self.launcher_launch = Some(slot);
    }
}

thread_local! {
    /// The pending work of this thread. There is one UI thread, so one cell is the whole store.
    static PENDING: Cell<Pending> = const { Cell::new(Pending::none()) };
}

/// Records work for the message loop.
pub fn set_pending(update: impl FnOnce(&mut Pending)) {
    PENDING.with(|pending| {
        let mut next = pending.get();
        update(&mut next);
        pending.set(next);
    });
}

/// Takes the pending work, leaving nothing behind.
pub fn take_pending() -> Pending {
    PENDING.with(|pending| pending.replace(Pending::none()))
}

/// Asks the loop to leave.
pub fn request_quit() {
    set_pending(|pending| pending.quit = true);
}

static CALLBACK_MESSAGE: OnceLock<u32> = OnceLock::new();
static TASKBAR_CREATED: OnceLock<u32> = OnceLock::new();
static BAR_CLASS_REGISTERED: OnceLock<bool> = OnceLock::new();
static CONTROL_CLASS_REGISTERED: OnceLock<bool> = OnceLock::new();
static CONTROL: OnceLock<isize> = OnceLock::new();

/// The message the shell sends `ABN_*` notifications through.
pub fn callback_message() -> u32 {
    *CALLBACK_MESSAGE.get_or_init(|| {
        let name = wide("ProjectNeb_AppBar");
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call.
        unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) }
    })
}

/// The message Explorer broadcasts after it restarts, when every app bar registration is gone.
pub fn taskbar_created() -> u32 {
    *TASKBAR_CREATED.get_or_init(|| {
        let name = wide("TaskbarCreated");
        // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call.
        unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) }
    })
}

/// What a bar is created with; it travels through `CreateWindowExW` as the create parameter.
#[derive(Debug, Clone)]
pub struct BarSpec {
    /// The monitor the bar belongs to.
    pub monitor: Monitor,
    /// Which edge of that monitor it takes.
    pub edge: Edge,
    /// The height the configuration asked for, in DIPs.
    pub height_dip: i32,
    /// The palette it paints with.
    pub theme: Theme,
    /// The language its strings are in.
    pub language: Language,
    /// The profile name, or `None` for the localised default.
    pub profile: Option<String>,
    /// Where the AppBar calls go; the shell itself, or a counter in a test.
    pub host: Arc<dyn EdgeHost>,
}

/// The strip `spec` asks for, in physical pixels.
fn strip_of(monitor: &Monitor, edge: Edge, height_dip: i32) -> Rect {
    let thickness = monitor.to_pixels(height_dip);
    let rect = monitor.rect;

    match edge {
        Edge::Top => Rect::new(rect.left, rect.top, rect.right, rect.top + thickness),
        Edge::Bottom => Rect::new(rect.left, rect.bottom - thickness, rect.right, rect.bottom),
    }
}

/// Everything one bar window owns.
#[derive(Debug)]
struct BarState {
    hwnd: HWND,
    monitor: Monitor,
    edge: Edge,
    height_dip: i32,
    language: Language,
    theme: Theme,
    text: BarText,
    clock: ClockText,
    metrics: MetricsText,
    renderer: Option<Renderer>,
    layouts: Option<Layouts>,
    host: Arc<dyn EdgeHost>,
    reserved: bool,
    topmost_suspended: bool,
    /// Set while a window procedure holds this state, so a re-entrant message cannot borrow it again.
    busy: bool,
    /// How many frames this bar has drawn, which is the only proof a paint test can offer.
    frames: u64,
}

impl BarState {
    fn new(hwnd: HWND, spec: BarSpec) -> Self {
        Self {
            hwnd,
            monitor: spec.monitor,
            edge: spec.edge,
            height_dip: spec.height_dip,
            language: spec.language,
            theme: spec.theme,
            text: BarText::new(spec.profile.as_deref(), spec.language),
            clock: ClockText::default(),
            metrics: MetricsText::default(),
            renderer: None,
            layouts: None,
            host: spec.host,
            reserved: false,
            topmost_suspended: false,
            busy: false,
            frames: 0,
        }
    }

    /// The strip this bar wants, in physical pixels.
    fn strip(&self) -> Rect {
        strip_of(&self.monitor, self.edge, self.height_dip)
    }

    /// Whether the strip should be topmost: never while a fullscreen app owns the screen.
    fn topmost(&self) -> bool {
        !self.topmost_suspended
    }

    /// Creates the renderer once the window has a size, and reserves the edge.
    fn attach(&mut self) -> Result<(), WindowError> {
        let rect = self.strip();
        appbar::place(self.host.as_ref(), self.hwnd, rect, self.topmost());

        let mut renderer = Renderer::new(self.hwnd, self.dpi(), &self.theme)?;
        renderer.resize((
            u32::try_from(rect.width()).unwrap_or(1),
            u32::try_from(rect.height()).unwrap_or(1),
        ))?;
        self.renderer = Some(renderer);

        // The two-line clock needs a tall enough bar; the date is hidden in a short one.
        let _ = self.clock.update(&mut self.text, self.language);
        self.rebuild_layouts();

        if let Some(layouts) = self.layouts.as_ref() {
            let (width, height) = self.size_dip();
            let (clock, date) = layouts.heights();
            crate::log_info!(
                "bar",
                "{}: laid out {width:.0}x{height:.0} dip, clock '{clock:.1}', date '{date:.1}' ({})",
                self.monitor.device,
                if layouts.show_date() {
                    "two lines"
                } else {
                    "one line"
                }
            );
        }

        self.reserve();
        Ok(())
    }

    /// Registers and reserves, logging either answer.
    fn reserve(&mut self) {
        let rect = self.strip();

        match appbar::reserve(
            self.host.as_ref(),
            self.hwnd,
            self.monitor.rect,
            self.edge,
            rect.height(),
            callback_message(),
            self.topmost(),
        ) {
            Ok(strip) => {
                self.reserved = true;
                crate::log_info!(
                    "appbar",
                    "{}: reserved {}px on the {:?} edge",
                    self.monitor.device,
                    strip.rect.height(),
                    self.edge
                );
            }
            Err(error) => {
                // A bar that cannot reserve is a cosmetic problem; one that crashes is not.
                self.reserved = false;
                crate::log_warn!(
                    "appbar",
                    "{}: {error}; the bar stays topmost only",
                    self.monitor.device
                );
            }
        }
    }

    /// The window's current DPI, refreshed from the system.
    fn dpi(&self) -> u32 {
        // SAFETY: `hwnd` is a live window of this process.
        let dpi = unsafe { GetDpiForWindow(self.hwnd) };

        dpi.max(96)
    }

    /// The bar's client size in DIPs, which is what the layout is measured against.
    fn size_dip(&self) -> (f32, f32) {
        match self.renderer.as_ref() {
            Some(renderer) => renderer.size_dip(),
            None => {
                let thickness = self.monitor.to_pixels(self.height_dip);
                let scale = 96.0 / self.dpi() as f32;
                (thickness as f32 * scale, self.height_dip.max(1) as f32)
            }
        }
    }

    /// Rebuilds the text layouts; called whenever the text or the size changed, never from `paint`.
    ///
    /// The callers are what make this cheap: the clock tick rebuilds only when the minute changed,
    /// the metrics only when a new reading differs, and a resize always.
    fn rebuild_layouts(&mut self) {
        let size = self.size_dip();
        let show_date = size.1 >= TWO_LINE_MIN_HEIGHT;

        match Layouts::build(&self.text, show_date, size) {
            Ok(layouts) => self.layouts = Some(layouts),
            Err(error) => crate::log_warn!("bar", "layout: {error}"),
        }
    }

    /// Draws the frame Windows asked for.
    fn paint(&mut self) {
        let outcome = match (self.renderer.as_mut(), self.layouts.as_ref()) {
            (Some(renderer), Some(layouts)) => renderer.paint(&self.theme, layouts),
            _ => return,
        };

        match outcome {
            PaintOutcome::Painted => self.frames += 1,
            PaintOutcome::Recreate => {
                // The device went away, and with it every layout measured against it. Both are
                // rebuilt here, outside the frame, so the next `WM_PAINT` has something to draw: a
                // lost target that only clears the cache would leave the bar blank until a resize.
                crate::log_info!("bar", "the render target was lost, rebuilding");
                self.rebuild_layouts();
                self.invalidate();
            }
            PaintOutcome::Failed(error) => crate::log_warn!("bar", "paint: {error}"),
        }
    }

    /// The clock tick.
    fn tick(&mut self) {
        if self.clock.update(&mut self.text, self.language) {
            self.rebuild_layouts();
            self.invalidate();
        }
    }

    /// A metrics sample.
    fn apply_metrics(&mut self, cpu: u32, ram: u32) {
        let reading = crate::widgets::metrics::Reading::from_packed(cpu, ram);

        if self.metrics.update(&mut self.text, reading, self.language) {
            self.rebuild_layouts();
            self.invalidate();
        }
    }

    /// Re-measures after the window changed size; also the second half of a DPI change.
    fn resized(&mut self) {
        let mut client = RECT::default();
        // SAFETY: `hwnd` is a live window and `client` is a live out-parameter.
        let _ = unsafe { GetClientRect(self.hwnd, &mut client) };

        let dpi = self.dpi();
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_dpi(dpi);
            if let Err(error) = renderer.resize((
                u32::try_from(client.right.max(1)).unwrap_or(1),
                u32::try_from(client.bottom.max(1)).unwrap_or(1),
            )) {
                crate::log_warn!("bar", "resize: {error}");
            }
        }

        self.layouts = None;
        self.rebuild_layouts();
        self.invalidate();
    }

    /// `WM_DPICHANGED`: the bar now lives on a monitor of a different scale.
    ///
    /// The monitor's DPI is updated first, because the reservation thickness is derived from it: a
    /// 40 DIP bar on a 144 DPI screen must reserve 60 physical pixels, not the 40 it reserved before.
    ///
    /// The order of the steps is the whole point. Windows suggests a rectangle for the window, the
    /// reservation then moves it to the strip, and the renderer must be sized by what the window ends
    /// up as — because the `WM_SIZE` that the reservation's `SetWindowPos` sends is swallowed by the
    /// re-entrancy guard, so a renderer measured between the two would keep the suggested size and the
    /// clock would drift out of the middle of the strip.
    fn dpi_changed(&mut self, dpi: u32, suggested: Option<Rect>) {
        let dpi = dpi.max(96);
        let previous = std::mem::replace(&mut self.monitor.dpi, dpi);
        crate::log_info!(
            "bar",
            "{}: dpi {previous} -> {dpi}, strip {}px",
            self.monitor.device,
            self.monitor.to_pixels(self.height_dip)
        );

        // 1. Take the rectangle Windows offers, which keeps the strip the same height in DIPs.
        if let Some(rect) = suggested {
            self.host.place(self.hwnd, rect, self.topmost());
        }

        // 2. Tell the renderer the new scale; its target is created for it on the next size.
        if let Some(renderer) = self.renderer.as_mut() {
            renderer.set_dpi(dpi);
        }

        // 3. Re-assert the strip, which moves the window again — and whose `WM_SIZE` is swallowed.
        self.reassert();

        // 4. Measure the client rectangle the window actually has *now*, which is the strip.
        self.resized();
    }

    /// Puts the strip back where it belongs after the shell moved it.
    fn reassert(&mut self) {
        if !self.reserved {
            return;
        }

        let rect = self.strip();
        match appbar::reassert(
            self.host.as_ref(),
            self.hwnd,
            self.monitor.rect,
            self.edge,
            rect.height(),
            self.topmost(),
        ) {
            Ok(strip) => self.host.place(self.hwnd, strip.rect, self.topmost()),
            Err(error) => crate::log_warn!("appbar", "{}: {error}", self.monitor.device),
        }
    }

    /// Explorer restarted and dropped every registration; registering again is the only way back.
    fn reregister(&mut self) {
        crate::log_info!(
            "appbar",
            "{}: Explorer restarted, registering the bar again",
            self.monitor.device
        );

        // Drop a registration that survived the restart before asking for a new one: `ABM_NEW` is
        // refused for a window the shell already counts as a bar, and a spurious broadcast leaves the
        // registration in place.
        if self.reserved {
            appbar::release(self.host.as_ref(), self.hwnd);
        }
        self.reserved = false;
        self.reserve();
    }

    /// Steps out of a fullscreen application's way, or back in front of it.
    fn suspend(&mut self, suspended: bool) {
        if self.topmost_suspended == suspended {
            return;
        }

        self.topmost_suspended = suspended;
        let rect = self.strip();
        self.host.place(self.hwnd, rect, self.topmost());
        crate::log_info!(
            "appbar",
            "{}: a fullscreen app {}",
            self.monitor.device,
            if suspended {
                "took the screen, topmost dropped"
            } else {
                "is gone, topmost restored"
            }
        );

        if !suspended {
            self.reassert();
        }
    }

    /// Gives the strip back. Idempotent, so every destroy path may call it.
    fn detach(&mut self) {
        if !self.reserved {
            return;
        }

        appbar::release(self.host.as_ref(), self.hwnd);
        self.reserved = false;
        crate::log_info!(
            "appbar",
            "{}: released the reserved edge",
            self.monitor.device
        );
    }

    /// Asks for a repaint, which Windows may coalesce with another one.
    fn invalidate(&self) {
        // SAFETY: `hwnd` is a live window.
        unsafe {
            let _ = InvalidateRect(Some(self.hwnd), None, false);
        }
    }
}

impl Drop for BarState {
    fn drop(&mut self) {
        // The last line of defence: whatever path destroys the window, the strip goes back. Windows
        // would drop the registration with the process, but a rebuild inside a living process would
        // otherwise leave the desktop short of forty pixels for as long as the shell runs.
        self.detach();
    }
}

/// Exclusive access to one bar's state for the length of one window message.
///
/// A window procedure can be re-entered by a message the sender triggered itself, so the state has one
/// owner at a time: `enter` refuses the second borrow and the nested call falls through to
/// `DefWindowProcW`, which is exactly what a nested `WM_SIZE` during our own `SetWindowPos` wants.
#[derive(Debug)]
struct StateGuard {
    state: *mut BarState,
}

impl StateGuard {
    /// Takes the state of `hwnd`, or `None` when it is missing or already borrowed.
    ///
    /// # Safety
    ///
    /// `hwnd` must be a window this module created on this thread.
    unsafe fn enter(hwnd: HWND) -> Option<Self> {
        // SAFETY: the caller guarantees `hwnd` is one of our windows, whose user data is either null
        // or the `Box<BarState>` that `WM_NCCREATE` installed.
        let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };

        if pointer == 0 {
            return None;
        }

        let state = pointer as *mut BarState;

        // SAFETY: the pointer is that box, and it is dropped only in `WM_NCDESTROY`, which cannot run
        // while a guard is alive. `busy` is the proof that no other guard holds it.
        if unsafe { (*state).busy } {
            return None;
        }

        // SAFETY: as above; this frame now owns the state until the guard drops.
        unsafe {
            (*state).busy = true;
        }

        Some(Self { state })
    }
}

impl Deref for StateGuard {
    type Target = BarState;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe { &*self.state }
    }
}

impl DerefMut for StateGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe { &mut *self.state }
    }
}

impl Drop for StateGuard {
    fn drop(&mut self) {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe {
            (*self.state).busy = false;
        }
    }
}

/// Replaces the state pointer of `hwnd`, taking or giving ownership.
///
/// # Safety
///
/// `pointer` must be null or a `Box<BarState>` this thread allocated and gave up.
unsafe fn set_state(hwnd: HWND, pointer: isize) -> isize {
    // SAFETY: the caller guarantees the pointer's provenance.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, pointer) }
}

/// The bar window procedure.
unsafe extern "system" fn bar_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the caller is Windows, delivering one message for one of our windows.
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = lparam.0 as *const CREATESTRUCTW;
                if create.is_null() {
                    return LRESULT(0);
                }

                // `create_bar` boxed the spec and hands it over here; nothing else owns it.
                let spec = *Box::from_raw((*create).lpCreateParams.cast::<BarSpec>());
                let state = Box::new(BarState::new(hwnd, spec));
                set_state(hwnd, Box::into_raw(state) as isize);
                return LRESULT(1);
            }
            WM_NCDESTROY => {
                let pointer = set_state(hwnd, 0);
                if pointer != 0 {
                    // `Drop` releases the edge if anything still holds it, and frees the box.
                    drop(Box::from_raw(pointer as *mut BarState));
                }
                return LRESULT(0);
            }
            _ => {}
        }

        let Some(mut state) = StateGuard::enter(hwnd) else {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        };

        match message {
            WM_PAINT => {
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut paint);
                state.paint();
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                state.resized();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xFFFF) as u32;
                let suggested = if lparam.0 == 0 {
                    None
                } else {
                    // SAFETY: `WM_DPICHANGED` carries a pointer to the rectangle Windows suggests for
                    // the window at the new scale.
                    Some(Rect::from(*(lparam.0 as *const RECT)))
                };

                state.dpi_changed(dpi, suggested);
                LRESULT(0)
            }
            WM_NCHITTEST => LRESULT(isize::try_from(HTCLIENT).unwrap_or(1)),
            WM_TIMER if wparam.0 == TIMER_CLOCK => {
                state.tick();
                // Re-armed against the wall clock rather than one second from now, so the tick cannot
                // drift away from the second the clock actually changes at.
                SetTimer(Some(hwnd), TIMER_CLOCK, millis_to_next_second(), None);
                LRESULT(0)
            }
            WM_METRICS => {
                state.apply_metrics(wparam.0 as u32, lparam.0 as u32);
                LRESULT(0)
            }
            WM_DESTROY => {
                // `WM_DESTROY` precedes `WM_NCDESTROY` and runs while the handle is still valid, which
                // is the right moment to hand the strip back.
                state.detach();
                LRESULT(0)
            }
            WM_CLOSE => {
                // A bar cannot be focused, so this only arrives from the shell integration, and it
                // means "go away": every bar closes together.
                request_quit();
                LRESULT(0)
            }
            WM_DISPLAYCHANGE => {
                request_rebuild_debounced();
                LRESULT(0)
            }
            _ if message == taskbar_created() => {
                state.reregister();
                LRESULT(0)
            }
            _ if message == callback_message() => {
                // The notification is recorded, not acted on: acting here would borrow the state while
                // `ABM_SETPOS` — the call that produced this message — is still using it.
                match wparam.0 as u32 {
                    appbar::ABN_FULLSCREENAPP => {
                        let suspended = lparam.0 != 0;
                        set_pending(|pending| pending.suspend = Some(suspended));
                    }
                    appbar::ABN_POSCHANGED if !appbar::is_reasserting() => {
                        set_pending(|pending| pending.reassert = true);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

/// Milliseconds until the next whole second, clamped so a tick just after the boundary does not spin.
pub fn millis_to_next_second() -> u32 {
    // SAFETY: `GetLocalTime` fills in the structure it returns and takes no pointer from the caller.
    let now = unsafe { GetLocalTime() };
    let remaining = 1_000u32.saturating_sub(u32::from(now.wMilliseconds));

    if remaining >= MIN_CLOCK_INTERVAL_MS {
        remaining
    } else {
        1_000
    }
}

/// The controller window procedure: no drawing, only routing.
unsafe extern "system" fn control_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the caller is Windows, delivering one message for one of our windows.
    unsafe {
        match message {
            WM_DISPLAYCHANGE | WM_REBUILD => {
                request_rebuild_debounced();
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_REBUILD => {
                let _ = KillTimer(Some(hwnd), TIMER_REBUILD);
                set_pending(|pending| pending.rebuild = true);
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_REPORT => {
                let _ = KillTimer(Some(hwnd), TIMER_REPORT);
                request_quit();
                LRESULT(0)
            }
            WM_RELOAD => {
                set_pending(|pending| pending.reload = true);
                LRESULT(0)
            }
            WM_VISIBILITY => {
                let visible = wparam.0 != 0;
                set_pending(|pending| pending.visible = Some(visible));
                LRESULT(0)
            }
            WM_HOTKEY if wparam.0 as i32 == crate::launcher::window::HOTKEY_ID => {
                set_pending(|pending| pending.set_launcher_toggle());
                LRESULT(0)
            }
            _ if message == crate::launcher::WM_LAUNCHER_OPEN => {
                set_pending(|pending| pending.set_launcher_open());
                LRESULT(0)
            }
            _ if message == crate::launcher::WM_LAUNCHER_INDEX => {
                set_pending(|pending| pending.set_launcher_index());
                LRESULT(0)
            }
            WM_CLOSE | WM_DESTROY => {
                request_quit();
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

/// Asks for a rebuild once the display has stopped changing for [`REBUILD_DEBOUNCE_MS`].
fn request_rebuild_debounced() {
    match control() {
        Some(control) => {
            // SAFETY: `control` is this process's controller window; an armed timer is replaced
            // rather than added to, which is what makes this a debounce.
            unsafe {
                SetTimer(Some(control), TIMER_REBUILD, REBUILD_DEBOUNCE_MS, None);
            }
        }
        None => set_pending(|pending| pending.rebuild = true),
    }
}

/// The controller window handle, if it has been created.
pub fn control() -> Option<HWND> {
    CONTROL
        .get()
        .map(|handle| HWND(*handle as *mut std::ffi::c_void))
}

/// This process's `HINSTANCE`.
fn instance() -> windows::Win32::Foundation::HINSTANCE {
    // SAFETY: a null module name asks for this process's own module.
    match unsafe { GetModuleHandleW(None) } {
        Ok(module) => windows::Win32::Foundation::HINSTANCE(module.0),
        Err(_) => windows::Win32::Foundation::HINSTANCE::default(),
    }
}

/// Registers one window class, once per process.
fn register_class(
    class: &'static str,
    proc: WndProc,
    atom: &'static OnceLock<bool>,
) -> Result<(), WindowError> {
    let registered = *atom.get_or_init(|| {
        let name = wide(class);
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(proc),
            hInstance: instance(),
            lpszClassName: PCWSTR(name.as_ptr()),
            ..WNDCLASSW::default()
        };

        // SAFETY: `window_class` is fully initialised and the class name it points at outlives the
        // call; a null background brush and no cursor keep the bar from flashing or grabbing one.
        unsafe { RegisterClassW(&window_class) != 0 }
    });

    if registered {
        Ok(())
    } else {
        Err(WindowError::Class { class })
    }
}

/// Creates the controller window, which lives as long as the process.
///
/// # Errors
///
/// [`WindowError::Class`] or [`WindowError::Create`] when Windows refuses the window.
pub fn create_controller() -> Result<HWND, WindowError> {
    register_class(CONTROL_CLASS, control_proc, &CONTROL_CLASS_REGISTERED)?;

    let class = wide(CONTROL_CLASS);
    let title = wide("ProjectNeb");

    // SAFETY: both strings are NUL-terminated and outlive the call; the window is a one-pixel popup
    // that is never shown, so it costs nothing beyond a message-queue target.
    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(instance()),
            None,
        )
    }
    .map_err(|source| WindowError::Create {
        class: CONTROL_CLASS,
        source,
    })?;

    let _ = CONTROL.set(hwnd.0 as isize);

    Ok(hwnd)
}

/// Creates one bar and attaches it to its monitor.
///
/// # Errors
///
/// [`WindowError`] when Windows or Direct2D refuses.
pub fn create_bar(spec: BarSpec) -> Result<HWND, WindowError> {
    register_class(BAR_CLASS, bar_proc, &BAR_CLASS_REGISTERED)?;

    let strip = strip_of(&spec.monitor, spec.edge, spec.height_dip);
    let class = wide(BAR_CLASS);
    let title = wide("ProjectNeb bar");
    let parameter = Box::into_raw(Box::new(spec));

    // SAFETY: both strings are NUL-terminated and outlive the call; `parameter` is a heap `BarSpec`
    // that `WM_NCCREATE` takes ownership of, or that is reclaimed below when the call fails.
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TOPMOST,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            strip.left,
            strip.top,
            strip.width(),
            strip.height(),
            None,
            None,
            Some(instance()),
            Some(parameter.cast()),
        )
    };

    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(source) => {
            // `WM_NCCREATE` never ran, so the spec is still ours to free.
            drop(unsafe { Box::from_raw(parameter) });
            return Err(WindowError::Create {
                class: BAR_CLASS,
                source,
            });
        }
    };

    // SAFETY: `hwnd` was created a line above, on this thread.
    let mut state = unsafe { StateGuard::enter(hwnd) }.ok_or(WindowError::NoState(BAR_CLASS))?;
    state.attach()?;
    state.invalidate();

    // SAFETY: `hwnd` is a live window of this process.
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        SetTimer(Some(hwnd), TIMER_CLOCK, millis_to_next_second(), None);
    }

    Ok(hwnd)
}

/// Destroys one bar, which releases its reserved edge on the way out.
pub fn destroy_bar(hwnd: HWND) {
    // SAFETY: `hwnd` is a live bar window of this process.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// Paints one bar immediately, synchronously, without waiting for the queue.
///
/// `WM_PAINT` is the only thing that draws, so this is the whole paint path; the allocation harness
/// in `tests/paint_no_alloc.rs` is what needs it. The message is delivered directly because
/// `UpdateWindow` is not part of the Win32 surface this crate links against.
pub fn repaint_now(hwnd: HWND) {
    // SAFETY: `hwnd` is a live bar window of this process, created on this thread, and an empty
    // `WM_PAINT` carries no data the procedure could misread.
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
        SendMessageW(hwnd, WM_PAINT, None, None);
    }
}

/// How many frames a bar has actually drawn; zero means the paint path never ran.
pub fn painted_frames(hwnd: HWND) -> u64 {
    // SAFETY: `hwnd` is a live bar window of this process, created on this thread.
    unsafe { StateGuard::enter(hwnd) }.map_or(0, |state| state.frames)
}

/// The pixel size of the render target behind `hwnd`, or `None` when it has no renderer yet.
///
/// This is what a DPI change has to get right: the target must follow the strip the reservation
/// produced, not the rectangle Windows suggested for the window.
pub fn renderer_size(hwnd: HWND) -> Option<(u32, u32)> {
    // SAFETY: `hwnd` is a live bar window of this process, created on this thread.
    unsafe { StateGuard::enter(hwnd) }?
        .renderer
        .as_ref()
        .map(Renderer::pixel_size)
}

/// Shows or hides one bar.
///
/// Hiding keeps the reservation: releasing it would make every maximised window jump whenever the
/// user toggles the bar.
pub fn set_bar_visible(hwnd: HWND, visible: bool) {
    // SAFETY: `hwnd` is a live bar window of this process; `SW_SHOWNOACTIVATE` is what keeps the bar
    // from taking focus away from whatever the user is typing in.
    unsafe {
        let _ = ShowWindow(hwnd, if visible { SW_SHOWNOACTIVATE } else { SW_HIDE });
    }
}

/// Whether a bar is currently on screen.
pub fn is_visible(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` is a live bar window of this process.
    unsafe { IsWindowVisible(hwnd).as_bool() }
}

/// Reserves or releases one bar's strip.
///
/// Hiding a bar releases its edge and showing it takes the edge back, which is how the desktop gets
/// its pixels returned instead of leaving a dead strip nobody can use.
pub fn set_edge(hwnd: HWND, reserved: bool) {
    // SAFETY: `hwnd` is a live bar window of this process, created on this thread.
    let Some(mut state) = (unsafe { StateGuard::enter(hwnd) }) else {
        return;
    };

    if reserved {
        if !state.reserved {
            state.reregister();
        }
    } else {
        state.detach();
    }
}

/// Re-asserts one bar's reservation, for a notification that arrived between dispatches.
pub fn reassert(hwnd: HWND) {
    // SAFETY: `hwnd` is a live bar window of this process, created on this thread.
    if let Some(mut state) = unsafe { StateGuard::enter(hwnd) } {
        state.reassert();
    }
}

/// Drops or restores one bar's topmost flag while a fullscreen app owns the screen.
pub fn suspend(hwnd: HWND, suspended: bool) {
    // SAFETY: `hwnd` is a live bar window of this process, created on this thread.
    if let Some(mut state) = unsafe { StateGuard::enter(hwnd) } {
        state.suspend(suspended);
    }
}

/// Posts a metrics sample to one bar.
pub fn post_metrics(hwnd: isize, cpu: u32, ram: u32) {
    let hwnd = HWND(hwnd as *mut std::ffi::c_void);

    // SAFETY: the handle came from this process's own list of bars; a vanished window makes the post
    // fail, which is the expected race during a rebuild and needs no handling.
    unsafe {
        let _ = PostMessageW(
            Some(hwnd),
            WM_METRICS,
            WPARAM(cpu as usize),
            LPARAM(ram as isize),
        );
    }
}

/// Posts a request to the controller; a no-op when there is none yet.
pub fn post_to_control(message: u32, wparam: usize) {
    if let Some(control) = control() {
        // SAFETY: `control` is this process's controller window.
        unsafe {
            let _ = PostMessageW(Some(control), message, WPARAM(wparam), LPARAM(0));
        }
    }
}

/// Asks the controller for a rebuild at once, without the debounce.
pub fn request_rebuild() {
    post_to_control(WM_REBUILD, 0);
}

/// Arms the memory report timer on the controller.
pub fn arm_report_timer(milliseconds: u32) {
    if let Some(control) = control() {
        // SAFETY: `control` is this process's controller window.
        unsafe {
            SetTimer(Some(control), TIMER_REPORT, milliseconds, None);
        }
    }
}

/// Runs the message loop until `WM_QUIT`, applying pending work between dispatches.
///
/// `apply` runs with nothing else borrowed, which is where the application may create and destroy
/// windows.
pub fn run_loop(mut apply: impl FnMut(Pending)) {
    loop {
        let mut message = MSG::default();
        // SAFETY: `message` is a live out-parameter; a null window and zero filters mean "every
        // message for this thread".
        let result = unsafe { GetMessageW(&mut message, None, 0, 0) };

        // A negative result is an error and is treated like `WM_QUIT`.
        if result.0 <= 0 {
            break;
        }

        // SAFETY: `message` was filled in by `GetMessageW` above.
        unsafe {
            let _ = TranslateMessage(&message);
            DispatchMessageW(&message);
        }

        let mut leaving = false;
        loop {
            let pending = take_pending();
            if pending.is_empty() {
                break;
            }

            if pending.quit() {
                leaving = true;
                break;
            }

            apply(pending);
        }

        if leaving {
            break;
        }
    }
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use neb_core::strings::Language;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A host that records what a bar asked the shell to do, so a handler can be driven without one.
    #[derive(Debug, Default)]
    struct RecordingHost {
        new: AtomicUsize,
        set_position: AtomicUsize,
        thickness: AtomicUsize,
        places: Mutex<Vec<Rect>>,
    }

    impl crate::appbar::EdgeHost for RecordingHost {
        fn appbar(&self, call: crate::appbar::Call, data: &mut crate::appbar::APPBARDATA) -> usize {
            match call {
                crate::appbar::Call::New => {
                    self.new.fetch_add(1, Ordering::SeqCst);
                }
                crate::appbar::Call::SetPosition => {
                    self.set_position.fetch_add(1, Ordering::SeqCst);
                    self.thickness.store(
                        usize::try_from(data.rc.bottom - data.rc.top).unwrap_or(0),
                        Ordering::SeqCst,
                    );
                }
                _ => {}
            }

            1
        }

        fn place(&self, _hwnd: HWND, rect: Rect, _topmost: bool) {
            self.places.lock().expect("lock").push(rect);
        }
    }

    fn monitor(dpi: u32) -> Monitor {
        Monitor {
            handle: 1,
            device: r"\\.\DISPLAY1".to_owned(),
            rect: Rect::new(0, 0, 1920, 1080),
            work: Rect::new(0, 0, 1920, 1032),
            dpi,
        }
    }

    #[test]
    fn a_dpi_change_re_reserves_the_strip_at_the_new_scale() {
        let host = Arc::new(RecordingHost::default());
        let mut state = BarState::new(
            HWND::default(),
            BarSpec {
                monitor: monitor(96),
                edge: Edge::Top,
                height_dip: 40,
                theme: Theme::baked_in(),
                language: Language::En,
                profile: None,
                host: Arc::clone(&host) as Arc<dyn EdgeHost>,
            },
        );
        // Pretend the bar already holds its edge, which is the case a DPI change happens in.
        state.reserved = true;

        state.dpi_changed(144, Some(Rect::new(0, 0, 1920, 60)));

        assert_eq!(
            state.monitor.dpi, 144,
            "the monitor's scale must be updated"
        );
        assert_eq!(
            state.strip().height(),
            60,
            "40 DIP at 144 DPI is 60 physical pixels"
        );
        assert_eq!(
            host.thickness.load(Ordering::SeqCst),
            60,
            "the reservation must be re-made with the new thickness"
        );
        let places = host.places.lock().expect("lock").clone();
        assert_eq!(
            places.first(),
            Some(&Rect::new(0, 0, 1920, 60)),
            "the suggested rectangle must be taken"
        );
    }

    #[test]
    fn a_dpi_change_without_a_suggestion_still_re_reserves() {
        let host = Arc::new(RecordingHost::default());
        let mut state = BarState::new(
            HWND::default(),
            BarSpec {
                monitor: monitor(96),
                edge: Edge::Top,
                height_dip: 40,
                theme: Theme::baked_in(),
                language: Language::En,
                profile: None,
                host: Arc::clone(&host) as Arc<dyn EdgeHost>,
            },
        );
        state.reserved = true;

        state.dpi_changed(120, None);

        assert_eq!(state.monitor.dpi, 120);
        assert_eq!(state.strip().height(), 50, "40 DIP at 120 DPI is 50 pixels");
        assert_eq!(host.thickness.load(Ordering::SeqCst), 50);
    }

    #[test]
    fn a_suspended_bar_is_re_placed_without_the_topmost_flag() {
        let host = Arc::new(RecordingHost::default());
        let mut state = BarState::new(
            HWND::default(),
            BarSpec {
                monitor: monitor(96),
                edge: Edge::Top,
                height_dip: 40,
                theme: Theme::baked_in(),
                language: Language::En,
                profile: None,
                host: Arc::clone(&host) as Arc<dyn EdgeHost>,
            },
        );
        state.reserved = true;
        state.suspend(true);
        assert!(state.topmost_suspended);
        assert!(
            !state.topmost(),
            "a fullscreen app must keep the strip behind it"
        );
    }

    #[test]
    fn the_pending_store_starts_empty_and_takes_what_it_is_given() {
        let _ = take_pending();

        set_pending(|pending| pending.reload = true);
        let pending = take_pending();
        assert!(pending.reload());
        assert!(!pending.rebuild());
        assert_eq!(pending.visible(), None);
        assert!(take_pending().is_empty());
    }

    #[test]
    fn visibility_and_quit_travel_through_the_store() {
        set_pending(|pending| pending.visible = Some(false));
        assert_eq!(take_pending().visible(), Some(false));

        request_quit();
        assert!(take_pending().quit());
        assert!(!take_pending().quit());
    }

    #[test]
    fn the_appbar_notifications_travel_through_the_store() {
        set_pending(|pending| pending.reassert = true);
        assert!(take_pending().reassert());

        set_pending(|pending| pending.suspend = Some(true));
        assert_eq!(take_pending().suspend(), Some(true));

        set_pending(|pending| pending.suspend = Some(false));
        assert_eq!(take_pending().suspend(), Some(false));
    }

    #[test]
    fn a_dock_click_and_a_refresh_travel_through_the_store() {
        assert_eq!(take_pending().dock_click(), None);

        set_pending(|pending| pending.set_dock_click(crate::dock::Click::Activate(0x1234)));
        assert_eq!(
            take_pending().dock_click(),
            Some(crate::dock::Click::Activate(0x1234))
        );

        set_pending(|pending| pending.set_dock_refresh(true));
        assert!(take_pending().dock_refresh());
    }

    #[test]
    fn launcher_requests_travel_through_the_store() {
        set_pending(|pending| pending.set_launcher_toggle());
        assert!(take_pending().launcher_toggle());
        assert!(!take_pending().launcher_toggle());

        set_pending(|pending| pending.set_launcher_open());
        assert!(take_pending().launcher_open());

        set_pending(|pending| pending.set_launcher_close());
        assert!(take_pending().launcher_close());

        set_pending(|pending| pending.set_launcher_index());
        assert!(take_pending().launcher_index());

        set_pending(|pending| pending.set_launcher_launch(3));
        assert_eq!(take_pending().launcher_launch(), Some(3));
        assert_eq!(take_pending().launcher_launch(), None);
    }

    #[test]
    fn the_custom_messages_sit_above_wm_app() {
        assert_eq!(WM_METRICS, 0x8000 + 1);
        assert_eq!(WM_REBUILD, 0x8000 + 2);
        assert_eq!(WM_RELOAD, 0x8000 + 3);
        assert_eq!(WM_VISIBILITY, 0x8000 + 4);
    }

    #[test]
    fn the_registered_messages_are_stable_across_calls() {
        let callback = callback_message();
        let taskbar = taskbar_created();

        assert_ne!(
            callback, 0,
            "the shell must have registered ProjectNeb_AppBar"
        );
        assert_ne!(taskbar, 0, "the shell must have registered TaskbarCreated");
        assert_eq!(callback, callback_message());
        assert_eq!(taskbar, taskbar_created());
    }

    #[test]
    fn the_clock_timer_lands_inside_the_next_second() {
        let first = millis_to_next_second();

        assert!(
            (MIN_CLOCK_INTERVAL_MS..=1_000).contains(&first),
            "the interval must be a real wait, got {first}"
        );
    }

    #[test]
    fn a_strip_is_the_configured_height_and_the_monitor_width() {
        let monitor = Monitor {
            handle: 1,
            device: r"\\.\DISPLAY1".to_owned(),
            rect: Rect::new(0, 0, 1920, 1080),
            work: Rect::new(0, 0, 1920, 1032),
            dpi: 96,
        };

        assert_eq!(strip_of(&monitor, Edge::Top, 40), Rect::new(0, 0, 1920, 40));
        assert_eq!(
            strip_of(&monitor, Edge::Bottom, 40),
            Rect::new(0, 1040, 1920, 1080)
        );
    }

    #[test]
    fn a_hundred_and_fifty_percent_monitor_gets_a_half_again_as_many_pixels() {
        let monitor = Monitor {
            handle: 1,
            device: r"\\.\DISPLAY1".to_owned(),
            rect: Rect::new(0, 0, 2560, 1440),
            work: Rect::new(0, 0, 2560, 1392),
            dpi: 144,
        };

        assert_eq!(strip_of(&monitor, Edge::Top, 40), Rect::new(0, 0, 2560, 60));
    }
}
