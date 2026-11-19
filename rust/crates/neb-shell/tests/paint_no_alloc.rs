//! Proves that a painted frame allocates nothing.
//!
//! The bar's frame is the hottest path in the process and the one a memory budget is measured
//! against, so "no allocation in paint" is a test, not a claim. The harness carries its own counting
//! allocator (the same trick `neb-core/tests/log_no_alloc.rs` uses) and drives a hundred real frames
//! through `WM_PAINT` on a real Direct2D target.
//!
//! Only compiled with `--features alloc-check`:
//!
//! ```text
//! cargo test -p neb-shell --features alloc-check --test paint_no_alloc
//! ```
//!
//! Creating a bar reserves the monitor's top edge for the length of the test, so the run is visible
//! on screen for a fraction of a second.

#![cfg(feature = "alloc-check")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use neb_core::strings::Language;
use neb_shell::appbar::Edge;
use neb_shell::geometry;
use neb_shell::theme::Theme;
use neb_shell::window::{self, BarSpec};
use windows::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, MSG, PM_REMOVE, PeekMessageW, TranslateMessage,
};

/// Counts every allocation the process makes.
struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

/// The counter is process-wide, so the tests that read it must not run beside each other: the control
/// test allocates on purpose, and doing that inside the other test's window would be reported as a
/// frame that allocates. The lock is taken before the warm-up, so it is never held across a
/// measurement it could disturb.
static SERIAL: Mutex<()> = Mutex::new(());

// SAFETY: both methods forward to the system allocator with the layout they were given and only add
// an atomic increment, so the allocator contract is unchanged.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
        // SAFETY: the same layout is forwarded unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the pointer came from `System.alloc` with the same layout.
        unsafe { System.dealloc(pointer, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// The fresh-install widget list, parsed the way the shell parses the configuration.
fn widgets() -> Vec<neb_shell::bar::WidgetSpec> {
    neb_shell::bar::parse(&neb_core::config::default_widgets()).0
}

/// A bar on the first monitor, which is all a paint test needs.
fn create_test_bar() -> windows::Win32::Foundation::HWND {
    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");

    window::create_bar(BarSpec {
        monitor,
        edge: Edge::Top,
        height_dip: 40,
        theme: Theme::baked_in(),
        language: Language::En,
        profile: Some("alloc-check".to_owned()),
        widgets: widgets(),
        animations: neb_core::config::AnimationSettings::default(),
        host: neb_shell::appbar::windows(),
    })
    .expect("the bar must be created")
}

#[test]
fn a_hundred_frames_allocate_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|lock| lock.into_inner());
    let hwnd = create_test_bar();

    // The first frames are allowed to allocate: the render target, the brushes and the first text
    // layouts are created then, which is exactly the work a frame must not repeat.
    for _ in 0..5 {
        window::repaint_now(hwnd);
    }

    let painted_before = window::painted_frames(hwnd);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..100 {
        window::repaint_now(hwnd);
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    let painted_after = window::painted_frames(hwnd);

    window::destroy_bar(hwnd);

    assert!(
        painted_after - painted_before >= 100,
        "the frames must actually reach Direct2D: {} of 100 painted",
        painted_after - painted_before
    );
    assert_eq!(
        after - before,
        0,
        "painting 100 frames allocated {} times",
        after - before
    );
}

/// The minute of the wall clock, used to tell a clock repaint from an idle one.
fn local_minute() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() / 60)
}

/// Dispatches every message this thread has for `milliseconds`, sleeping between passes.
fn pump_for(milliseconds: u64) {
    let deadline = Instant::now() + Duration::from_millis(milliseconds);

    while Instant::now() < deadline {
        let mut message = MSG::default();

        // SAFETY: `message` is a live out-parameter and the filters ask for every message on this
        // thread, which is exactly what the real loop does.
        while unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) }.as_bool() {
            // SAFETY: the message was filled in by `PeekMessageW` above.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Waits until the bar's slide and hover fades are finished, or `milliseconds` elapse.
fn settle(hwnd: windows::Win32::Foundation::HWND, milliseconds: u64) {
    let deadline = Instant::now() + Duration::from_millis(milliseconds);

    while Instant::now() < deadline {
        pump_for(20);
        if !window::is_animating(hwnd) {
            return;
        }
    }
}

#[test]
fn an_idle_second_paints_no_frames() {
    let _serial = SERIAL.lock().unwrap_or_else(|lock| lock.into_inner());
    let hwnd = create_static_test_bar();

    // The slide-in arms a 16 ms timer; once it is done, nothing should ask for another frame. One
    // more pump lets the last animation tick run the window's final placement before the baseline.
    settle(hwnd, 2_000);
    pump_for(100);
    assert!(
        !window::is_animating(hwnd),
        "the slide must have finished before the idle check"
    );

    let before = window::painted_frames(hwnd);
    let minute = local_minute();
    pump_for(1_100);
    let after = window::painted_frames(hwnd);

    destroy_bar_after_idle(hwnd, minute, before, after);
}

/// The assertion half, split out so the bar is destroyed even when the check fails.
fn destroy_bar_after_idle(
    hwnd: windows::Win32::Foundation::HWND,
    minute: u64,
    before: u64,
    after: u64,
) {
    let same_minute = local_minute() == minute;
    window::destroy_bar(hwnd);

    if same_minute {
        assert_eq!(
            after,
            before,
            "an idle second drew {} frame(s); nothing changed, so nothing may paint",
            after - before
        );
    }
}

/// A bar whose content never changes: one launcher glyph, no clock and no metrics. Only the
/// slide-in runs, so any frame after it settles is a frame nothing asked for.
fn create_static_test_bar() -> windows::Win32::Foundation::HWND {
    use neb_core::config::BarWidget;

    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");
    let entries = [BarWidget {
        kind: "launcher".to_owned(),
        align: "left".to_owned(),
        ..BarWidget::default()
    }];

    window::create_bar(BarSpec {
        monitor,
        edge: Edge::Top,
        height_dip: 40,
        theme: Theme::baked_in(),
        language: Language::En,
        profile: Some("idle-check".to_owned()),
        widgets: neb_shell::bar::parse(&entries).0,
        animations: neb_core::config::AnimationSettings::default(),
        host: neb_shell::appbar::windows(),
    })
    .expect("the static bar must be created")
}

#[test]
fn the_counter_notices_an_allocation_the_test_makes() {
    let _serial = SERIAL.lock().unwrap_or_else(|lock| lock.into_inner());
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    let deliberate: Vec<u8> = Vec::with_capacity(4_096);
    let after = ALLOCATIONS.load(Ordering::Relaxed);

    assert!(
        after > before,
        "the harness must be able to see an allocation, otherwise the other test proves nothing"
    );

    drop(deliberate);
}

/// A dock on the first monitor with three tiles, two of them "running".
fn create_test_dock() -> windows::Win32::Foundation::HWND {
    use std::path::PathBuf;

    use neb_shell::dock::DockItem;
    use neb_shell::dock::window::{DockSpec, create_dock};

    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");

    let items = vec![
        DockItem {
            name: "notepad".to_owned(),
            path: Some(PathBuf::from(r"C:\Windows\System32\notepad.exe")),
            target: Some(0x1000),
            pinned: true,
            running: true,
        },
        DockItem {
            name: "paint".to_owned(),
            path: Some(PathBuf::from(r"C:\Windows\System32\mspaint.exe")),
            target: None,
            pinned: true,
            running: false,
        },
        DockItem {
            name: "nowhere".to_owned(),
            path: None,
            target: Some(0x2000),
            pinned: false,
            running: true,
        },
    ];

    create_dock(DockSpec {
        monitor,
        theme: Theme::baked_in(),
        language: Language::En,
        items,
        auto_hide: false,
        tile_dip: neb_shell::dock::layout::TILE_DIP,
        animations: Default::default(),
        host: neb_shell::dock::window::windows(),
    })
    .expect("the dock must be created")
}

#[test]
fn a_hundred_dock_frames_allocate_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|lock| lock.into_inner());
    let hwnd = create_test_dock();

    // The first frames are allowed to allocate: the DIB, the brushes, the icon bitmaps and the
    // placeholder letters are created then, which is exactly the work a frame must not repeat.
    for _ in 0..5 {
        neb_shell::dock::window::repaint_now(hwnd);
    }

    let painted_before = neb_shell::dock::window::painted_frames(hwnd);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..100 {
        neb_shell::dock::window::repaint_now(hwnd);
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    let painted_after = neb_shell::dock::window::painted_frames(hwnd);

    let plan = neb_shell::dock::window::plan_of(hwnd).expect("the dock has a plan");
    let size = neb_shell::dock::window::client_size(hwnd).expect("the dock has a client size");
    neb_shell::dock::window::destroy_dock(hwnd);

    assert!(
        painted_after - painted_before >= 100,
        "the frames must actually reach Direct2D: {} of 100 painted",
        painted_after - painted_before
    );
    assert_eq!(
        after - before,
        0,
        "painting 100 dock frames allocated {} times",
        after - before
    );
    assert_eq!(
        size,
        (plan.width, plan.height),
        "the window must be exactly as wide as the content it draws"
    );
}

/// A launcher panel with enough results to fill every visible row.
fn create_test_launcher() -> windows::Win32::Foundation::HWND {
    use std::sync::Arc;

    use neb_shell::launcher::Entry;
    use neb_shell::launcher::window::{LauncherSpec, create_launcher};

    let monitor = geometry::monitors()
        .into_iter()
        .next()
        .expect("a desktop session has at least one monitor");

    let entries: Vec<Entry> = [
        "Calculator",
        "Paint",
        "Notepad",
        "Terminal",
        "Command Prompt",
        "7-Zip File Manager",
        "Character Map",
        "Registry Editor",
        "Snipping Tool",
    ]
    .iter()
    .map(|name| Entry {
        name: (*name).to_owned(),
        path: std::path::PathBuf::from(r"C:\Windows\System32\notepad.exe"),
    })
    .collect();

    create_launcher(LauncherSpec {
        monitor,
        theme: Theme::baked_in(),
        language: Language::En,
        entries: Arc::new(entries),
        building: false,
        animations: Default::default(),
    })
    .expect("the launcher must be created")
}

#[test]
fn a_hundred_launcher_frames_allocate_nothing() {
    let _serial = SERIAL.lock().unwrap_or_else(|lock| lock.into_inner());
    let hwnd = create_test_launcher();

    // The first frames are allowed to allocate: the target, the brushes, the visible rows' layouts
    // and their icons are created then, which is exactly the work a frame must not repeat.
    for _ in 0..5 {
        neb_shell::launcher::window::repaint_now(hwnd);
    }

    let painted_before = neb_shell::launcher::window::painted_frames(hwnd);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    for _ in 0..100 {
        neb_shell::launcher::window::repaint_now(hwnd);
    }
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    let painted_after = neb_shell::launcher::window::painted_frames(hwnd);

    let results = neb_shell::launcher::window::result_count(hwnd);
    let size = neb_shell::launcher::window::client_size(hwnd).expect("the panel has a client size");
    neb_shell::launcher::window::destroy_launcher(hwnd);

    assert!(
        painted_after - painted_before >= 100,
        "the frames must actually reach Direct2D: {} of 100 painted",
        painted_after - painted_before
    );
    assert_eq!(
        after - before,
        0,
        "painting 100 launcher frames allocated {} times",
        after - before
    );
    assert_eq!(results, 9, "the panel shows every result it was given");
    assert_eq!(size, (720, 480), "the panel is 720x480 at 100 % scale");
}
