//! Turning a layout change into a short sequence of positions.
//!
//! `SetWindowPos` has no animation of its own that can be asked for from another process, so the
//! manager steps the windows itself: one `SetWindowPos` per window every [`STEP_MS`] milliseconds
//! along an ease-out curve. The total is capped at [`MAX_DURATION_MS`], because a layout change that
//! takes longer than a blink is worse than an instant snap.
//!
//! The plan is pure data — a list of frames, each one rectangle per moving window — so the tests can
//! count frames and assert the cap without waiting for real time or touching a window.

use crate::rect::Rect;

/// Delay between two frames, in milliseconds.
pub const STEP_MS: u32 = 8;

/// Longest a single layout change may animate.
pub const MAX_DURATION_MS: u32 = 180;

/// Moves smaller than this are applied in one step instead of animated.
pub const SNAP_PX: i32 = 2;

/// One window's travel during a layout change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Move {
    /// Where the window is now.
    pub from: Rect,
    /// Where the layout wants it.
    pub to: Rect,
}

impl Move {
    /// A move from `from` to `to`.
    pub const fn new(from: Rect, to: Rect) -> Self {
        Self { from, to }
    }

    /// Whether this move is big enough to be worth animating.
    pub fn needs_animation(&self) -> bool {
        self.from.distance(&self.to) >= SNAP_PX
    }
}

/// The frames of one animation, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AnimationPlan {
    /// One vector of rectangles per frame, in the same order as the moves.
    pub frames: Vec<Vec<Rect>>,
    /// Time from the first frame to the last, in milliseconds; always `frames.len() * STEP_MS`.
    pub duration_ms: u32,
}

impl AnimationPlan {
    /// How many frames will be written.
    pub fn frame_count(&self) -> usize {
        self.frames.len()
    }

    /// Whether there is nothing to animate.
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The final frame, which is exactly the layout's target.
    pub fn last(&self) -> Option<&[Rect]> {
        self.frames.last().map(Vec::as_slice)
    }
}

/// Whether any of `moves` is big enough to animate.
pub fn should_animate(moves: &[Move]) -> bool {
    moves.iter().any(Move::needs_animation)
}

/// Builds the frames for `moves`, occupying at most [`MAX_DURATION_MS`].
///
/// The last frame is exactly the target rectangle, so the animation can never leave a window a
/// couple of pixels away from where the layout meant it to be. A request shorter than one step is
/// rounded up to one frame; a request longer than the cap is shortened to the cap.
pub fn plan(moves: &[Move], duration_ms: u32) -> AnimationPlan {
    if moves.is_empty() {
        return AnimationPlan::default();
    }

    let duration = duration_ms.clamp(STEP_MS, MAX_DURATION_MS);
    let steps = (duration / STEP_MS).max(1);

    let frames = (1..=steps)
        .map(|step| {
            let progress = f64::from(step) / f64::from(steps);
            let eased = ease_out_cubic(progress);

            moves
                .iter()
                .map(|travel| Rect::lerp(&travel.from, &travel.to, eased))
                .collect()
        })
        .collect();

    AnimationPlan {
        frames,
        duration_ms: steps * STEP_MS,
    }
}

/// The plan for a normal layout change: the full [`MAX_DURATION_MS`].
pub fn plan_layout(moves: &[Move]) -> AnimationPlan {
    plan(moves, MAX_DURATION_MS)
}

/// The standard ease-out used for every window move: fast at the start, gentle at the end.
pub fn ease_out_cubic(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let inverse = 1.0 - t;

    1.0 - inverse * inverse * inverse
}

#[cfg(test)]
mod tests {
    use super::*;

    fn moving() -> Vec<Move> {
        vec![
            Move::new(Rect::new(0, 0, 900, 600), Rect::new(960, 0, 1920, 1080)),
            Move::new(Rect::new(0, 600, 900, 1080), Rect::new(0, 0, 960, 540)),
        ]
    }

    #[test]
    fn a_move_under_two_pixels_is_not_animated() {
        let from = Rect::new(10, 10, 110, 110);
        let tiny = Move::new(from, Rect::new(11, 10, 111, 110));
        let edge = Move::new(from, Rect::new(12, 10, 112, 110));

        assert_eq!(tiny.from.distance(&tiny.to), 1);
        assert!(!tiny.needs_animation());
        assert!(
            edge.needs_animation(),
            "two pixels is the first animated distance"
        );
        assert!(!should_animate(&[tiny]));
        assert!(should_animate(&[tiny, edge]));
        assert!(!should_animate(&[]));
    }

    #[test]
    fn the_plan_is_capped_at_one_hundred_and_eighty_milliseconds() {
        let plan = plan_layout(&moving());

        assert_eq!(plan.duration_ms, 176, "22 whole steps of 8 ms");
        assert_eq!(plan.frame_count(), 22);
        assert_eq!(plan.duration_ms, plan.frame_count() as u32 * STEP_MS);
        assert!(plan.duration_ms <= MAX_DURATION_MS);
    }

    #[test]
    fn a_longer_request_is_shortened_and_a_shorter_one_is_rounded_up() {
        assert_eq!(plan(&moving(), 10_000).duration_ms, 176);
        assert_eq!(plan(&moving(), 1).duration_ms, STEP_MS);
        assert_eq!(plan(&moving(), 0).frame_count(), 1);
        assert_eq!(plan(&moving(), 60).frame_count(), 7);
    }

    #[test]
    fn the_last_frame_is_exactly_the_target() {
        let moves = moving();
        let plan = plan_layout(&moves);
        let last = plan.last().expect("frames exist");

        assert_eq!(last.len(), moves.len());
        for (index, target) in last.iter().enumerate() {
            assert_eq!(
                *target, moves[index].to,
                "window {index} must land on its tile"
            );
        }
    }

    #[test]
    fn every_frame_stays_between_the_old_and_the_new_tile() {
        let moves = moving();

        for frame in plan_layout(&moves).frames {
            for (index, rect) in frame.iter().enumerate() {
                let from = moves[index].from;
                let to = moves[index].to;

                assert!(rect.left >= from.left.min(to.left) && rect.left <= from.left.max(to.left));
                assert!(rect.right >= from.right.min(to.right));
                assert!(rect.right <= from.right.max(to.right));
                assert!(rect.top >= from.top.min(to.top) && rect.top <= from.top.max(to.top));
                assert!(rect.bottom >= from.bottom.min(to.bottom));
                assert!(rect.bottom <= from.bottom.max(to.bottom));
            }
        }
    }

    #[test]
    fn the_curve_is_monotonic_and_hits_both_ends() {
        assert!((ease_out_cubic(0.0) - 0.0).abs() < f64::EPSILON);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < f64::EPSILON);

        let mut previous = 0.0;
        for step in 0..=100 {
            let value = ease_out_cubic(f64::from(step) / 100.0);
            assert!(value >= previous, "the curve must never go backwards");
            previous = value;
        }

        // Fast at the start: the first tenth of the time covers much more than a tenth of the way.
        assert!(ease_out_cubic(0.1) > 0.25);
    }

    #[test]
    fn an_empty_list_produces_no_frames() {
        let plan = plan_layout(&[]);

        assert!(plan.is_empty());
        assert_eq!(plan.duration_ms, 0);
        assert_eq!(plan.last(), None);
    }

    #[test]
    fn the_first_frame_has_already_moved_forward() {
        let moves = moving();
        let plan = plan_layout(&moves);
        let first = &plan.frames[0];

        assert_ne!(
            first[0], moves[0].from,
            "a frame at the start would stall the eye"
        );
        assert_ne!(
            first[0], moves[0].to,
            "and a frame at the end would skip the motion"
        );
    }
}
