//! Motion: easing curves, one-shot transitions, and the rule that idle means no frames.
//!
//! Every animation in the shell is the same object: a [`Transition`] that starts at one value, ends at
//! another, and answers "what is the value now" from a millisecond clock. Nothing here owns a timer;
//! a window arms a 16 ms timer while [`Transition::is_done`] is false and kills it the moment the
//! transition finishes, which is what keeps an idle bar at zero frames.
//!
//! The curves are pure functions of `0.0..=1.0`, so the tests pin their exact values rather than
//! approximating them. Durations are scaled by [`neb_core::config::AnimationSettings`]: a disabled
//! animation has a zero duration and therefore lands on its target value on the first read.

use neb_core::config::AnimationSettings;
use windows::Win32::System::SystemInformation::GetTickCount64;

/// How often a window paints while a transition is running, in milliseconds.
pub const FRAME_MS: u32 = 16;

/// The bar sliding into place.
pub const BAR_SLIDE_MS: u32 = 220;

/// The dock sliding up on reveal and down on hide.
pub const DOCK_SLIDE_MS: u32 = 180;

/// A launcher, settings or dock tile fading its hover highlight in or out.
pub const HOVER_MS: u32 = 120;

/// The launcher panel fading and growing into place.
pub const LAUNCHER_MS: u32 = 160;

/// The scale the launcher starts at; it grows to `1.0`.
pub const LAUNCHER_SCALE_FROM: f32 = 0.96;

/// A cubic ease-out: fast at the start, settled at the end. `f(0) = 0`, `f(1) = 1`.
pub fn ease_out_cubic(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let complement = 1.0 - t;
    1.0 - complement * complement * complement
}

/// A linear ease, for a value that should move at a constant rate.
pub fn linear(t: f32) -> f32 {
    t.clamp(0.0, 1.0)
}

/// The monotonic millisecond clock the transitions read.
pub fn now_ms() -> u64 {
    // SAFETY: the call takes no arguments and returns a monotonically increasing count.
    unsafe { GetTickCount64() }
}

/// One value moving from `from` to `to` over a fixed duration.
///
/// A zero duration is the "instant" case: the value is already `to` at the start, and
/// [`is_done`](Self::is_done) is true from the first read, so no timer is ever armed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    from: f32,
    to: f32,
    start_ms: u64,
    duration_ms: u32,
}

impl Transition {
    /// A transition that starts at `now_ms` and lands after `duration_ms`.
    pub fn new(now_ms: u64, from: f32, to: f32, duration_ms: u32) -> Self {
        Self {
            from,
            to,
            start_ms: now_ms,
            duration_ms,
        }
    }

    /// A value that is already settled, with no timer behind it.
    pub fn settled(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start_ms: 0,
            duration_ms: 0,
        }
    }

    /// A transition whose duration comes from the user's animation settings.
    pub fn scaled(
        now_ms: u64,
        from: f32,
        to: f32,
        base_ms: u32,
        animations: &AnimationSettings,
    ) -> Self {
        Self::new(now_ms, from, to, animations.scaled_ms(base_ms))
    }

    /// The value at `now_ms`.
    pub fn value(&self, now_ms: u64) -> f32 {
        if self.duration_ms == 0 {
            return self.to;
        }

        let elapsed = now_ms.saturating_sub(self.start_ms);
        let progress = (elapsed as f32 / self.duration_ms as f32).clamp(0.0, 1.0);
        let eased = ease_out_cubic(progress);

        self.from + (self.to - self.from) * eased
    }

    /// Whether the transition has reached its target, i.e. whether the timer may stop.
    pub fn is_done(&self, now_ms: u64) -> bool {
        self.duration_ms == 0 || now_ms.saturating_sub(self.start_ms) >= u64::from(self.duration_ms)
    }

    /// The value this transition is heading for.
    pub fn target(&self) -> f32 {
        self.to
    }

    /// The value it started from.
    pub fn origin(&self) -> f32 {
        self.from
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neb_core::config::AnimationSettings;

    #[test]
    fn the_ease_out_curve_has_the_briefs_values() {
        assert!((ease_out_cubic(0.0) - 0.0).abs() < f32::EPSILON);
        assert!((ease_out_cubic(0.5) - 0.875).abs() < 1e-6);
        assert!((ease_out_cubic(1.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_curve_is_clamped_outside_the_unit_interval() {
        assert!((ease_out_cubic(-2.0) - 0.0).abs() < f32::EPSILON);
        assert!((ease_out_cubic(3.0) - 1.0).abs() < f32::EPSILON);
        assert!((linear(-1.0) - 0.0).abs() < f32::EPSILON);
        assert!((linear(2.0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_transition_moves_between_its_ends() {
        let transition = Transition::new(1_000, 0.0, 1.0, 220);

        assert!((transition.value(1_000) - 0.0).abs() < f32::EPSILON);
        assert!((transition.value(1_110) - 0.875).abs() < 1e-6);
        assert!((transition.value(1_220) - 1.0).abs() < f32::EPSILON);
        assert!(
            (transition.value(9_000) - 1.0).abs() < f32::EPSILON,
            "past the end"
        );
    }

    #[test]
    fn a_transition_reports_when_the_timer_may_stop() {
        let transition = Transition::new(0, 0.0, 1.0, 120);

        assert!(!transition.is_done(119));
        assert!(transition.is_done(120));
        assert!(transition.is_done(121));
        assert!(Transition::settled(1.0).is_done(0));
    }

    #[test]
    fn the_speed_scales_the_duration_in_both_directions() {
        let fast = AnimationSettings {
            enabled: true,
            speed: 2.0,
        };
        let slow = AnimationSettings {
            enabled: true,
            speed: 0.5,
        };

        let fast_slide = Transition::scaled(0, 0.0, 1.0, BAR_SLIDE_MS, &fast);
        let slow_slide = Transition::scaled(0, 0.0, 1.0, BAR_SLIDE_MS, &slow);

        assert!(fast_slide.is_done(110), "220 ms at 2x is 110 ms");
        assert!(!slow_slide.is_done(220), "220 ms at 0.5x is 440 ms");
        assert!(slow_slide.is_done(440));
    }

    #[test]
    fn a_disabled_animation_is_instant() {
        let disabled = AnimationSettings {
            enabled: false,
            speed: 1.0,
        };
        let transition = Transition::scaled(500, 0.0, 1.0, LAUNCHER_MS, &disabled);

        assert!((transition.value(500) - 1.0).abs() < f32::EPSILON);
        assert!(
            transition.is_done(500),
            "an instant transition arms no timer"
        );
        assert!((transition.value(0) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_launcher_starts_at_ninety_six_percent() {
        let transition = Transition::new(0, LAUNCHER_SCALE_FROM, 1.0, LAUNCHER_MS);

        assert!((LAUNCHER_SCALE_FROM - 0.96).abs() < f32::EPSILON);
        assert!((transition.value(0) - 0.96).abs() < 1e-6);
        assert!((transition.value(LAUNCHER_MS as u64) - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_durations_are_the_briefs() {
        assert_eq!(FRAME_MS, 16);
        assert_eq!(BAR_SLIDE_MS, 220);
        assert_eq!(DOCK_SLIDE_MS, 180);
        assert_eq!(HOVER_MS, 120);
        assert_eq!(LAUNCHER_MS, 160);
    }
}
