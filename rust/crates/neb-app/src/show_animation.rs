//! The first-show slide, as arithmetic with no Win32 in it.
//!
//! The window is shown twelve device-independent pixels below its final position and slides up
//! over [`BASE_DURATION_MS`]. The plan is a pure function of the elapsed time, not a frame counter:
//! `WM_TIMER` is allowed to arrive late, and the last frame must land exactly on the stored
//! rectangle even when the message loop was busy for a whole frame.
//!
//! `config.json` owns whether this runs at all and how fast: [`AnimationSettings::scaled_ms`]
//! already returns zero when animations are disabled, which is the single code path that makes
//! "instant" indistinguishable from "no animation".

use neb_core::config::AnimationSettings;

/// Duration of the first-show slide before the user's speed multiplier, in milliseconds.
pub const BASE_DURATION_MS: u32 = 160;

/// How far below its final position the window starts, in device-independent pixels.
pub const SLIDE_DISTANCE_DIP: f64 = 12.0;

/// How often the window procedure is asked to move the window, in milliseconds.
///
/// Windows timers are not high-resolution; 15 ms is the classic `USER_TIMER_MINIMUM` and keeps the
/// slide smooth without asking for a timer resolution change the process would have to undo.
pub const FRAME_INTERVAL_MS: u32 = 15;

/// One frame of the plan: when it happens and how far below the final rectangle it sits.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Frame {
    /// Milliseconds from the start of the slide.
    pub at_ms: u32,
    /// Pixels below the final top edge; the last frame is `0.0`.
    pub offset_px: f64,
}

/// The slide duration after the user's `animations.speed`, or `0` when animations are disabled.
pub fn duration_ms(animations: &AnimationSettings) -> u32 {
    animations.scaled_ms(BASE_DURATION_MS)
}

/// The starting offset in physical pixels for a monitor at `scale` (1.0 at 100 %).
pub fn distance_px(scale: f64) -> f64 {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };

    SLIDE_DISTANCE_DIP * scale
}

/// Ease-out cubic: fast at first, gently settling on the stored rectangle.
pub fn ease_out_cubic(progress: f64) -> f64 {
    let progress = progress.clamp(0.0, 1.0);
    1.0 - (1.0 - progress).powi(3)
}

/// Pixels below the final top edge after `elapsed_ms`.
///
/// A zero duration, a non-positive distance or a broken value is `0.0`, so the caller can always
/// move the window to `final_top + offset` without a special case.
pub fn offset_px(duration_ms: u32, elapsed_ms: u32, distance_px: f64) -> f64 {
    if duration_ms == 0 || !distance_px.is_finite() || distance_px <= 0.0 {
        return 0.0;
    }

    let progress = f64::from(elapsed_ms.min(duration_ms)) / f64::from(duration_ms);
    distance_px * (1.0 - ease_out_cubic(progress))
}

/// The frames a caller would schedule for a duration, with the exact final frame appended.
///
/// The window procedure does not iterate this list (it samples the clock instead), but the plan is
/// what the tests and the log line describe: a 160 ms slide is eleven 15 ms frames plus the landing.
pub fn frames(duration_ms: u32, distance_px: f64) -> Vec<Frame> {
    if duration_ms == 0 || !distance_px.is_finite() || distance_px <= 0.0 {
        return Vec::new();
    }

    let mut frames = Vec::new();
    let mut at_ms = 0;

    while at_ms < duration_ms {
        frames.push(Frame {
            at_ms,
            offset_px: offset_px(duration_ms, at_ms, distance_px),
        });
        at_ms = at_ms.saturating_add(FRAME_INTERVAL_MS);
    }

    frames.push(Frame {
        at_ms: duration_ms,
        offset_px: 0.0,
    });

    frames
}

#[cfg(test)]
mod tests {
    use super::*;

    fn animations(enabled: bool, speed: f64) -> AnimationSettings {
        AnimationSettings { enabled, speed }
    }

    #[test]
    fn the_default_plan_is_a_160_ms_slide_of_12_dip() {
        let animations = animations(true, 1.0);

        assert_eq!(duration_ms(&animations), 160);
        assert_eq!(distance_px(1.0), 12.0);
        assert_eq!(distance_px(1.25), 15.0);
        assert_eq!(distance_px(1.5), 18.0);
    }

    #[test]
    fn a_disabled_animation_has_no_frames_and_never_moves_the_window() {
        let animations = animations(false, 1.0);

        assert_eq!(duration_ms(&animations), 0);
        assert!(frames(0, 12.0).is_empty());
        assert_eq!(offset_px(0, 0, 12.0), 0.0);
        assert_eq!(offset_px(0, 160, 12.0), 0.0);
    }

    #[test]
    fn the_offset_starts_at_the_distance_and_lands_exactly_on_zero() {
        assert_eq!(offset_px(160, 0, 12.0), 12.0);
        assert_eq!(offset_px(160, 160, 12.0), 0.0);
        assert_eq!(offset_px(160, 10_000, 12.0), 0.0);
        assert_eq!(offset_px(160, 80, 12.0), 1.5);
    }

    #[test]
    fn every_frame_is_closer_to_the_final_position_than_the_one_before() {
        let plan = frames(160, 12.0);

        assert!(plan.len() >= 10, "a 160 ms slide must have real frames");
        assert_eq!(plan.first().map(|frame| frame.offset_px), Some(12.0));
        assert_eq!(plan.last().map(|frame| frame.offset_px), Some(0.0));
        assert_eq!(plan.last().map(|frame| frame.at_ms), Some(160));

        for pair in plan.windows(2) {
            assert!(
                pair[1].offset_px <= pair[0].offset_px,
                "{:?} must not move back down after {:?}",
                pair[1],
                pair[0]
            );
            assert!(pair[1].at_ms > pair[0].at_ms);
        }
    }

    #[test]
    fn the_speed_multiplier_changes_the_duration_and_the_frame_count() {
        assert_eq!(duration_ms(&animations(true, 2.0)), 80);
        assert_eq!(duration_ms(&animations(true, 0.5)), 320);
        assert!(frames(80, 12.0).len() < frames(320, 12.0).len());
    }

    #[test]
    fn a_broken_scale_or_distance_falls_back_to_a_safe_plan() {
        assert_eq!(distance_px(f64::NAN), 12.0);
        assert_eq!(distance_px(0.0), 12.0);
        assert_eq!(offset_px(160, 80, f64::NAN), 0.0);
        assert_eq!(offset_px(160, 80, -1.0), 0.0);
    }

    #[test]
    fn easing_is_monotonic_and_pinned_at_both_ends() {
        assert_eq!(ease_out_cubic(0.0), 0.0);
        assert_eq!(ease_out_cubic(1.0), 1.0);
        assert_eq!(ease_out_cubic(-4.0), 0.0);
        assert_eq!(ease_out_cubic(9.0), 1.0);

        let mut previous = 0.0;
        for step in 0..=20 {
            let value = ease_out_cubic(f64::from(step) / 20.0);
            assert!(value >= previous);
            previous = value;
        }
    }
}
