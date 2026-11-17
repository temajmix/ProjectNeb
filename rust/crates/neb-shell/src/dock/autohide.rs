//! When the dock shows itself and when it goes away.
//!
//! The dock owns no mouse hook: every [`POLL_MS`] it asks where the pointer is and compares that with
//! the monitor's bottom strip and with its own rectangle. All of the policy lives in
//! [`Autohide::settle`], which is pure: it takes the clock reading, the pointer and two rectangles and
//! answers with one [`Action`], so the timing can be tested without a desktop or a sleeping thread.

use crate::geometry::Rect;

/// Rows at the very bottom of the monitor that reveal a hidden dock.
pub const REVEAL_ROWS: i32 = 2;

/// How long the pointer has to be away before the dock hides.
pub const HIDE_AFTER_MS: u64 = 400;

/// How often the pointer is sampled, in milliseconds.
pub const POLL_MS: u32 = 100;

/// Margin around the dock that still counts as "over it", so brushing the edge does not hide it.
pub const OVER_MARGIN: i32 = 4;

/// What the dock should do with what it just learned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Nothing changed.
    None,
    /// The pointer reached the bottom strip: show the dock.
    Reveal,
    /// The pointer has been away long enough: hide the dock.
    Hide,
}

/// The auto-hide state machine: hidden or shown, and since when the pointer has been away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Autohide {
    hidden: bool,
    away_since: Option<u64>,
}

impl Autohide {
    /// A state that starts hidden (`auto_hide`) or shown.
    pub fn new(auto_hide: bool) -> Self {
        Self {
            hidden: auto_hide,
            away_since: None,
        }
    }

    /// Whether the dock is currently out of the way.
    pub fn is_hidden(&self) -> bool {
        self.hidden
    }

    /// The tick at which the pointer left the dock, for the log line and the tests.
    pub fn away_since(&self) -> Option<u64> {
        self.away_since
    }

    /// Forces the dock on screen, as turning auto-hide off does.
    pub fn show(&mut self) {
        self.hidden = false;
        self.away_since = None;
    }

    /// Forces the dock away, as turning auto-hide on does while the pointer is elsewhere.
    pub fn hide(&mut self) {
        self.hidden = true;
        self.away_since = None;
    }

    /// Applies one pointer sample taken at `now_ms`.
    ///
    /// `monitor` is the full monitor rectangle, because the taskbar is allowed to sit below the dock
    /// and the pointer can still reach the last row of the screen over it.
    pub fn settle(
        &mut self,
        now_ms: u64,
        pointer: (i32, i32),
        monitor: Rect,
        dock: Rect,
    ) -> Action {
        if self.hidden {
            if !in_reveal_strip(pointer, monitor) {
                return Action::None;
            }

            self.hidden = false;
            self.away_since = None;
            return Action::Reveal;
        }

        if is_over_dock(pointer, dock, OVER_MARGIN) {
            self.away_since = None;
            return Action::None;
        }

        let since = *self.away_since.get_or_insert(now_ms);

        if now_ms.saturating_sub(since) >= HIDE_AFTER_MS {
            self.hidden = true;
            self.away_since = None;
            Action::Hide
        } else {
            Action::None
        }
    }
}

/// Whether `pointer` is on the monitor's bottom [`REVEAL_ROWS`] rows, within its horizontal span.
pub fn in_reveal_strip(pointer: (i32, i32), monitor: Rect) -> bool {
    pointer.0 >= monitor.left
        && pointer.0 < monitor.right
        && pointer.1 > monitor.bottom - REVEAL_ROWS
        && pointer.1 <= monitor.bottom
}

/// Whether `pointer` is on the dock, with `margin` pixels of grace on every side.
pub fn is_over_dock(pointer: (i32, i32), dock: Rect, margin: i32) -> bool {
    pointer.0 >= dock.left - margin
        && pointer.0 < dock.right + margin
        && pointer.1 >= dock.top - margin
        && pointer.1 < dock.bottom + margin
}

#[cfg(test)]
mod tests {
    use super::*;

    const MONITOR: Rect = Rect {
        left: 0,
        top: 0,
        right: 1920,
        bottom: 1080,
    };
    const DOCK: Rect = Rect {
        left: 700,
        top: 958,
        right: 1220,
        bottom: 1032,
    };

    #[test]
    fn only_the_last_rows_of_the_monitor_reveal_the_dock() {
        assert!(in_reveal_strip((960, 1080), MONITOR));
        assert!(in_reveal_strip((960, 1079), MONITOR));
        assert!(!in_reveal_strip((960, 1078), MONITOR), "two rows only");
        assert!(!in_reveal_strip((1920, 1080), MONITOR), "outside the width");
        assert!(!in_reveal_strip((-1, 1080), MONITOR));
    }

    #[test]
    fn a_hidden_dock_reveals_when_the_pointer_reaches_the_strip() {
        let mut state = Autohide::new(true);

        assert_eq!(state.settle(1_000, (960, 500), MONITOR, DOCK), Action::None);
        assert!(state.is_hidden());

        assert_eq!(
            state.settle(1_100, (960, 1080), MONITOR, DOCK),
            Action::Reveal
        );
        assert!(!state.is_hidden());
    }

    #[test]
    fn a_shown_dock_hides_only_after_the_grace_period() {
        let mut state = Autohide::new(false);

        assert_eq!(
            state.settle(1_000, (200, 200), MONITOR, DOCK),
            Action::None,
            "the clock starts, nothing happens yet"
        );
        assert_eq!(state.away_since(), Some(1_000));

        assert_eq!(
            state.settle(1_399, (200, 200), MONITOR, DOCK),
            Action::None,
            "399 ms away is still within the grace period"
        );
        assert!(!state.is_hidden());

        assert_eq!(state.settle(1_400, (200, 200), MONITOR, DOCK), Action::Hide);
        assert!(state.is_hidden());
        assert_eq!(state.away_since(), None);
    }

    #[test]
    fn the_pointer_coming_back_resets_the_clock() {
        let mut state = Autohide::new(false);

        assert_eq!(state.settle(1_000, (200, 200), MONITOR, DOCK), Action::None);
        assert_eq!(
            state.settle(1_300, (960, 1000), MONITOR, DOCK),
            Action::None,
            "back over the dock"
        );
        assert_eq!(state.away_since(), None);

        assert_eq!(
            state.settle(1_500, (200, 200), MONITOR, DOCK),
            Action::None,
            "a new absence, timed from the sample that saw it"
        );
        assert_eq!(state.away_since(), Some(1_500));
    }

    #[test]
    fn the_margin_keeps_a_dock_from_flickering_at_its_edge() {
        assert!(is_over_dock((700, 1000), DOCK, OVER_MARGIN));
        assert!(is_over_dock((696, 1000), DOCK, OVER_MARGIN));
        assert!(!is_over_dock((690, 1000), DOCK, OVER_MARGIN));
        assert!(!is_over_dock((960, 500), DOCK, OVER_MARGIN));
    }

    #[test]
    fn forcing_it_show_or_hidden_clears_the_timer() {
        let mut state = Autohide::new(true);
        state.show();
        assert!(!state.is_hidden());

        assert_eq!(state.settle(1_000, (200, 200), MONITOR, DOCK), Action::None);
        state.hide();
        assert!(state.is_hidden());
        assert_eq!(state.away_since(), None);
    }
}
