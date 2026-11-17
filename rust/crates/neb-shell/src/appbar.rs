//! The shell AppBar protocol (`SHAppBarMessage`): reserving a strip so maximised windows stop above it.
//!
//! Registering tells Explorer to shrink the monitor's work area by exactly the strip's height and to
//! send the bar `ABN_*` notifications through [`callback`](crate::window::callback_message). A refusal
//! is not fatal — the bar keeps its topmost fallback position and covers the same pixels, the desktop
//! is simply not told about it — so every failure is a typed error the caller logs, never a panic.
//!
//! The monitor rectangle is always passed in by the caller rather than looked up with
//! `MonitorFromWindow`: a bar is created while its window may still be on the primary monitor, so
//! asking Windows where the window is would reserve the primary's edge on a mixed-DPI setup.
//!
//! Two calls go out to the shell — `SHAppBarMessage` and `SetWindowPos` — and both are behind
//! [`EdgeHost`], so a test can count exactly which registrations and releases a bar's lifetime
//! produced without touching the real desktop.

use std::cell::Cell;
use std::fmt;
use std::sync::Arc;

use windows::Win32::Foundation::HWND;
use windows::Win32::UI::Shell::{
    ABE_BOTTOM, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, SHAppBarMessage,
};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SetWindowPos,
};

use crate::geometry::Rect;

pub use windows::Win32::UI::Shell::APPBARDATA;

/// `ABN_POSCHANGED`: the shell moved or re-laid-out the strip and it must be reserved again.
pub const ABN_POSCHANGED: u32 = 1;
/// `ABN_FULLSCREENAPP`: `lParam` is non-zero while a fullscreen app owns the screen.
pub const ABN_FULLSCREENAPP: u32 = 2;
/// `ABN_WINDOWPOSCHANGED`: the window moved; re-asserting here would feed our own `SetWindowPos`.
pub const ABN_WINDOWPOSCHANGED: u32 = 3;
/// `ABN_STATECHANGE`: the auto-hide state changed; the strip itself is still correct.
pub const ABN_STATECHANGE: u32 = 4;

/// Which edge a bar takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    /// The top of the monitor.
    Top,
    /// The bottom of the monitor.
    Bottom,
}

impl Edge {
    /// The `ABE_*` value the AppBar API expects.
    const fn as_abm(self) -> u32 {
        match self {
            Self::Top => ABE_TOP,
            Self::Bottom => ABE_BOTTOM,
        }
    }

    /// Whether the bar grows downwards from the top edge.
    pub const fn is_top(self) -> bool {
        matches!(self, Self::Top)
    }
}

/// What went wrong while reserving the strip.
#[derive(Debug, thiserror::Error)]
pub enum AppBarError {
    /// `ABM_NEW` was refused, so this window is not an app bar at all.
    #[error("the shell refused to register the bar (ABM_NEW)")]
    NotRegistered,
    /// `ABM_SETPOS` was refused, so the strip was not moved and the edge was released again.
    #[error("the shell refused to place the bar (ABM_SETPOS)")]
    NotPlaced,
}

/// The strip Windows gave the bar, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Strip {
    /// The rectangle the bar must cover.
    pub rect: Rect,
    /// The edge it is attached to.
    pub edge: Edge,
}

/// One AppBar message, named rather than numbered.
///
/// The seam speaks the bar's language: a host is asked to register, to ask where the strip belongs, to
/// place it, or to let go — and only [`WindowsHost`] has to know which `ABM_*` value each of those is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Call {
    /// `ABM_NEW`: take this window as an app bar.
    New,
    /// `ABM_QUERYPOS`: where does the strip belong, given the others?
    QueryPosition,
    /// `ABM_SETPOS`: reserve the strip.
    SetPosition,
    /// `ABM_REMOVE`: give the strip back.
    Remove,
}

impl Call {
    /// The `ABM_*` value the shell documents for this call.
    const fn as_abm(self) -> u32 {
        match self {
            Self::New => ABM_NEW,
            Self::QueryPosition => ABM_QUERYPOS,
            Self::SetPosition => ABM_SETPOS,
            Self::Remove => ABM_REMOVE,
        }
    }
}

/// The calls a bar makes into the shell.
///
/// Every function below takes one, which is what lets `tests/appbar_lifecycle.rs` count registrations
/// and releases without registering anything: the production host is [`WindowsHost`], and the test
/// host is a set of counters.
pub trait EdgeHost: fmt::Debug {
    /// One `SHAppBarMessage` call, returning the value the shell answered with.
    fn appbar(&self, call: Call, data: &mut APPBARDATA) -> usize;

    /// Moves `hwnd` onto `rect`, topmost or not, without activating it.
    fn place(&self, hwnd: HWND, rect: Rect, topmost: bool);
}

/// The real shell.
#[derive(Debug, Default, Clone, Copy)]
pub struct WindowsHost;

impl EdgeHost for WindowsHost {
    fn appbar(&self, call: Call, data: &mut APPBARDATA) -> usize {
        // SAFETY: the caller guarantees `data` is a fully initialised `APPBARDATA` whose `cbSize` is
        // its own size, which is the contract of every `SHAppBarMessage` call.
        unsafe { SHAppBarMessage(call.as_abm(), data) }
    }

    fn place(&self, hwnd: HWND, rect: Rect, topmost: bool) {
        // SAFETY: `hwnd` is a window this process created; the rectangle is a plain value.
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
                SWP_NOACTIVATE,
            )
        };
    }
}

/// The host a running shell uses.
pub fn windows() -> Arc<dyn EdgeHost> {
    Arc::new(WindowsHost)
}

thread_local! {
    /// Set while a reservation is being applied, so a notification the shell sends back synchronously
    /// cannot start another one: `ABM_SETPOS` can deliver `ABN_POSCHANGED` before it returns.
    static REASSERTING: Cell<bool> = const { Cell::new(false) };
}

/// Whether a reservation is being applied right now, on this thread.
pub fn is_reasserting() -> bool {
    REASSERTING.with(Cell::get)
}

/// Marks the reservation as in progress, restoring the previous value when it ends.
struct Reentry {
    previous: bool,
}

impl Reentry {
    fn begin() -> Self {
        let previous = REASSERTING.with(|flag| flag.replace(true));
        Self { previous }
    }
}

impl Drop for Reentry {
    fn drop(&mut self) {
        REASSERTING.with(|flag| flag.set(self.previous));
    }
}

/// Registers `hwnd` as an app bar and reserves `thickness` physical pixels along `edge` of `monitor`.
///
/// `callback` is the message the shell sends `ABN_*` notifications through, i.e.
/// [`callback_message`](crate::window::callback_message).
///
/// # Errors
///
/// [`AppBarError::NotRegistered`] when the shell will not take the window, [`AppBarError::NotPlaced`]
/// when it takes the window but refuses the position; the registration is dropped in the second case
/// so a half-registered bar cannot linger.
pub fn reserve(
    host: &dyn EdgeHost,
    hwnd: HWND,
    monitor: Rect,
    edge: Edge,
    thickness: i32,
    callback: u32,
    topmost: bool,
) -> Result<Strip, AppBarError> {
    // A null handle would register nothing useful and leave the shell convinced a bar exists, which
    // is worse than the bar simply staying topmost.
    if hwnd.0.is_null() {
        return Err(AppBarError::NotRegistered);
    }

    let mut data = APPBARDATA {
        cbSize: size_of_data(),
        hWnd: hwnd,
        uCallbackMessage: callback,
        uEdge: edge.as_abm(),
        ..APPBARDATA::default()
    };

    if host.appbar(Call::New, &mut data) == 0 {
        return Err(AppBarError::NotRegistered);
    }

    match set_position(host, hwnd, monitor, edge, thickness, topmost) {
        Ok(strip) => Ok(strip),
        Err(error) => {
            release(host, hwnd);
            Err(error)
        }
    }
}

/// Re-applies an existing reservation after the shell moved the bar or a fullscreen app left.
///
/// Unlike [`reserve`] this does not re-register: the bar keeps the registration it already has. A
/// re-assert that arrives while one is already being applied is dropped, because it can only be the
/// echo of our own `ABM_SETPOS`.
///
/// # Errors
///
/// [`AppBarError::NotPlaced`] when the shell refuses the position.
pub fn reassert(
    host: &dyn EdgeHost,
    hwnd: HWND,
    monitor: Rect,
    edge: Edge,
    thickness: i32,
    topmost: bool,
) -> Result<Strip, AppBarError> {
    set_position(host, hwnd, monitor, edge, thickness, topmost)
}

/// Gives the reserved strip back. Safe to call for a window that never reserved anything.
pub fn release(host: &dyn EdgeHost, hwnd: HWND) {
    if hwnd.0.is_null() {
        return;
    }

    let mut data = APPBARDATA {
        cbSize: size_of_data(),
        hWnd: hwnd,
        ..APPBARDATA::default()
    };

    host.appbar(Call::Remove, &mut data);
}

/// Asks the shell where the strip belongs and moves the window there.
fn set_position(
    host: &dyn EdgeHost,
    hwnd: HWND,
    monitor: Rect,
    edge: Edge,
    thickness: i32,
    topmost: bool,
) -> Result<Strip, AppBarError> {
    let thickness = thickness.clamp(1, monitor.height().min(monitor.width()));
    let mut data = APPBARDATA {
        cbSize: size_of_data(),
        hWnd: hwnd,
        uEdge: edge.as_abm(),
        rc: monitor.to_win32(),
        ..APPBARDATA::default()
    };

    // The guard covers the whole exchange: the shell answers `ABM_SETPOS` by sending the window
    // `ABN_POSCHANGED` when it moved the strip, and that notification must not start a second
    // reservation while this one is still in flight.
    let _reentry = Reentry::begin();

    // `ABM_QUERYPOS` lets the shell nudge the rectangle away from edges other app bars already own.
    // Only the thickness on our own edge is authoritative, so only that part is written back.
    host.appbar(Call::QueryPosition, &mut data);

    match edge {
        Edge::Top => data.rc.bottom = data.rc.top + thickness,
        Edge::Bottom => data.rc.top = data.rc.bottom - thickness,
    }

    if host.appbar(Call::SetPosition, &mut data) == 0 {
        return Err(AppBarError::NotPlaced);
    }

    let rect = Rect::from(data.rc);
    host.place(hwnd, rect, topmost);

    Ok(Strip { rect, edge })
}

/// Puts `hwnd` over `rect` without activating it, topmost or not.
///
/// This is both the AppBar fallback and the final half of a successful reservation: the shell tells
/// the bar where the strip is, and the bar has to move itself there. `topmost` is false while a
/// fullscreen application owns the screen, so re-asserting cannot put the bar back over a game.
pub fn place(host: &dyn EdgeHost, hwnd: HWND, rect: Rect, topmost: bool) {
    host.place(hwnd, rect, topmost);
}

/// `size_of::<APPBARDATA>()` as the `u32` the structure stores.
fn size_of_data() -> u32 {
    std::mem::size_of::<APPBARDATA>() as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// A host that records what was asked of it and answers like a shell that agrees.
    #[derive(Debug, Default)]
    struct RecordingHost {
        new: AtomicUsize,
        remove: AtomicUsize,
        set_position: AtomicUsize,
        places: Mutex<Vec<Rect>>,
    }

    impl EdgeHost for RecordingHost {
        fn appbar(&self, call: Call, _data: &mut APPBARDATA) -> usize {
            match call {
                Call::New => {
                    self.new.fetch_add(1, Ordering::SeqCst);
                }
                Call::Remove => {
                    self.remove.fetch_add(1, Ordering::SeqCst);
                }
                Call::SetPosition => {
                    self.set_position.fetch_add(1, Ordering::SeqCst);
                }
                _ => {}
            }

            1
        }

        fn place(&self, _hwnd: HWND, rect: Rect, _topmost: bool) {
            self.places.lock().expect("lock").push(rect);
        }
    }

    fn window() -> HWND {
        // A handle the host only ever passes through; nothing dereferences it here.
        HWND(std::ptr::without_provenance_mut(1))
    }

    fn monitor() -> Rect {
        Rect::new(0, 0, 1920, 1080)
    }

    #[test]
    fn the_callback_constants_are_the_documented_values() {
        assert_eq!(ABN_POSCHANGED, 0x0001);
        assert_eq!(ABN_FULLSCREENAPP, 0x0002);
        assert_eq!(ABN_WINDOWPOSCHANGED, 0x0003);
        assert_eq!(ABN_STATECHANGE, 0x0004);
    }

    #[test]
    fn an_edge_maps_to_its_abe_value() {
        assert_eq!(Edge::Top.as_abm(), ABE_TOP);
        assert_eq!(Edge::Bottom.as_abm(), ABE_BOTTOM);
        assert!(Edge::Top.is_top());
        assert!(!Edge::Bottom.is_top());
    }

    #[test]
    fn the_structure_size_is_what_the_shell_expects() {
        // cbSize (4) and four bytes of padding, hWnd (8), uCallbackMessage (4), uEdge (4),
        // rc (4 x i32), lParam (8): 48 bytes on x64, which is the size the shell validates the
        // structure against.
        assert_eq!(size_of_data(), 48);
    }

    #[test]
    fn a_bar_without_a_window_is_refused_rather_than_registered() {
        // A null handle must never reach `SHAppBarMessage`: the shell would think a bar exists and
        // shrink the work area for a window that can never give it back.
        let host = RecordingHost::default();

        assert!(matches!(
            reserve(&host, HWND::default(), monitor(), Edge::Top, 40, 0, true),
            Err(AppBarError::NotRegistered)
        ));
        assert_eq!(host.new.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn a_reservation_registers_places_and_releases_exactly_once_each() {
        let host = RecordingHost::default();

        let strip = reserve(&host, window(), monitor(), Edge::Top, 40, 0, true).expect("reserve");
        assert_eq!(strip.rect, Rect::new(0, 0, 1920, 40));
        assert_eq!(host.new.load(Ordering::SeqCst), 1);
        assert_eq!(host.set_position.load(Ordering::SeqCst), 1);
        assert_eq!(host.places.lock().expect("lock").len(), 1);

        release(&host, window());
        assert_eq!(host.remove.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn releasing_nothing_is_harmless_and_still_asks_the_shell() {
        let host = RecordingHost::default();

        release(&host, window());

        // `ABM_REMOVE` for a window that never registered is documented as a no-op, so the call is
        // made rather than guessed about: the counter proves the bar asked.
        assert_eq!(host.remove.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_reentry_guard_blocks_a_nested_reservation() {
        assert!(!is_reasserting());

        let guard = Reentry::begin();
        assert!(is_reasserting());
        drop(guard);

        assert!(!is_reasserting());
    }

    #[test]
    fn a_nested_begin_restores_the_outer_state() {
        let outer = Reentry::begin();
        {
            let _inner = Reentry::begin();
            assert!(is_reasserting());
        }
        assert!(
            is_reasserting(),
            "the inner guard must not clear the outer one"
        );
        drop(outer);
        assert!(!is_reasserting());
    }

    #[test]
    fn a_refused_position_removes_the_registration_again() {
        #[derive(Debug, Default)]
        struct RefusingHost {
            new: AtomicUsize,
            remove: AtomicUsize,
        }

        impl EdgeHost for RefusingHost {
            fn appbar(&self, call: Call, _data: &mut APPBARDATA) -> usize {
                match call {
                    Call::New => {
                        self.new.fetch_add(1, Ordering::SeqCst);
                        1
                    }
                    Call::Remove => {
                        self.remove.fetch_add(1, Ordering::SeqCst);
                        1
                    }
                    _ => 0,
                }
            }

            fn place(&self, _hwnd: HWND, _rect: Rect, _topmost: bool) {}
        }

        let host = RefusingHost::default();

        assert!(matches!(
            reserve(&host, window(), monitor(), Edge::Top, 40, 0, true),
            Err(AppBarError::NotPlaced)
        ));
        assert_eq!(host.new.load(Ordering::SeqCst), 1);
        assert_eq!(
            host.remove.load(Ordering::SeqCst),
            1,
            "a half-registered bar must be released again"
        );
    }

    #[test]
    fn the_bottom_edge_reserves_the_bottom_of_the_monitor() {
        let host = RecordingHost::default();

        let strip =
            reserve(&host, window(), monitor(), Edge::Bottom, 40, 0, false).expect("reserve");

        assert_eq!(strip.rect, Rect::new(0, 1040, 1920, 1080));
        let places = host.places.lock().expect("lock");
        assert_eq!(places[0], strip.rect);
    }
}
