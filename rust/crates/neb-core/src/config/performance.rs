//! Performance profiles: one switch that paces every ProjectNeb surface.
//!
//! The file keeps the user's choice (`performance.profile`) and, optionally, a handful of per-key
//! overrides. [`PerformanceSettings::resolved`] turns both into [`Performance`], the values the
//! shell, the wallpaper and the supervisor actually apply. Resolution is total and clamped: a
//! hand-edited or older file can never make a process ask for a zero frame rate or a check every
//! second of the day.

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::model::{AnimationSettings, MAX_ANIMATION_SPEED, MIN_ANIMATION_SPEED};

/// Profile a file that does not name one runs at.
pub const DEFAULT_PROFILE: PerformanceProfile = PerformanceProfile::Balanced;

/// Slowest wallpaper the override may ask for, in frames per second.
pub const MIN_WALLPAPER_FPS: u32 = 1;

/// Fastest wallpaper the override may ask for, in frames per second.
pub const MAX_WALLPAPER_FPS: u32 = 120;

/// Shortest metrics interval the override may ask for, in milliseconds.
pub const MIN_METRICS_MS: u64 = 100;

/// Longest metrics interval the override may ask for, in milliseconds.
pub const MAX_METRICS_MS: u64 = 60_000;

/// Shortest update-check interval the override may ask for, in hours.
pub const MIN_UPDATE_CHECK_HOURS: u64 = 1;

/// Longest update-check interval the override may ask for, in hours.
pub const MAX_UPDATE_CHECK_HOURS: u64 = 168;

/// Animation speed every profile currently uses; the `animations.speed` key still multiplies it.
pub const PROFILE_ANIMATION_SPEED: f64 = 1.0;

/// One of the three shipped performance profiles.
///
/// Serialised as the lower-case name the dashboard writes. Deserialisation is deliberately
/// forgiving: an unknown name loads as [`DEFAULT_PROFILE`] instead of rejecting the whole file,
/// which is the same rule every other string enum in the config follows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PerformanceProfile {
    /// Fewest frames, no motion, cheap polling.
    Performance,
    /// The shipped compromise.
    #[default]
    Balanced,
    /// Smooth motion and the fastest polls.
    Quality,
}

impl PerformanceProfile {
    /// Every profile, in the order the settings window lists them.
    pub const ALL: [Self; 3] = [Self::Performance, Self::Balanced, Self::Quality];

    /// The name written to `config.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Performance => "performance",
            Self::Balanced => "balanced",
            Self::Quality => "quality",
        }
    }

    /// Reads a name, tolerating case and surrounding whitespace; `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();

        Self::ALL
            .into_iter()
            .find(|profile| profile.as_str().eq_ignore_ascii_case(text))
    }

    /// Wallpaper frames per second.
    pub fn wallpaper_fps(self) -> u32 {
        match self {
            Self::Performance => 10,
            Self::Balanced => 30,
            Self::Quality => 60,
        }
    }

    /// Whether shell motion runs at all.
    pub fn animations_enabled(self) -> bool {
        !matches!(self, Self::Performance)
    }

    /// Speed multiplier applied on top of `animations.speed`.
    pub fn animation_speed(self) -> f64 {
        PROFILE_ANIMATION_SPEED
    }

    /// Interval between two metrics samples, in milliseconds.
    pub fn metrics_ms(self) -> u64 {
        match self {
            Self::Performance => 2_000,
            Self::Balanced => 1_000,
            Self::Quality => 500,
        }
    }

    /// Hours between two automatic update checks.
    pub fn update_check_hours(self) -> u64 {
        match self {
            Self::Performance => 24,
            Self::Balanced | Self::Quality => 6,
        }
    }

    /// How often the dock samples the pointer, in milliseconds.
    pub fn dock_poll_ms(self) -> u64 {
        match self {
            Self::Performance => 250,
            Self::Balanced => 100,
            Self::Quality => 60,
        }
    }
}

impl<'de> Deserialize<'de> for PerformanceProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // Anything that is not a string (including `null` and a number) is the balanced profile,
        // because a whole configuration file must not become unreadable over one misspelled enum.
        let value = Value::deserialize(deserializer)?;
        let text = value.as_str().unwrap_or_default();

        Ok(Self::parse(text).unwrap_or(DEFAULT_PROFILE))
    }
}

/// The `performance` object as it is persisted, with the optional per-key overrides.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct PerformanceSettings {
    /// The profile the user picked.
    pub profile: PerformanceProfile,
    /// Overrides the profile's wallpaper frame rate.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wallpaper_fps: Option<u32>,
    /// Overrides the profile's motion switch. The `animations.enabled` key can still turn motion
    /// off; this key cannot force it on against that switch.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub animations: Option<bool>,
    /// Overrides the profile's update-check interval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_check_hours: Option<u64>,
    /// Overrides the profile's metrics interval.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metrics_ms: Option<u64>,
}

impl PerformanceSettings {
    /// Resolves the profile and every override into the values a process applies.
    ///
    /// Every numeric value is clamped into its documented range here rather than at each consumer,
    /// so the shell, wallpaper and supervisor cannot disagree about a hand-edited file.
    pub fn resolved(&self) -> Performance {
        let profile = self.profile;

        Performance {
            profile,
            wallpaper_fps: self
                .wallpaper_fps
                .unwrap_or_else(|| profile.wallpaper_fps())
                .clamp(MIN_WALLPAPER_FPS, MAX_WALLPAPER_FPS),
            animations: self
                .animations
                .unwrap_or_else(|| profile.animations_enabled()),
            animation_speed: profile.animation_speed(),
            metrics_ms: self
                .metrics_ms
                .unwrap_or_else(|| profile.metrics_ms())
                .clamp(MIN_METRICS_MS, MAX_METRICS_MS),
            update_check_hours: self
                .update_check_hours
                .unwrap_or_else(|| profile.update_check_hours())
                .clamp(MIN_UPDATE_CHECK_HOURS, MAX_UPDATE_CHECK_HOURS),
            dock_poll_ms: profile.dock_poll_ms(),
        }
    }
}

/// The values a process applies after [`PerformanceSettings::resolved`].
///
/// This is what `performance.get` answers and what the bridge reports after a change; it contains
/// no optional fields, so a consumer never has to re-invent a default.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Performance {
    /// The profile the values came from.
    pub profile: PerformanceProfile,
    /// Wallpaper frames per second.
    pub wallpaper_fps: u32,
    /// Whether shell motion runs.
    pub animations: bool,
    /// Multiplier applied on top of `animations.speed`.
    pub animation_speed: f64,
    /// Interval between two metrics samples, in milliseconds.
    pub metrics_ms: u64,
    /// Hours between two automatic update checks.
    pub update_check_hours: u64,
    /// How often the dock samples the pointer, in milliseconds.
    pub dock_poll_ms: u64,
}

impl Performance {
    /// The shell motion settings for this profile, combined with the user's `animations` section.
    ///
    /// The profile decides whether motion is on; the `animations` section still owns the speed
    /// multiplier and can switch motion off, so neither key silently overrides the other.
    pub fn animations_with(&self, base: AnimationSettings) -> AnimationSettings {
        AnimationSettings {
            enabled: self.animations && base.enabled,
            speed: (base.speed * self.animation_speed)
                .clamp(MIN_ANIMATION_SPEED, MAX_ANIMATION_SPEED),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use serde_json::json;

    #[test]
    fn every_profile_has_the_documented_values() {
        let expected = [
            (PerformanceProfile::Performance, 10, false, 2_000, 24, 250),
            (PerformanceProfile::Balanced, 30, true, 1_000, 6, 100),
            (PerformanceProfile::Quality, 60, true, 500, 6, 60),
        ];

        for (profile, fps, animations, metrics, hours, dock) in expected {
            let resolved = PerformanceSettings {
                profile,
                ..PerformanceSettings::default()
            }
            .resolved();

            assert_eq!(resolved.profile, profile);
            assert_eq!(resolved.wallpaper_fps, fps, "{}", profile.as_str());
            assert_eq!(resolved.animations, animations, "{}", profile.as_str());
            assert_eq!(resolved.metrics_ms, metrics, "{}", profile.as_str());
            assert_eq!(resolved.update_check_hours, hours, "{}", profile.as_str());
            assert_eq!(resolved.dock_poll_ms, dock, "{}", profile.as_str());
            assert!((resolved.animation_speed - 1.0).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn the_default_profile_is_balanced() {
        assert_eq!(PerformanceSettings::default().profile, DEFAULT_PROFILE);
        assert_eq!(
            PerformanceSettings::default().resolved(),
            PerformanceSettings {
                profile: PerformanceProfile::Balanced,
                ..PerformanceSettings::default()
            }
            .resolved()
        );
        assert_eq!(PerformanceProfile::default(), PerformanceProfile::Balanced);
    }

    #[test]
    fn an_unknown_profile_name_loads_as_balanced() {
        for text in ["turbo", "", "  ", "QUALITY?"] {
            let settings: PerformanceSettings =
                serde_json::from_value(json!({ "profile": text })).expect("parse");

            assert_eq!(
                settings.profile,
                PerformanceProfile::Balanced,
                "'{text}' must fall back"
            );
        }

        // A wrong JSON type is the same kind of mistake and must not make the file unreadable.
        for wrong in [Value::Null, json!(7), json!(true), json!([])] {
            let settings: PerformanceSettings =
                serde_json::from_value(json!({ "profile": wrong })).expect("parse");

            assert_eq!(settings.profile, PerformanceProfile::Balanced, "{wrong}");
        }
    }

    #[test]
    fn profile_names_are_case_insensitive_and_round_trip_lower_case() {
        assert_eq!(
            PerformanceProfile::parse(" Quality "),
            Some(PerformanceProfile::Quality)
        );
        assert_eq!(
            PerformanceProfile::parse("PERFORMANCE"),
            Some(PerformanceProfile::Performance)
        );
        assert_eq!(
            PerformanceProfile::parse("balanced"),
            Some(PerformanceProfile::Balanced)
        );
        assert_eq!(PerformanceProfile::parse("turbo"), None);

        let settings = PerformanceSettings {
            profile: PerformanceProfile::Quality,
            ..PerformanceSettings::default()
        };
        let text = serde_json::to_string(&settings).expect("render");

        assert!(text.contains(r#""profile":"quality""#), "{text}");
    }

    #[test]
    fn overrides_replace_the_profile_and_are_clamped() {
        let settings = PerformanceSettings {
            profile: PerformanceProfile::Quality,
            wallpaper_fps: Some(0),
            animations: Some(false),
            update_check_hours: Some(9_999),
            metrics_ms: Some(0),
        };

        let resolved = settings.resolved();
        assert_eq!(
            resolved.wallpaper_fps, MIN_WALLPAPER_FPS,
            "zero frames would freeze the wallpaper"
        );
        assert!(!resolved.animations, "an explicit switch wins");
        assert_eq!(resolved.update_check_hours, MAX_UPDATE_CHECK_HOURS);
        assert_eq!(resolved.metrics_ms, MIN_METRICS_MS);

        let high = PerformanceSettings {
            wallpaper_fps: Some(10_000),
            metrics_ms: Some(u64::MAX),
            ..PerformanceSettings::default()
        }
        .resolved();
        assert_eq!(high.wallpaper_fps, MAX_WALLPAPER_FPS);
        assert_eq!(high.metrics_ms, MAX_METRICS_MS);
    }

    #[test]
    fn an_override_of_the_other_direction_is_accepted_inside_the_range() {
        let resolved = PerformanceSettings {
            profile: PerformanceProfile::Performance,
            wallpaper_fps: Some(45),
            animations: Some(true),
            update_check_hours: Some(12),
            metrics_ms: Some(750),
        }
        .resolved();

        assert_eq!(resolved.wallpaper_fps, 45);
        assert!(resolved.animations);
        assert_eq!(resolved.update_check_hours, 12);
        assert_eq!(resolved.metrics_ms, 750);
        assert_eq!(
            resolved.dock_poll_ms, 250,
            "the dock poll follows the profile, not an override"
        );
    }

    #[test]
    fn unset_overrides_are_omitted_from_the_file() {
        let settings = PerformanceSettings {
            profile: PerformanceProfile::Performance,
            ..PerformanceSettings::default()
        };
        let value = serde_json::to_value(&settings).expect("render");

        assert_eq!(value, json!({ "profile": "performance" }));

        let with_one = PerformanceSettings {
            wallpaper_fps: Some(12),
            ..settings
        };
        let value = serde_json::to_value(with_one).expect("render");
        assert_eq!(
            value,
            json!({ "profile": "performance", "wallpaperFps": 12 })
        );
    }

    #[test]
    fn a_config_file_without_the_section_gets_balanced() {
        let config: Config = serde_json::from_str(r#"{"schemaVersion":1}"#).expect("parse");

        assert_eq!(config.performance, PerformanceSettings::default());
        assert_eq!(
            config.performance.resolved().profile,
            PerformanceProfile::Balanced
        );
    }

    #[test]
    fn the_section_round_trips_with_its_overrides() {
        let config = Config {
            performance: PerformanceSettings {
                profile: PerformanceProfile::Performance,
                wallpaper_fps: Some(15),
                animations: Some(true),
                update_check_hours: Some(48),
                metrics_ms: Some(1_500),
            },
            ..Config::default()
        };

        let text = serde_json::to_string(&config).expect("render");
        assert!(
            text.contains(r#""performance":{"profile":"performance""#),
            "{text}"
        );

        let back: Config = serde_json::from_str(&text).expect("parse");
        assert_eq!(back, config);
        assert_eq!(back.performance.resolved().wallpaper_fps, 15);
    }

    #[test]
    fn the_profiles_combine_with_the_animation_section() {
        let base = AnimationSettings {
            enabled: true,
            speed: 1.0,
        };
        let none = PerformanceSettings {
            profile: PerformanceProfile::Performance,
            ..PerformanceSettings::default()
        }
        .resolved()
        .animations_with(base);
        assert!(!none.enabled);
        assert_eq!(none.scaled_ms(220), 0);

        let quality_profile = PerformanceSettings {
            profile: PerformanceProfile::Quality,
            ..PerformanceSettings::default()
        }
        .resolved();
        let quality = quality_profile.animations_with(base);
        assert!(quality.enabled);
        assert_eq!(quality.scaled_ms(220), 220);

        // The user's own switch still turns motion off, and the speed multiplier still applies.
        let off = quality_profile.animations_with(AnimationSettings {
            enabled: false,
            speed: 2.0,
        });
        assert!(!off.enabled);
        assert!((off.speed() - 2.0).abs() < f64::EPSILON);
    }
}
