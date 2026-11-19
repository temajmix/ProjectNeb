//! Asserts that the dock asks to be *topmost*, which is the defect beta testing found: a dock placed
//! in the ordinary band disappears under a maximised window.
//!
//! The placement calls go through [`DockPlacement`], so the test records what the dock asked for
//! instead of inspecting the desktop. A real window is still created — the point is what the window
//! asks for, and that has to be asked of something.
//!
//! Set `NEB_SKIP_DESKTOP_TESTS=1` to skip it on a machine without an interactive session.

use std::path::PathBuf;
use std::sync::Mutex;

use neb_core::strings::Language;
use neb_shell::dock::DockItem;
use neb_shell::dock::window::{
    DockPlacement, DockSpec, create_dock, destroy_dock, plan_of, windows,
};
use neb_shell::geometry::{self, Rect};
use neb_shell::theme::Theme;
use windows::Win32::Foundation::HWND;
use windows::Win32::UI::WindowsAndMessaging::{GWL_EXSTYLE, GetWindowLongPtrW, WS_EX_TOPMOST};

/// What the dock asked the desktop to do.
#[derive(Debug, Default)]
struct Recorder {
    places: Mutex<Vec<(Rect, bool)>>,
    raises: Mutex<usize>,
}

impl DockPlacement for Recorder {
    fn place(&self, _hwnd: HWND, rect: Rect, topmost: bool) {
        self.places
            .lock()
            .unwrap_or_else(|lock| lock.into_inner())
            .push((rect, topmost));
    }

    fn raise(&self, _hwnd: HWND) {
        *self.raises.lock().unwrap_or_else(|lock| lock.into_inner()) += 1;
    }
}

fn items(names: &[&str]) -> Vec<DockItem> {
    names
        .iter()
        .map(|name| DockItem {
            name: (*name).to_owned(),
            path: Some(PathBuf::from(r"C:\Windows\System32\notepad.exe")),
            target: None,
            pinned: true,
            running: false,
        })
        .collect()
}

#[test]
fn the_dock_asks_for_the_topmost_band_when_it_is_placed() {
    if std::env::var_os("NEB_SKIP_DESKTOP_TESTS").is_some() {
        eprintln!("skipped: NEB_SKIP_DESKTOP_TESTS is set");
        return;
    }

    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");
    let recorder = std::sync::Arc::new(Recorder::default());

    let hwnd = create_dock(DockSpec {
        monitor,
        theme: Theme::baked_in(),
        language: Language::En,
        items: items(&["notepad"]),
        auto_hide: false,
        tile_dip: neb_shell::dock::layout::TILE_DIP,
        animations: Default::default(),
        poll_ms: neb_shell::dock::autohide::POLL_MS,
        host: std::sync::Arc::clone(&recorder) as std::sync::Arc<dyn DockPlacement>,
    })
    .expect("the dock must be created");

    let places = recorder
        .places
        .lock()
        .unwrap_or_else(|lock| lock.into_inner())
        .clone();

    assert!(
        !places.is_empty(),
        "the dock must place itself at least once"
    );
    assert!(
        places.iter().all(|(_, topmost)| *topmost),
        "every placement must ask for the topmost band: {places:?}"
    );

    let plan = plan_of(hwnd).expect("the dock has a plan");
    assert_eq!(
        places[0].0.width(),
        plan.width,
        "the placement must be the strip the plan describes"
    );
    let raises = *recorder
        .raises
        .lock()
        .unwrap_or_else(|lock| lock.into_inner());
    assert!(
        raises >= 1,
        "showing the dock must re-assert the band, not only the position"
    );

    // The window itself carries the style as well, so a shell that reorders windows cannot drop it
    // silently between two placements.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_EXSTYLE) } as u32;
    assert!(
        style & WS_EX_TOPMOST.0 != 0,
        "the window must be created with WS_EX_TOPMOST, got {style:#x}"
    );

    // A relayout has to ask again: this is the path a monitor change and a refresh take.
    neb_shell::dock::window::set_items(hwnd, items(&["notepad", "charmap"]));

    let places = recorder
        .places
        .lock()
        .unwrap_or_else(|lock| lock.into_inner())
        .clone();
    assert!(
        places.len() >= 2,
        "a relayout must place the window again: {places:?}"
    );
    assert!(
        places.iter().all(|(_, topmost)| *topmost),
        "a relayout must keep asking for the topmost band: {places:?}"
    );

    destroy_dock(hwnd);
}

#[test]
fn the_production_host_is_what_the_shell_uses() {
    // A cheap guard against the seam being left wired to a recorder in production code.
    let host = windows();
    let debug = format!("{host:?}");

    assert!(debug.contains("WindowsPlacement"), "{debug}");
}
