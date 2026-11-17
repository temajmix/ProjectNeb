//! Proves that a filtered log line allocates nothing.
//!
//! The resident processes must not allocate in hot paths, and "the log is disabled" is exactly when a
//! call site is most likely to sit in one. The test file carries its own counting allocator because the
//! property is about allocations, not about output.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use neb_core::logging::{Level, init_with};
use neb_core::time::Clock;

/// Counts every allocation the process makes.
struct CountingAllocator;

static ALLOCATIONS: AtomicUsize = AtomicUsize::new(0);

// SAFETY: both methods forward to the system allocator with the layout they were given, and only add
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

/// A clock that never moves: the test is about allocation, not about the calendar.
#[derive(Debug)]
struct FixedClock;

impl Clock for FixedClock {
    fn now(&self) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(1_700_000_000)
    }
}

#[test]
fn a_filtered_line_does_not_allocate() {
    let dir = tempfile::tempdir().expect("temp dir");
    let log = init_with(dir.path(), Level::Info, Arc::new(FixedClock)).expect("init logging");

    // Warm up, so the measurement covers only the filtered call.
    log.info("alloc-test", "warm up");

    let before = ALLOCATIONS.load(Ordering::Relaxed);
    log.debug("alloc-test", "filtered out");
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert_eq!(
        after,
        before,
        "a filtered debug line must not allocate: {} allocations",
        after - before
    );

    // Control: a line that is written does allocate its text, so the counter really measures.
    log.set_level(Level::Debug);
    let before = ALLOCATIONS.load(Ordering::Relaxed);
    log.debug("alloc-test", "now it writes");
    let after = ALLOCATIONS.load(Ordering::Relaxed);
    assert!(
        after > before,
        "a written line must allocate, otherwise this test proves nothing"
    );
}
