//! The wallpaper's end of `\\.\pipe\ProjectNeb.wallpaper`.
//!
//! Four methods, all of them cheap: `wallpaper.ping` and `wallpaper.getState` answer from atomics,
//! `wallpaper.reload` and `wallpaper.stop` post a message to the loop and answer immediately.
//! Nothing here waits for the loop, which is the rule that keeps a diagnostic client from freezing
//! the desktop and a desktop that is presenting from stalling a client.

use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use neb_core::ipc::{IpcError, Request, Response, ServerHandle, serve};
use serde_json::{Value, json};
use windows::Win32::Foundation::HWND;

use crate::desktop::attach::{Slot, SlotKind};
use crate::desktop::window::{self, WM_RELOAD, WM_STOP};
use crate::power::PauseState;

/// Pipe name shared with `Neb.App` and with the C# wallpaper.
pub const WALLPAPER_PIPE: &str = "ProjectNeb.wallpaper";

/// State the IPC handlers read, shared with the loop that writes it.
#[derive(Debug)]
pub struct Shared {
    /// The loop's control window; IPC posts to it and never calls into the loop directly.
    control: AtomicIsize,
    /// The first surface's window handle, for the report.
    window: AtomicIsize,
    /// The desktop slot the surfaces hang from, or zero on the fallback path.
    parent: AtomicIsize,
    /// `SlotKind` as a small integer; 0 none, 1 Progman child, 2 sibling.
    slot_kind: AtomicU32,
    /// How many surfaces are on screen.
    monitors: AtomicU32,
    /// Whether every surface is inside the desktop.
    attached: AtomicBool,
    /// Whether the pause rules have the scene stopped.
    paused: AtomicBool,
    /// Whether frames are going to the screen right now.
    presenting: AtomicBool,
    /// Frames presented since start, summed over the surfaces.
    frames: AtomicU64,
    /// The last sampled pause inputs.
    fullscreen: AtomicBool,
    on_battery: AtomicBool,
    locked: AtomicBool,
    /// The colour the scene was built from, after `wallpaper.reload`.
    colour: Mutex<String>,
}

impl Shared {
    /// Shares `control` and reports an empty desktop until the loop says otherwise.
    pub fn new(control: HWND) -> Self {
        Self {
            control: AtomicIsize::new(control.0 as isize),
            window: AtomicIsize::new(0),
            parent: AtomicIsize::new(0),
            slot_kind: AtomicU32::new(0),
            monitors: AtomicU32::new(0),
            attached: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            // A fresh wallpaper presents its first frame before the pause rules are first sampled, so
            // "not presenting" is a state only a real pause puts it in.
            presenting: AtomicBool::new(true),
            frames: AtomicU64::new(0),
            fullscreen: AtomicBool::new(false),
            on_battery: AtomicBool::new(false),
            locked: AtomicBool::new(false),
            colour: Mutex::new(crate::config::normalise_colour(
                neb_core::config::DEFAULT_WALLPAPER_COLOR,
            )),
        }
    }

    /// The control window IPC posts to.
    pub fn control(&self) -> HWND {
        HWND(self.control.load(Ordering::Relaxed) as *mut core::ffi::c_void)
    }

    /// Records where the surfaces ended up.
    pub fn record_placement(&self, slot: &Slot, window: HWND, attached: bool) {
        self.window.store(window.0 as isize, Ordering::Relaxed);
        self.parent.store(slot.slot.0 as isize, Ordering::Relaxed);
        self.slot_kind.store(slot.kind.code(), Ordering::Relaxed);
        self.attached.store(attached, Ordering::Relaxed);
    }

    /// Records how many surfaces exist and how many are attached.
    pub fn set_monitors(&self, total: usize, attached: usize) {
        self.monitors
            .store(u32::try_from(total).unwrap_or(u32::MAX), Ordering::Relaxed);
        self.attached
            .store(attached == total && total > 0, Ordering::Relaxed);
    }

    /// Records the pause decision.
    pub fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::Relaxed);
        self.presenting.store(!paused, Ordering::Relaxed);
    }

    /// Records the sampled pause inputs.
    pub fn set_state(&self, state: PauseState) {
        self.fullscreen.store(state.fullscreen, Ordering::Relaxed);
        self.on_battery.store(state.on_battery, Ordering::Relaxed);
        self.locked.store(state.locked, Ordering::Relaxed);
    }

    /// Adds presented frames to the running total.
    pub fn add_frames(&self, frames: u64) {
        self.frames.fetch_add(frames, Ordering::Relaxed);
    }

    /// Records the colour the scene is built from.
    pub fn set_colour(&self, colour: &str) {
        if let Ok(mut current) = self.colour.lock() {
            *current = crate::config::normalise_colour(colour);
        }
    }

    /// The answer to `wallpaper.getState`: everything a client can ask about, in one object.
    pub fn state(&self) -> Value {
        let colour = self.colour.lock().map_or_else(
            |_| crate::config::normalise_colour(""),
            |colour| colour.clone(),
        );

        json!({
            "ok": true,
            "pid": std::process::id(),
            "version": crate::VERSION,
            "paused": self.paused.load(Ordering::Relaxed),
            "presenting": self.presenting.load(Ordering::Relaxed),
            "attached": self.attached.load(Ordering::Relaxed),
            "hwnd": format!("0x{:x}", self.window.load(Ordering::Relaxed)),
            "parent": format!("0x{:x}", self.parent.load(Ordering::Relaxed)),
            "slot": slot_name(self.slot_kind.load(Ordering::Relaxed)),
            "monitors": self.monitors.load(Ordering::Relaxed),
            "frames": self.frames.load(Ordering::Relaxed),
            "fullscreen": self.fullscreen.load(Ordering::Relaxed),
            "onBattery": self.on_battery.load(Ordering::Relaxed),
            "locked": self.locked.load(Ordering::Relaxed),
            "color": colour,
        })
    }
}

/// The slot kind as the answer names it.
fn slot_name(code: u32) -> &'static str {
    SlotKind::from_code(code).as_str()
}

/// Starts answering on [`WALLPAPER_PIPE`].
///
/// # Errors
///
/// [`IpcError`] when the pipe cannot be created; the wallpaper then has no diagnostic surface but
/// still draws, which the caller logs rather than treats as fatal.
pub fn start(shared: Arc<Shared>) -> Result<ServerHandle, IpcError> {
    serve(WALLPAPER_PIPE, move |request: Request| {
        dispatch(&shared, request)
    })
}

/// Turns one request into one response. Never blocks on the loop thread.
fn dispatch(shared: &Shared, request: Request) -> Response {
    let id = request.id.clone();

    match request.method.as_str() {
        "wallpaper.ping" | "wallpaper.getState" => Response::success(id, shared.state()),
        "wallpaper.reload" => {
            if window::post(shared.control(), WM_RELOAD, 0) {
                Response::success(id, shared.state())
            } else {
                Response::failure(id, "the wallpaper loop is not running")
            }
        }
        "wallpaper.stop" => {
            if window::post(shared.control(), WM_STOP, 0) {
                Response::success(
                    id,
                    json!({ "ok": true, "stopping": true, "pid": std::process::id() }),
                )
            } else {
                Response::failure(id, "the wallpaper loop is not running")
            }
        }
        other => Response::failure(id, format!("unknown method '{other}'")),
    }
}

/// A pipe name no test run can collide with.
#[cfg(test)]
pub fn unique_pipe(tag: &str) -> String {
    use std::sync::atomic::AtomicUsize;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    format!(
        "{WALLPAPER_PIPE}.test.{tag}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::desktop::SlotKind;

    fn shared() -> Arc<Shared> {
        Arc::new(Shared::new(HWND::default()))
    }

    fn request(method: &str) -> Request {
        Request {
            id: "7".to_owned(),
            method: method.to_owned(),
            params: None,
        }
    }

    #[test]
    fn a_fresh_state_reports_nothing_on_screen() {
        let state = shared().state();

        assert_eq!(state["ok"], json!(true));
        assert_eq!(state["paused"], json!(false));
        assert_eq!(
            state["presenting"],
            json!(true),
            "a fresh wallpaper is presenting until the pause rules say otherwise"
        );
        assert_eq!(state["attached"], json!(false));
        assert_eq!(state["monitors"], json!(0));
        assert_eq!(state["slot"], json!("none"));
        assert_eq!(state["version"], json!(crate::VERSION));
    }

    #[test]
    fn the_placement_records_the_slot_and_the_first_window() {
        let shared = shared();
        let slot = Slot {
            slot: HWND(0x40 as *mut core::ffi::c_void),
            kind: SlotKind::ProgmanChild,
            ..Slot::default()
        };

        shared.record_placement(&slot, HWND(0x99 as *mut core::ffi::c_void), true);
        let state = shared.state();

        assert_eq!(state["parent"], json!("0x40"));
        assert_eq!(state["hwnd"], json!("0x99"));
        assert_eq!(state["slot"], json!("progman-child"));
        assert_eq!(state["attached"], json!(true));
    }

    #[test]
    fn a_sibling_slot_is_named_as_such() {
        let shared = shared();
        let slot = Slot {
            slot: HWND(0x41 as *mut core::ffi::c_void),
            kind: SlotKind::Sibling,
            ..Slot::default()
        };

        shared.record_placement(&slot, HWND(0x99 as *mut core::ffi::c_void), true);

        assert_eq!(shared.state()["slot"], json!("sibling"));
    }

    #[test]
    fn pausing_stops_presenting_and_resuming_restores_it() {
        let shared = shared();

        shared.set_paused(true);
        assert_eq!(shared.state()["paused"], json!(true));
        assert_eq!(shared.state()["presenting"], json!(false));

        shared.set_paused(false);
        assert_eq!(shared.state()["presenting"], json!(true));
    }

    #[test]
    fn the_monitors_are_attached_only_when_every_one_is() {
        let shared = shared();

        shared.set_monitors(2, 1);
        assert_eq!(shared.state()["attached"], json!(false));

        shared.set_monitors(2, 2);
        assert_eq!(shared.state()["attached"], json!(true));

        shared.set_monitors(0, 0);
        assert_eq!(
            shared.state()["attached"],
            json!(false),
            "no desktop is not attached"
        );
    }

    #[test]
    fn frames_accumulate_and_the_colour_is_normalised() {
        let shared = shared();

        shared.add_frames(3);
        shared.add_frames(4);
        shared.set_colour("ABC");

        assert_eq!(shared.state()["frames"], json!(7));
        assert_eq!(shared.state()["color"], json!("#aabbcc"));
    }

    #[test]
    fn an_unknown_method_is_refused_by_name() {
        let response = dispatch(&shared(), request("wallpaper.explode"));

        assert!(!response.ok);
        assert!(
            response
                .error
                .unwrap_or_default()
                .contains("wallpaper.explode"),
            "the refusal must name the method"
        );
    }

    #[test]
    fn ping_and_get_state_answer_with_the_same_shape() {
        let shared = shared();

        let ping = dispatch(&shared, request("wallpaper.ping"));
        let state = dispatch(&shared, request("wallpaper.getState"));

        assert!(ping.ok && state.ok);
        assert_eq!(ping.result, state.result);
        assert_eq!(ping.id, "7");
    }

    #[test]
    fn reload_without_a_loop_is_an_error_rather_than_a_silent_success() {
        let response = dispatch(&shared(), request("wallpaper.reload"));

        assert!(!response.ok, "a dead loop must not look alive");
        assert!(
            response.error.unwrap_or_default().contains("not running"),
            "the refusal must say why"
        );
    }

    #[test]
    fn the_surface_answers_over_a_real_pipe() {
        let shared = shared();
        shared.set_monitors(1, 1);
        let pipe = unique_pipe("dispatch");
        let server = serve(&pipe, {
            let shared = Arc::clone(&shared);
            move |request| dispatch(&shared, request)
        })
        .expect("start the test server");

        let answer = neb_core::ipc::call(
            &pipe,
            "wallpaper.getState",
            None,
            neb_core::ipc::DEFAULT_TIMEOUT,
        )
        .expect("the server must answer");

        assert_eq!(answer["monitors"], json!(1));
        assert_eq!(answer["attached"], json!(true));
        server.shutdown();
    }
}
