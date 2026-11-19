//! Drives the dock's auto-hide through a real window and a pointer sample.
//!
//! The state machine is pure and tested in `dock::autohide`; what this test adds is the wiring: a real
//! `WS_EX_LAYERED` window, the real tick and the real `ShowWindow`. The pointer is never automated —
//! `probe_pointer` feeds one sample into the same tick a timer would, which is the only thing the two
//! paths do not share.
//!
//! Set `NEB_SKIP_DESKTOP_TESTS=1` to skip it on a machine without an interactive session.

use std::path::PathBuf;
use std::time::Duration;

use neb_core::strings::Language;
use neb_shell::dock::DockItem;
use neb_shell::dock::window::{DockSpec, create_dock, destroy_dock, is_hidden, probe_pointer};
use neb_shell::geometry;
use neb_shell::theme::Theme;

/// Whether this desktop is running a fullscreen app right now, which would hide the dock by itself
/// and make these assertions meaningless. The probe is the product's own, so the guard cannot drift.
fn a_fullscreen_app_is_in_front() -> bool {
    neb_shell::dock::fullscreen::foreground_is_fullscreen()
}

#[test]
fn a_hidden_dock_reveals_at_the_bottom_and_hides_again_when_the_pointer_leaves() {
    if std::env::var_os("NEB_SKIP_DESKTOP_TESTS").is_some() {
        eprintln!("skipped: NEB_SKIP_DESKTOP_TESTS is set");
        return;
    }

    if a_fullscreen_app_is_in_front() {
        eprintln!("skipped: a fullscreen application owns the screen");
        return;
    }

    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");

    let hwnd = create_dock(DockSpec {
        monitor: monitor.clone(),
        theme: Theme::baked_in(),
        language: Language::En,
        items: vec![DockItem {
            name: "notepad".to_owned(),
            path: Some(PathBuf::from(r"C:\Windows\System32\notepad.exe")),
            target: None,
            pinned: true,
            running: false,
        }],
        auto_hide: true,
        tile_dip: neb_shell::dock::layout::TILE_DIP,
        animations: Default::default(),
        poll_ms: neb_shell::dock::autohide::POLL_MS,
        host: neb_shell::dock::window::windows(),
    })
    .expect("the dock must be created");

    assert!(is_hidden(hwnd), "an auto-hiding dock starts out of the way");

    // The monitor's last row, which is where the reveal strip is.
    let bottom = (
        monitor.rect.left + monitor.rect.width() / 2,
        monitor.rect.bottom - 1,
    );
    probe_pointer(hwnd, bottom.0, bottom.1);
    assert!(!is_hidden(hwnd), "a pointer at the bottom edge reveals it");

    // Away from the dock, and long enough for the 400 ms grace period to expire.
    probe_pointer(hwnd, monitor.rect.left + 10, monitor.rect.top + 10);
    assert!(!is_hidden(hwnd), "the grace period has not expired yet");

    std::thread::sleep(Duration::from_millis(450));
    probe_pointer(hwnd, monitor.rect.left + 10, monitor.rect.top + 10);
    assert!(is_hidden(hwnd), "400 ms away hides it again");

    destroy_dock(hwnd);
}

#[test]
fn an_auto_hiding_dock_that_is_not_auto_hiding_stays_put() {
    if std::env::var_os("NEB_SKIP_DESKTOP_TESTS").is_some() {
        eprintln!("skipped: NEB_SKIP_DESKTOP_TESTS is set");
        return;
    }

    if a_fullscreen_app_is_in_front() {
        eprintln!("skipped: a fullscreen application owns the screen");
        return;
    }

    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");

    let hwnd = create_dock(DockSpec {
        monitor,
        theme: Theme::baked_in(),
        language: Language::En,
        items: vec![DockItem {
            name: "notepad".to_owned(),
            path: Some(PathBuf::from(r"C:\Windows\System32\notepad.exe")),
            target: None,
            pinned: true,
            running: false,
        }],
        auto_hide: false,
        tile_dip: neb_shell::dock::layout::TILE_DIP,
        animations: Default::default(),
        poll_ms: neb_shell::dock::autohide::POLL_MS,
        host: neb_shell::dock::window::windows(),
    })
    .expect("the dock must be created");

    assert!(
        !is_hidden(hwnd),
        "a dock without auto-hide is always on screen"
    );

    probe_pointer(hwnd, 10, 10);
    assert!(!is_hidden(hwnd), "and a pointer sample cannot move it");

    destroy_dock(hwnd);
}
