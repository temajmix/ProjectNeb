//! The dock window, its procedure, and the messages between it and the application.
//!
//! The window is a layered popup on the bottom edge: `WS_EX_TOOLWINDOW` keeps it out of Alt+Tab,
//! `WS_EX_LAYERED` is what makes the glass and the rounded corners possible, and it stays
//! *activatable* on purpose вЂ” a dock tile has to be able to hand the foreground to the app behind it.
//!
//! Like the bar, the procedure reaches into nothing but its own window's state, guarded against
//! re-entry by [`DockGuard`]: a click is recorded in [`Pending`] for the message loop to apply, and a
//! refresh from the running-app worker is recorded the same way. The only work the procedure does
//! itself is local: hover, the auto-hide timer, and presenting a frame.

use std::ops::{Deref, DerefMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread::JoinHandle;
use std::time::Duration;

use neb_core::config::AnimationSettings;
use neb_core::strings::{Language, Text, get};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, EndPaint, HDC, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::Win32::UI::Controls::{
    TOOLTIPS_CLASSW, TTF_SUBCLASS, TTM_ADDTOOLW, TTM_NEWTOOLRECTW, TTM_SETMAXTIPWIDTH,
    TTM_UPDATETIPTEXTW, TTS_ALWAYSTIP, TTS_NOPREFIX, TTTOOLINFOW, WM_MOUSELEAVE,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, GWLP_USERDATA, GetClientRect,
    GetCursorPos, GetForegroundWindow, GetWindowLongPtrW, GetWindowRect, HTCLIENT, HWND_NOTOPMOST,
    HWND_TOPMOST, IDC_ARROW, KillTimer, LoadCursorW, PostMessageW, RegisterClassW, SW_HIDE,
    SW_SHOWNOACTIVATE, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SendMessageW,
    SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow, WINDOW_STYLE, WM_APP, WM_CLOSE,
    WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_LBUTTONUP, WM_MOUSEMOVE, WM_NCCREATE,
    WM_NCDESTROY, WM_NCHITTEST, WM_PAINT, WM_PRINTCLIENT, WM_TIMER, WNDCLASSW, WS_EX_LAYERED,
    WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_POPUPWINDOW,
};
use windows::core::{PCWSTR, PWSTR};

use crate::anim::{DOCK_SLIDE_MS, FRAME_MS, HOVER_MS, Transition, now_ms};
use crate::geometry::{Monitor, Rect};
use crate::render::PaintOutcome;
use crate::theme::Theme;
use crate::widgets::WideBuf;
use crate::window::{self as bar_window, WindowError};

use super::autohide::{self, Autohide};
use super::fullscreen;
use super::layout::{self, DockPlan, Metrics};
use super::render::{DockRenderer, FrameFx};
use super::running::{self, RunningApp};
use super::{Click, DockItem};

/// Class name of a dock window.
pub const DOCK_CLASS: &str = "ProjectNeb.NativeDock";

/// A running-application snapshot is ready: the application should adopt it.
pub const WM_DOCK_RUNNING: u32 = WM_APP + 5;

/// The auto-hide and fullscreen poll timer.
const TIMER_POLL: usize = 21;

/// The 16 ms timer that runs only while the dock is sliding or a hover is fading.
const TIMER_ANIM: usize = 22;

/// How often the foreground is asked whether a fullscreen application owns the screen.
///
/// The auto-hide tick runs five times faster than this; a game going fullscreen or leaving it is not a
/// thing that needs 100 ms of precision, and the probe is seven Win32 calls.
const FULLSCREEN_CHECK_MS: u64 = 500;

/// Id of the single tooltip tool, which is re-targeted at the tile under the pointer.
const TOOL_ID: usize = 1;

/// Where a dock's placement calls go: the real desktop, or a counter in a test.
///
/// The dock always wants the topmost band вЂ” it floats over maximised windows вЂ” so the flag is part of
/// the contract rather than something the caller could forget.
pub trait DockPlacement: std::fmt::Debug {
    /// Moves `hwnd` onto `rect` in the topmost band, without activating it.
    fn place(&self, hwnd: HWND, rect: Rect, topmost: bool);

    /// Puts `hwnd` back at the front of the topmost band without moving or resizing it.
    fn raise(&self, hwnd: HWND);
}

/// The real desktop.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsPlacement;

impl DockPlacement for WindowsPlacement {
    fn place(&self, hwnd: HWND, rect: Rect, topmost: bool) {
        // SAFETY: `hwnd` is a window this process created; the rectangle is a plain value.
        // `SWP_NOACTIVATE` is what keeps the strip from taking the focus when it appears or moves.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                Some(if topmost {
                    HWND_TOPMOST
                } else {
                    HWND_NOTOPMOST
                }),
                rect.left,
                rect.top,
                rect.width(),
                rect.height(),
                SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        };
    }

    fn raise(&self, hwnd: HWND) {
        // SAFETY: as above; the size and position are left alone, only the band is re-asserted.
        let _ = unsafe {
            SetWindowPos(
                hwnd,
                Some(HWND_TOPMOST),
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
            )
        };
    }
}

/// The placement host a running shell uses.
pub fn windows() -> Arc<dyn DockPlacement> {
    Arc::new(WindowsPlacement)
}

/// What a dock is created with; it travels through `CreateWindowExW` as the create parameter.
#[derive(Debug, Clone)]
pub struct DockSpec {
    /// The monitor the strip floats over.
    pub monitor: Monitor,
    /// The palette it paints with.
    pub theme: Theme,
    /// The language its strings are in.
    pub language: Language,
    /// The tiles to draw, in order.
    pub items: Vec<DockItem>,
    /// Whether the strip stays hidden until the pointer reaches the bottom edge.
    pub auto_hide: bool,
    /// The tile side in device-independent pixels: the theme's `dockTile`, or the shipped default.
    pub tile_dip: i32,
    /// The user's motion preferences.
    pub animations: AnimationSettings,
    /// How often the pointer and the fullscreen input are sampled, in milliseconds.
    pub poll_ms: u32,
    /// Where the placement calls go; the shell itself, or a recorder in a test.
    pub host: Arc<dyn DockPlacement>,
}

/// Everything one dock window owns.
#[derive(Debug)]
struct DockState {
    hwnd: HWND,
    monitor: Monitor,
    theme: Theme,
    language: Language,
    items: Vec<DockItem>,
    plan: DockPlan,
    rect: Rect,
    renderer: Option<DockRenderer>,
    auto_hide: bool,
    autohide: Autohide,
    /// A fullscreen application owns the foreground, so the strip is out of the way.
    fullscreen: bool,
    /// When the foreground was last asked about fullscreen, so the probe runs at its own interval.
    last_check: u64,
    /// Where the placement calls go.
    host: Arc<dyn DockPlacement>,
    hover: Option<usize>,
    /// How far the hover highlight under the pointer has faded in.
    hover_fade: Transition,
    /// The tile the pointer just left, while its highlight fades out.
    previous_hover: Option<usize>,
    /// How far the previous tile's highlight still is.
    previous_fade: Transition,
    /// Whether the window is currently shown, including while it is sliding away.
    shown: bool,
    /// The slide: `0.0` below the strip, `1.0` in place.
    slide: Transition,
    /// Whether the 16 ms animation timer is armed.
    animating: bool,
    /// The tile side the plan was built with, in DIPs.
    tile_dip: i32,
    /// The user's motion preferences.
    animations: AnimationSettings,
    /// The pointer/fullscreen poll period this window was created with.
    poll_ms: u32,
    tooltip: Option<Tooltip>,
    /// A pointer sample waiting for the next tick; [`probe_pointer`] writes this.
    pointer: Option<(i32, i32)>,
    /// Set while a window procedure holds this state, so a re-entrant message cannot borrow it again.
    busy: bool,
    /// How many frames this dock has drawn, which is the only proof a paint test can offer.
    frames: u64,
}

impl DockState {
    fn new(hwnd: HWND, spec: DockSpec) -> Self {
        let plan = plan_for(&spec.items, &spec.monitor, spec.tile_dip);
        let rect = layout::place(&spec.monitor, &plan);
        let renderer = make_renderer(
            u32::try_from(plan.width).unwrap_or(1),
            u32::try_from(plan.height).unwrap_or(1),
            &spec.monitor,
            &spec.theme,
            spec.language,
        );

        Self {
            hwnd,
            monitor: spec.monitor,
            theme: spec.theme,
            language: spec.language,
            items: spec.items,
            plan,
            rect,
            renderer,
            auto_hide: spec.auto_hide,
            autohide: Autohide::new(spec.auto_hide),
            fullscreen: false,
            last_check: 0,
            host: spec.host,
            hover: None,
            hover_fade: Transition::settled(0.0),
            previous_hover: None,
            previous_fade: Transition::settled(0.0),
            shown: false,
            slide: Transition::settled(0.0),
            animating: false,
            tile_dip: spec.tile_dip,
            animations: spec.animations,
            poll_ms: spec.poll_ms.max(1),
            tooltip: None,
            pointer: None,
            busy: false,
            frames: 0,
        }
    }

    /// Places the window, draws the first frame and takes over the bottom strip.
    fn attach(&mut self) {
        self.place_window();

        // The timer drives both the pointer sample and the fullscreen probe, so it runs whether or
        // not the strip hides itself: a game must be able to push the dock away either way.
        // SAFETY: `hwnd` is this window and the timer id is ours.
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_POLL, self.poll_ms, None);
        }

        self.check_fullscreen();
        self.paint();
        self.show_or_hide();

        crate::log_info!(
            "dock",
            "{}: {} item(s), {} drawn + {} over the edge, {}x{} px at {},{} (language '{}')",
            self.monitor.device,
            self.items.len(),
            self.plan.visible,
            self.plan.overflow,
            self.plan.width,
            self.plan.height,
            self.rect.left,
            self.rect.top,
            self.language.tag()
        );
    }

    /// Whether the strip should be on screen right now.
    fn visible(&self) -> bool {
        !self.items.is_empty() && !self.fullscreen && !self.autohide.is_hidden()
    }

    /// Shows or hides the window to match [`visible`](Self::visible), sliding it into or out of place.
    fn show_or_hide(&mut self) {
        if self.visible() {
            if !self.shown {
                self.shown = true;
                // SAFETY: `hwnd` is a live window of this process. `SW_SHOWNOACTIVATE` is what keeps
                // the dock from taking focus away from whatever the user is typing in.
                unsafe {
                    let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                }
                self.begin_slide(true);
            }

            // `ShowWindow` can put the window back at the top of the *non-topmost* band on some
            // transitions, and a dock under a maximised window is the defect this exists to prevent.
            self.host.raise(self.hwnd);
        } else if self.shown {
            // The window stays on screen until the slide has taken it out of view; the animation
            // tick hides it then.
            self.begin_slide(false);
        }
    }

    /// Starts the dock's slide towards `shown` (`0.0` down, `1.0` up).
    fn begin_slide(&mut self, shown: bool) {
        let now = now_ms();
        let target = if shown { 1.0 } else { 0.0 };

        self.slide = Transition::scaled(
            now,
            self.slide.value(now),
            target,
            DOCK_SLIDE_MS,
            &self.animations,
        );

        // With animations off the slide has no duration and no timer will ever fire, so the hide has
        // to happen here rather than in `animation_step`.
        if self.slide.is_done(now) && target == 0.0 && self.shown {
            self.shown = false;
            // SAFETY: `hwnd` is a live window of this process and the slide is already over.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }

        self.arm_animation();
        self.paint();
    }

    /// Moves the window to its strip, in the topmost band.
    fn place_window(&self) {
        self.host.place(self.hwnd, self.rect, true);
    }

    /// Re-plans for the current items and monitor, then repaints.
    fn relayout(&mut self) {
        self.plan = plan_for(&self.items, &self.monitor, self.tile_dip);
        self.rect = layout::place(&self.monitor, &self.plan);
        self.place_window();

        let size = (
            u32::try_from(self.plan.width).unwrap_or(1),
            u32::try_from(self.plan.height).unwrap_or(1),
        );

        let stale = self
            .renderer
            .as_ref()
            .is_none_or(|renderer| renderer.pixel_size() != size);

        if stale {
            self.renderer =
                make_renderer(size.0, size.1, &self.monitor, &self.theme, self.language);
        }

        self.paint();
        self.show_or_hide();
    }

    /// Draws one frame through the renderer, and counts it.
    fn paint(&mut self) {
        match self.paint_once() {
            PaintOutcome::Painted => self.frames += 1,
            PaintOutcome::Recreate => {
                // The device went away with every bitmap built on it, so both are rebuilt here,
                // outside the frame, and the frame is drawn once more.
                crate::log_info!("dock", "the render target was lost, rebuilding");
                self.renderer = make_renderer(
                    u32::try_from(self.plan.width).unwrap_or(1),
                    u32::try_from(self.plan.height).unwrap_or(1),
                    &self.monitor,
                    &self.theme,
                    self.language,
                );
                if let PaintOutcome::Painted = self.paint_once() {
                    self.frames += 1;
                }
            }
            PaintOutcome::Failed(error) => crate::log_warn!("dock", "paint: {error}"),
        }
    }

    /// One attempt at a frame.
    fn paint_once(&mut self) -> PaintOutcome {
        let frame = FrameFx {
            slide: self.slide.value(now_ms()),
            hover: self.hover,
            hover_fade: self.hover_fade.value(now_ms()),
            previous: self.previous_hover,
            previous_fade: self.previous_fade.value(now_ms()),
        };

        match self.renderer.as_mut() {
            Some(renderer) => renderer.paint(self.hwnd, &self.plan, &self.items, frame),
            None => PaintOutcome::Failed(crate::render::RenderError::Unavailable(
                "the dock has no renderer".to_owned(),
            )),
        }
    }

    /// Arms the 16 ms animation timer unless everything is already at rest.
    fn arm_animation(&mut self) {
        if self.animating || !self.is_animating(now_ms()) {
            return;
        }

        self.animating = true;
        // SAFETY: `hwnd` is this window and the timer id is ours.
        unsafe {
            SetTimer(Some(self.hwnd), TIMER_ANIM, FRAME_MS, None);
        }
    }

    /// Kills the animation timer; a later transition arms it again.
    fn stop_animation(&mut self) {
        if !self.animating {
            return;
        }

        self.animating = false;
        // SAFETY: `hwnd` is this window and the id belongs to the animation timer.
        unsafe {
            let _ = KillTimer(Some(self.hwnd), TIMER_ANIM);
        }
    }

    /// Whether the dock is sliding or a hover highlight is still moving.
    fn is_animating(&self, now: u64) -> bool {
        !self.slide.is_done(now)
            || !self.hover_fade.is_done(now)
            || !self.previous_fade.is_done(now)
    }

    /// One 16 ms animation frame.
    fn animation_step(&mut self) {
        let now = now_ms();

        if self.shown {
            self.paint();
        }

        if self.slide.is_done(now) && self.slide.target() == 0.0 && self.shown {
            self.shown = false;
            // SAFETY: `hwnd` is a live window of this process and the slide has taken it out of view.
            unsafe {
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            }
        }

        if !self.is_animating(now) {
            self.previous_hover = None;
            self.stop_animation();

            if self.shown {
                self.paint();
            }
        }
    }

    /// Adopts a new item list, rebuilding only if it differs.
    fn set_items(&mut self, items: Vec<DockItem>) {
        if items == self.items {
            return;
        }

        let running = items.iter().filter(|item| item.running).count();
        self.items = items;
        self.hover = None;
        self.relayout();

        crate::log_info!(
            "dock",
            "{} item(s), {running} running, {} drawn + {} over the edge, {}x{} px",
            self.items.len(),
            self.plan.visible,
            self.plan.overflow,
            self.plan.width,
            self.plan.height
        );
    }

    /// The poll tick: a fullscreen probe at its own interval, then the pointer sample.
    fn tick(&mut self) {
        // SAFETY: the call takes no arguments and returns a monotonically increasing millisecond count.
        let now = unsafe { GetTickCount64() };

        if now >= self.last_check.saturating_add(FULLSCREEN_CHECK_MS) {
            self.check_fullscreen_at(now);
        }

        if !self.auto_hide {
            return;
        }

        let pointer = self.pointer.take().unwrap_or_else(cursor_position);

        match self
            .autohide
            .settle(now, pointer, self.monitor.rect, self.rect)
        {
            autohide::Action::None => {}
            autohide::Action::Reveal => {
                self.hover = None;
                self.show_or_hide();

                if self.visible() {
                    self.paint();
                }
                crate::log_info!("dock", "revealed at the bottom edge");
            }
            autohide::Action::Hide => {
                self.hover = None;
                self.show_or_hide();
                crate::log_info!("dock", "hidden: the pointer has been away for 400 ms");
            }
        }
    }

    /// Asks the desktop whether a fullscreen application owns the foreground.
    fn check_fullscreen(&mut self) {
        // SAFETY: the call takes no arguments and returns a monotonically increasing millisecond count.
        let now = unsafe { GetTickCount64() };
        self.check_fullscreen_at(now);
    }

    /// Reacts to a change in who owns the screen: a game in front hides the strip, its absence brings
    /// it back.
    fn check_fullscreen_at(&mut self, now: u64) {
        self.last_check = now;

        let fullscreen = fullscreen::foreground_is_fullscreen();
        if fullscreen == self.fullscreen {
            return;
        }

        self.fullscreen = fullscreen;
        self.hover = None;

        if let Some(tooltip) = self.tooltip.as_mut() {
            tooltip.hide();
        }

        if fullscreen {
            crate::log_info!("dock", "hidden: a fullscreen application owns the screen");
        } else {
            crate::log_info!("dock", "shown again: the fullscreen application is gone");
        }

        self.show_or_hide();

        if self.visible() {
            self.paint();
        }
    }

    /// The pointer moved inside the strip: hover a tile, or none.
    fn pointer_moved(&mut self, x: i32, y: i32) {
        let hover = self.plan.slot_at(x, y);

        if hover == self.hover {
            return;
        }

        let now = now_ms();
        // The tile being left keeps its highlight while it fades out; the new one fades in.
        self.previous_hover = self.hover;
        self.previous_fade = Transition::scaled(
            now,
            self.hover_fade.value(now),
            0.0,
            HOVER_MS,
            &self.animations,
        );
        self.hover = hover;
        self.hover_fade = Transition::scaled(now, 0.0, 1.0, HOVER_MS, &self.animations);

        self.update_tooltip();
        self.arm_animation();

        if !self.animating {
            self.paint();
        }
    }

    /// The pointer left the strip.
    fn pointer_left(&mut self) {
        if self.hover.is_none() && self.previous_hover.is_none() {
            return;
        }

        let now = now_ms();
        self.previous_hover = self.hover.take();
        self.previous_fade = Transition::scaled(
            now,
            self.hover_fade.value(now),
            0.0,
            HOVER_MS,
            &self.animations,
        );
        self.hover_fade = Transition::settled(0.0);
        self.arm_animation();

        if !self.animating {
            self.paint();
        }
    }

    /// What a click at `(x, y)` asks the application to do.
    fn click(&mut self, x: i32, y: i32) -> Click {
        let Some(slot) = self.plan.slot_at(x, y) else {
            return Click::Nothing;
        };

        if slot >= self.plan.visible {
            return Click::Overflow;
        }

        super::click_for(self.items.get(slot), slot)
    }

    /// Points the tooltip at the hovered tile, or empties it when nothing is hovered.
    fn update_tooltip(&mut self) {
        if self.tooltip.is_none() {
            self.tooltip = Tooltip::new(self.hwnd);
        }

        let Some(tooltip) = self.tooltip.as_mut() else {
            return;
        };

        match self.hover {
            Some(slot) if slot < self.plan.visible => match self.items.get(slot) {
                Some(item) => {
                    let mut text: WideBuf<96> = WideBuf::new();
                    text.push_str(&item.name);
                    tooltip.show(tile_rect(&self.plan, slot), text.as_units());
                }
                None => tooltip.hide(),
            },
            Some(_) => {
                let mut text: WideBuf<32> = WideBuf::new();
                text.push_str(get(self.language, Text::DockMore));
                text.push(' ');
                text.push_number(u32::try_from(self.plan.overflow).unwrap_or(u32::MAX));

                let slot = self.plan.slots().saturating_sub(1);
                tooltip.show(tile_rect(&self.plan, slot), text.as_units());
            }
            None => tooltip.hide(),
        }
    }

    /// `WM_DPICHANGED`: the strip is on a monitor of a different scale now.
    fn dpi_changed(&mut self, dpi: u32) {
        let dpi = dpi.max(96);
        let previous = std::mem::replace(&mut self.monitor.dpi, dpi);
        if previous == dpi {
            return;
        }

        self.relayout();
        crate::log_info!(
            "dock",
            "dpi {previous} -> {dpi}, strip {}px tall",
            self.plan.height
        );
    }

    /// Hands the last frame to a device context, for `WM_PRINTCLIENT`.
    fn print(&self, hdc: HDC) -> bool {
        self.renderer
            .as_ref()
            .is_some_and(|renderer| renderer.blit_to(hdc))
    }
}

/// Builds a renderer, logging and giving up when Direct2D refuses.
fn make_renderer(
    width: u32,
    height: u32,
    monitor: &Monitor,
    theme: &Theme,
    language: Language,
) -> Option<DockRenderer> {
    match DockRenderer::new(width, height, monitor.dpi, theme, language) {
        Ok(renderer) => Some(renderer),
        Err(error) => {
            crate::log_warn!("dock", "cannot create the renderer: {error}");
            None
        }
    }
}

/// Plans the strip for `items` on `monitor`, in physical pixels.
fn plan_for(items: &[DockItem], monitor: &Monitor, tile_dip: i32) -> DockPlan {
    layout::plan(items.len(), monitor.rect, monitor.to_pixels(tile_dip))
}

/// The client rectangle of one slot, in physical pixels.
fn tile_rect(plan: &DockPlan, slot: usize) -> RECT {
    let metrics: Metrics = plan.metrics;
    let left = metrics.slot_left(slot);

    RECT {
        left,
        top: metrics.border + metrics.padding,
        right: left + metrics.tile,
        bottom: metrics.border + metrics.padding + metrics.tile,
    }
}

/// Exclusive access to one dock's state for the length of one window message.
///
/// The same contract as the bar's guard: a message this window sends to itself cannot borrow state its
/// own sender still holds, and falls through to `DefWindowProcW` instead.
#[derive(Debug)]
struct DockGuard {
    state: *mut DockState,
}

impl DockGuard {
    /// Takes the state of `hwnd`, or `None` when it is missing or already borrowed.
    ///
    /// # Safety
    ///
    /// `hwnd` must be a window this module created on this thread.
    unsafe fn enter(hwnd: HWND) -> Option<Self> {
        // SAFETY: the caller guarantees `hwnd` is one of our windows, whose user data is either null
        // or the `Box<DockState>` that `WM_NCCREATE` installed.
        let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };

        if pointer == 0 {
            return None;
        }

        let state = pointer as *mut DockState;

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

impl Deref for DockGuard {
    type Target = DockState;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe { &*self.state }
    }
}

impl DerefMut for DockGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe { &mut *self.state }
    }
}

impl Drop for DockGuard {
    fn drop(&mut self) {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe {
            (*self.state).busy = false;
        }
    }
}

/// The current pointer position in screen pixels, or the origin when the call fails.
fn cursor_position() -> (i32, i32) {
    let mut point = POINT::default();
    // SAFETY: `point` is a live out-parameter.
    let _ = unsafe { GetCursorPos(&mut point) };

    (point.x, point.y)
}

/// The dock window procedure.
unsafe extern "system" fn dock_proc(
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

                // `create_dock` boxed the spec and hands it over here; nothing else owns it.
                let spec = *Box::from_raw((*create).lpCreateParams.cast::<DockSpec>());
                let state = Box::new(DockState::new(hwnd, spec));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
                return LRESULT(1);
            }
            WM_NCDESTROY => {
                let pointer = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if pointer != 0 {
                    drop(Box::from_raw(pointer as *mut DockState));
                }
                return LRESULT(0);
            }
            _ => {}
        }

        let Some(mut state) = DockGuard::enter(hwnd) else {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        };

        match message {
            WM_PAINT => {
                // A layered window presents through `UpdateLayeredWindow`; the update region exists
                // only because Windows asked, so it is validated without drawing.
                let mut paint = PAINTSTRUCT::default();
                let _ = BeginPaint(hwnd, &mut paint);
                let _ = EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_PRINTCLIENT => {
                let _ = state.print(HDC(wparam.0 as *mut _));
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_MOUSEMOVE => {
                let (x, y) = mouse_position(lparam);
                state.pointer_moved(x, y);

                let mut track = TRACKMOUSEEVENT {
                    cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                    dwFlags: TME_LEAVE,
                    hwndTrack: hwnd,
                    dwHoverTime: 0,
                };
                let _ = TrackMouseEvent(&mut track);
                LRESULT(0)
            }
            WM_MOUSELEAVE => {
                state.pointer_left();
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let (x, y) = mouse_position(lparam);
                let click = state.click(x, y);

                if click != Click::Nothing {
                    // Recorded, not executed: a launch may block, and the loop applies it with
                    // nothing else borrowed.
                    bar_window::set_pending(|pending| pending.set_dock_click(click));
                }
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_POLL => {
                state.tick();
                LRESULT(0)
            }
            WM_TIMER if wparam.0 == TIMER_ANIM => {
                state.animation_step();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                state.dpi_changed((wparam.0 & 0xFFFF) as u32);
                LRESULT(0)
            }
            WM_DOCK_RUNNING => {
                bar_window::set_pending(|pending| pending.set_dock_refresh(true));
                LRESULT(0)
            }
            WM_NCHITTEST => LRESULT(isize::try_from(HTCLIENT).unwrap_or(1)),
            WM_DESTROY => {
                let _ = KillTimer(Some(hwnd), TIMER_POLL);
                LRESULT(0)
            }
            WM_CLOSE => {
                bar_window::request_quit();
                LRESULT(0)
            }
            _ if message == bar_window::taskbar_created() => {
                // Explorer restarted: the tooltip's owner window is gone with it, so the control is
                // recreated on the next hover rather than aimed at a dead owner.
                state.tooltip = None;
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

/// The two coordinates packed into a mouse message.
fn mouse_position(lparam: LPARAM) -> (i32, i32) {
    let packed = lparam.0 as u32;
    let x = (packed & 0xFFFF) as u16 as i16;
    let y = ((packed >> 16) & 0xFFFF) as u16 as i16;

    (i32::from(x), i32::from(y))
}

/// The tooltip that names the tile under the pointer.
///
/// One tool, re-targeted at the hovered tile: a tool per tile would cost a window's worth of
/// bookkeeping for a control that can only ever show one thing at a time.
#[derive(Debug)]
struct Tooltip {
    hwnd: HWND,
    owner: HWND,
    text: Vec<u16>,
}

impl Tooltip {
    /// A tooltip owned by `owner`, or `None` when Windows refuses the control.
    fn new(owner: HWND) -> Option<Self> {
        // SAFETY: the class name is a static wide string, the window is a popup with no size, and
        // `owner` is a live window of this process. A null instance is required for a system class.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST,
                TOOLTIPS_CLASSW,
                PCWSTR::null(),
                WS_POPUPWINDOW | WINDOW_STYLE(TTS_ALWAYSTIP | TTS_NOPREFIX),
                0,
                0,
                0,
                0,
                Some(owner),
                None,
                None,
                None,
            )
        }
        .ok()?;

        let mut tooltip = Self {
            hwnd,
            owner,
            text: vec![0u16; 128],
        };

        // SAFETY: `hwnd` is the control just created, and the tool structure points at this value's
        // own text buffer, which outlives every message it is sent with.
        unsafe {
            let _ = SendMessageW(hwnd, TTM_SETMAXTIPWIDTH, None, Some(LPARAM(400)));

            let mut tool = tooltip.tool(RECT::default());
            let _ = SendMessageW(
                hwnd,
                TTM_ADDTOOLW,
                None,
                Some(LPARAM((&raw mut tool) as isize)),
            );
        }

        Some(tooltip)
    }

    /// The tool description, pointing at this value's text buffer.
    fn tool(&mut self, rect: RECT) -> TTTOOLINFOW {
        TTTOOLINFOW {
            cbSize: std::mem::size_of::<TTTOOLINFOW>() as u32,
            uFlags: TTF_SUBCLASS,
            hwnd: self.owner,
            uId: TOOL_ID,
            rect,
            hinst: Default::default(),
            lpszText: PWSTR(self.text.as_mut_ptr()),
            lParam: LPARAM(0),
            lpReserved: std::ptr::null_mut(),
        }
    }

    /// Points the tooltip at `rect` with `text`.
    fn show(&mut self, rect: RECT, text: &[u16]) {
        self.text.iter_mut().for_each(|unit| *unit = 0);

        for (slot, unit) in self.text.iter_mut().zip(text.iter().copied()) {
            *slot = unit;
        }

        // SAFETY: `self.hwnd` is the tooltip control and each tool structure borrows this value's own
        // text buffer for the duration of its call.
        unsafe {
            let mut tool = self.tool(rect);
            let _ = SendMessageW(
                self.hwnd,
                TTM_UPDATETIPTEXTW,
                None,
                Some(LPARAM((&raw mut tool) as isize)),
            );

            let mut tool = self.tool(rect);
            let _ = SendMessageW(
                self.hwnd,
                TTM_NEWTOOLRECTW,
                None,
                Some(LPARAM((&raw mut tool) as isize)),
            );
        }
    }

    /// Leaves the tooltip with nothing to say.
    fn hide(&mut self) {
        self.text.iter_mut().for_each(|unit| *unit = 0);
    }
}

impl Drop for Tooltip {
    fn drop(&mut self) {
        // SAFETY: `hwnd` is the control this value created and is destroyed exactly once, here.
        unsafe {
            let _ = DestroyWindow(self.hwnd);
        }
    }
}

static DOCK_CLASS_REGISTERED: OnceLock<bool> = OnceLock::new();

/// This process's `HINSTANCE`.
fn instance() -> windows::Win32::Foundation::HINSTANCE {
    // SAFETY: a null module name asks for this process's own module.
    match unsafe { GetModuleHandleW(None) } {
        Ok(module) => windows::Win32::Foundation::HINSTANCE(module.0),
        Err(_) => windows::Win32::Foundation::HINSTANCE::default(),
    }
}

/// Registers the dock class once per process.
fn register_class() -> Result<(), WindowError> {
    let registered = *DOCK_CLASS_REGISTERED.get_or_init(|| {
        let name = wide(DOCK_CLASS);
        let window_class = WNDCLASSW {
            lpfnWndProc: Some(dock_proc),
            hInstance: instance(),
            lpszClassName: PCWSTR(name.as_ptr()),
            hCursor: unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default(),
            ..WNDCLASSW::default()
        };

        // SAFETY: `window_class` is fully initialised and the class name it points at outlives the
        // call; a null background brush keeps the strip from flashing before the first frame. The
        // arrow cursor is set on the class so a stale "app starting" spinner never sticks to the dock.
        unsafe { RegisterClassW(&window_class) != 0 }
    });

    if registered {
        Ok(())
    } else {
        Err(WindowError::Class { class: DOCK_CLASS })
    }
}

/// Creates the dock for `spec` and attaches it to its monitor.
///
/// # Errors
///
/// [`WindowError`] when Windows refuses the class or the window, or the state never arrives.
pub fn create_dock(spec: DockSpec) -> Result<HWND, WindowError> {
    register_class()?;

    let plan = plan_for(&spec.items, &spec.monitor, spec.tile_dip);
    let rect = layout::place(&spec.monitor, &plan);
    let class = wide(DOCK_CLASS);
    let title = wide("ProjectNeb dock");
    let parameter = Box::into_raw(Box::new(spec));

    // SAFETY: both strings are NUL-terminated and outlive the call; `parameter` is a heap `DockSpec`
    // that `WM_NCCREATE` takes ownership of, or that is reclaimed below when the call fails.
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_TOPMOST,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
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
                class: DOCK_CLASS,
                source,
            });
        }
    };

    // SAFETY: `hwnd` was created a line above, on this thread.
    let mut state = unsafe { DockGuard::enter(hwnd) }.ok_or(WindowError::NoState(DOCK_CLASS))?;
    state.attach();

    Ok(hwnd)
}

/// Destroys one dock, which stops its timer and drops its tooltip on the way out.
pub fn destroy_dock(hwnd: HWND) {
    // SAFETY: `hwnd` is a live dock window of this process.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// Replaces the tiles of `hwnd`.
pub fn set_items(hwnd: HWND, items: Vec<DockItem>) {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    if let Some(mut state) = unsafe { DockGuard::enter(hwnd) } {
        state.set_items(items);
    }
}

/// The item in `slot`, for the application that is about to launch or activate it.
pub fn item(hwnd: HWND, slot: usize) -> Option<DockItem> {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    let state = unsafe { DockGuard::enter(hwnd) }?;
    state.items.get(slot).cloned()
}

/// The plan the strip is drawn with, for the tests and the live harness.
pub fn plan_of(hwnd: HWND) -> Option<DockPlan> {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    let state = unsafe { DockGuard::enter(hwnd) }?;
    Some(state.plan)
}

/// The strip's rectangle in physical pixels.
pub fn rect_of(hwnd: HWND) -> Option<Rect> {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    let state = unsafe { DockGuard::enter(hwnd) }?;
    Some(state.rect)
}

/// Whether the strip is currently out of the way.
pub fn is_hidden(hwnd: HWND) -> bool {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    unsafe { DockGuard::enter(hwnd) }.is_some_and(|state| !state.visible())
}

/// How many frames this dock has actually drawn; zero means the paint path never ran.
pub fn painted_frames(hwnd: HWND) -> u64 {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    unsafe { DockGuard::enter(hwnd) }.map_or(0, |state| state.frames)
}

/// Paints `hwnd` synchronously, without waiting for the queue.
///
/// `WM_PAINT` on a layered window only validates, so the frame is produced here; the allocation
/// harness in `tests/paint_no_alloc.rs` is what needs the synchronous entry point.
pub fn repaint_now(hwnd: HWND) {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    if let Some(mut state) = unsafe { DockGuard::enter(hwnd) } {
        state.paint();
    }
}

/// Copies the last frame out of `hwnd`, for a screenshot guard or a probe.
pub fn capture(hwnd: HWND) -> Option<(u32, u32, Vec<u8>)> {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    let mut state = unsafe { DockGuard::enter(hwnd) }?;
    let renderer = state.renderer.as_mut()?;
    renderer.capture();
    let (width, height) = renderer.pixel_size();

    Some((width, height, renderer.pixels().to_vec()))
}

/// Posts "a new running snapshot is ready" to `hwnd`.
pub fn post_running(hwnd: isize) {
    let hwnd = HWND(hwnd as *mut std::ffi::c_void);

    // SAFETY: the handle came from this process's own dock; a vanished window makes the post fail,
    // which is the expected race during shutdown and needs no handling.
    unsafe {
        let _ = PostMessageW(Some(hwnd), WM_DOCK_RUNNING, WPARAM(0), LPARAM(0));
    }
}

/// How often the running set is re-read.
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(5);

/// The background worker that keeps one snapshot of the running applications fresh.
///
/// It owns the thread and the snapshot; the UI thread only ever asks for the newest copy, which is
/// why a slow or refused `EnumWindows` can never stall a frame or a click.
#[derive(Debug)]
pub struct Worker {
    latest: Arc<Mutex<Vec<RunningApp>>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Worker {
    /// Starts sampling, posting [`WM_DOCK_RUNNING`] to `hwnd` after every pass.
    pub fn start(hwnd: isize) -> Self {
        let latest = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_latest = Arc::clone(&latest);
        let thread_stop = Arc::clone(&stop);

        let thread = std::thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                let snapshot = running::enumerate();

                if let Ok(mut guard) = thread_latest.lock() {
                    *guard = snapshot;
                }

                post_running(hwnd);

                // Sleeping in slices keeps the worker responsive to `stop` without a second handle.
                let mut waited = Duration::ZERO;
                while waited < REFRESH_INTERVAL && !thread_stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(100));
                    waited += Duration::from_millis(100);
                }
            }
        });

        Self {
            latest,
            stop,
            thread: Some(thread),
        }
    }

    /// The newest snapshot; empty before the first pass finishes.
    pub fn latest(&self) -> Vec<RunningApp> {
        self.latest
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Asks the worker to finish and waits for it; idempotent.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);

        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Feeds the auto-hide policy a pointer sample without touching the mouse.
///
/// The sample goes through the same tick a real one does; only its source differs. Used by the tests
/// and by a probe, because automating the desktop's pointer is not something this process does.
pub fn probe_pointer(hwnd: HWND, x: i32, y: i32) {
    // SAFETY: `hwnd` is a live dock window of this process, created on this thread.
    if let Some(mut state) = unsafe { DockGuard::enter(hwnd) } {
        state.pointer = Some((x, y));
        state.tick();
    }
}

/// The handle of the window that owns the foreground, for a harness that checks an activation.
pub fn foreground() -> isize {
    // SAFETY: the call takes no arguments and returns a handle this process does not own.
    unsafe { GetForegroundWindow().0 as isize }
}

/// The client size of `hwnd` in pixels, which a harness compares with the plan's width.
pub fn client_size(hwnd: HWND) -> Option<(i32, i32)> {
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is a live window and `rect` is a live out-parameter.
    unsafe { GetClientRect(hwnd, &mut rect) }.ok()?;

    Some((rect.right, rect.bottom))
}

/// The window rectangle of `hwnd` in screen pixels.
pub fn window_rect(hwnd: HWND) -> Option<Rect> {
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is a live window and `rect` is a live out-parameter.
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;

    Some(Rect::from(rect))
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::dock::layout::TILE_DIP;

    #[test]
    fn the_custom_message_continues_the_bar_sequence() {
        assert_eq!(WM_DOCK_RUNNING, 0x8000 + 5);
        assert_ne!(WM_DOCK_RUNNING, bar_window::WM_VISIBILITY);
    }

    #[test]
    fn a_mouse_message_is_unpacked_as_two_signed_shorts() {
        // x = 700, y = 40 packed the way Windows packs it.
        let lparam = LPARAM(((40u32 << 16) | 700u32) as isize);

        assert_eq!(mouse_position(lparam), (700, 40));
    }

    #[test]
    fn a_negative_coordinate_survives_the_unpacking() {
        let packed = ((-5i32 as u32 & 0xFFFF) << 16) | (-3i32 as u32 & 0xFFFF);

        assert_eq!(mouse_position(LPARAM(packed as isize)), (-3, -5));
    }

    #[test]
    fn the_tile_rect_starts_after_the_padding() {
        let plan = layout::plan(3, Rect::new(0, 0, 1920, 1080), TILE_DIP);
        let rect = tile_rect(&plan, 0);

        assert_eq!(rect.left, plan.metrics.border + plan.metrics.padding);
        assert_eq!(rect.right - rect.left, plan.metrics.tile);
        assert_eq!(rect.top, plan.metrics.border + plan.metrics.padding);
    }
}
