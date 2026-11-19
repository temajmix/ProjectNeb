//! Proves that every path that destroys a bar hands its reserved strip back, and that a DPI change
//! leaves the renderer sized to the strip.
//!
//! The bar registers itself with the shell's AppBar API, which shrinks the monitor's work area. If a
//! destroy path forgets `ABM_REMOVE`, the desktop stays forty pixels short until the process exits —
//! visible only as a work area that never grows back, and easy to miss in review.
//!
//! The seam is [`EdgeHost`]: production talks to `SHAppBarMessage`, and these tests hand the bar a
//! counter instead, so a real window is created and destroyed while the real work area is untouched.
//! The test host still moves the window when it answers a reservation, because a message that move
//! sends is exactly where the DPI sizing lives.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use neb_core::strings::Language;
use neb_shell::appbar::{APPBARDATA, Call, Edge, EdgeHost};
use neb_shell::bar::WidgetSpec;
use neb_shell::geometry::{self, Rect};
use neb_shell::theme::Theme;
use neb_shell::window::{self, BarSpec};
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    HWND_NOTOPMOST, HWND_TOPMOST, SWP_NOACTIVATE, SendMessageW, SetWindowPos, WM_DPICHANGED,
};

/// Whether this test may touch the desktop at all.
///
/// Every test here creates a real window, which a build machine without an interactive session cannot
/// do; `NEB_SKIP_DESKTOP_TESTS=1` turns them into a reported skip instead of a failure.
fn desktop() -> bool {
    if std::env::var_os("NEB_SKIP_DESKTOP_TESTS").is_some() {
        eprintln!("skipped: NEB_SKIP_DESKTOP_TESTS is set");
        return false;
    }

    true
}

/// A host that counts what the shell would have been asked to do, and moves the window the way the
/// shell would move it.
#[derive(Debug, Default)]
struct CountingHost {
    new: AtomicUsize,
    remove: AtomicUsize,
    set_position: AtomicUsize,
    places: Mutex<Vec<Rect>>,
}

impl CountingHost {
    fn registrations(&self) -> usize {
        self.new.load(Ordering::SeqCst)
    }

    fn releases(&self) -> usize {
        self.remove.load(Ordering::SeqCst)
    }

    fn positions(&self) -> usize {
        self.set_position.load(Ordering::SeqCst)
    }

    fn places(&self) -> Vec<Rect> {
        self.places.lock().expect("lock").clone()
    }
}

impl EdgeHost for CountingHost {
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

        // A shell that agrees with everything, which is the interesting case for counting.
        1
    }

    fn place(&self, hwnd: HWND, rect: Rect, topmost: bool) {
        self.places.lock().expect("lock").push(rect);

        // The real host moves the window here, and the `WM_SIZE` that produces is swallowed by the
        // bar's re-entrancy guard. Doing the same in the test is what makes the DPI case honest.
        // SAFETY: `hwnd` is a window this process created on this thread.
        unsafe {
            let _ = SetWindowPos(
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
            );
        }
    }
}

/// The fresh-install widget list, parsed the way the shell parses the configuration.
fn default_widgets() -> Vec<WidgetSpec> {
    neb_shell::bar::parse(&neb_core::config::default_widgets()).0
}

/// One bar on the first monitor of this machine, talking to `host`.
fn create(host: &Arc<CountingHost>) -> HWND {
    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");
    let host: Arc<dyn EdgeHost> = host.clone();

    window::create_bar(BarSpec {
        monitor,
        edge: Edge::Top,
        height_dip: 40,
        theme: Theme::baked_in(),
        language: Language::En,
        profile: Some("lifecycle".to_owned()),
        widgets: default_widgets(),
        animations: neb_core::config::AnimationSettings::default(),
        host,
    })
    .expect("the bar window must be created")
}

#[test]
fn destroying_a_bar_releases_the_edge() {
    if !desktop() {
        return;
    }

    let host = Arc::new(CountingHost::default());
    let hwnd = create(&host);

    assert_eq!(host.registrations(), 1, "creating a bar registers it");
    assert_eq!(host.positions(), 1, "and places it once");
    assert_eq!(host.releases(), 0, "and has not released anything yet");

    window::destroy_bar(hwnd);

    assert_eq!(
        host.releases(),
        1,
        "destroying a bar must hand the strip back; a missing ABM_REMOVE leaves the desktop short"
    );
}

#[test]
fn a_rebuild_registers_and_releases_once_per_bar() {
    if !desktop() {
        return;
    }

    let host = Arc::new(CountingHost::default());

    // What `shell.reload` and `WM_DISPLAYCHANGE` do: destroy every bar, then create the new set.
    let first = create(&host);
    window::destroy_bar(first);
    let second = create(&host);
    window::destroy_bar(second);

    assert_eq!(host.registrations(), 2, "each bar registers itself");
    assert_eq!(
        host.releases(),
        2,
        "each destroy releases exactly the registration it made"
    );
    assert_eq!(host.positions(), 2);
}

#[test]
fn hiding_a_bar_releases_the_edge_and_showing_it_takes_it_back() {
    if !desktop() {
        return;
    }

    let host = Arc::new(CountingHost::default());
    let hwnd = create(&host);

    window::set_edge(hwnd, false);
    assert_eq!(host.releases(), 1, "a hidden bar must not hold the strip");

    window::set_edge(hwnd, true);
    assert_eq!(host.registrations(), 2, "showing it registers again");

    window::destroy_bar(hwnd);
    assert_eq!(
        host.releases(),
        2,
        "and the final destroy releases the second one"
    );
    assert_eq!(
        host.registrations(),
        host.releases(),
        "every registration a bar ever made must be released again"
    );
}

#[test]
fn the_release_happens_after_the_window_is_gone_too() {
    if !desktop() {
        return;
    }

    // `DestroyWindow` sends `WM_DESTROY` and then `WM_NCDESTROY`; the second one drops the state. The
    // release must have happened by the time the call returns, whichever of the two did it.
    let host = Arc::new(CountingHost::default());
    let hwnd = create(&host);

    window::destroy_bar(hwnd);

    assert!(
        !window::is_visible(hwnd),
        "the window must be gone, so the release was not skipped on the way out"
    );
    assert_eq!(host.releases(), 1);
    assert!(
        !host.places().is_empty(),
        "the bar must have been placed at least once while it existed"
    );
}

#[test]
fn a_dpi_change_sizes_the_renderer_to_the_strip_not_to_the_suggestion() {
    if !desktop() {
        return;
    }

    let host = Arc::new(CountingHost::default());
    let hwnd = create(&host);

    assert_eq!(
        window::renderer_size(hwnd),
        Some((1920, 40)),
        "a 40 DIP bar at 96 DPI draws into 40 physical pixels"
    );

    // Windows suggests a rectangle that is deliberately *not* the strip: 80 pixels tall, where the
    // new scale needs 60. A renderer measured against the suggestion keeps 80 and the clock drifts.
    let suggested = RECT {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 80,
    };

    // SAFETY: `hwnd` belongs to this thread, so the send runs the procedure synchronously here, and
    // the structure it points at outlives the call.
    unsafe {
        SendMessageW(
            hwnd,
            WM_DPICHANGED,
            Some(WPARAM(144 | (144 << 16))),
            Some(LPARAM((&raw const suggested) as isize)),
        );
    }

    assert_eq!(
        window::renderer_size(hwnd),
        Some((1920, 60)),
        "the target must follow the strip the reservation produced, not the suggested rectangle"
    );

    window::destroy_bar(hwnd);
}
