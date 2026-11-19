//! Turns a validated [`Preset`] into the `activity` object of `SET_ACTIVITY`.
//!
//! Empty fields are omitted rather than sent as `""`: Discord renders a present-but-empty line as a
//! gap, and an absent one is what callers expect when they clear a field in the settings window.
//! The `assets` object only appears when at least one of its four values is set.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

use crate::presets::Preset;

/// Seconds since the Unix epoch, saturating rather than panicking on a clock before 1970.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

/// The `activity` document for `preset`, with `now` used when the preset has no start timestamp.
pub fn build(preset: &Preset, now: u64) -> Value {
    let mut activity = serde_json::Map::new();

    if !preset.details.is_empty() {
        activity.insert("details".to_owned(), json!(preset.details));
    }
    if !preset.state.is_empty() {
        activity.insert("state".to_owned(), json!(preset.state));
    }

    let mut assets = serde_json::Map::new();
    if !preset.large_image.is_empty() {
        assets.insert("large_image".to_owned(), json!(preset.large_image));
    }
    if !preset.large_text.is_empty() {
        assets.insert("large_text".to_owned(), json!(preset.large_text));
    }
    if !preset.small_image.is_empty() {
        assets.insert("small_image".to_owned(), json!(preset.small_image));
    }
    if !preset.small_text.is_empty() {
        assets.insert("small_text".to_owned(), json!(preset.small_text));
    }
    if !assets.is_empty() {
        activity.insert("assets".to_owned(), Value::Object(assets));
    }

    if !preset.buttons.is_empty() {
        let buttons: Vec<Value> = preset
            .buttons
            .iter()
            .map(|button| json!({ "label": button.label, "url": button.url }))
            .collect();
        activity.insert("buttons".to_owned(), Value::Array(buttons));
    }

    activity.insert(
        "timestamps".to_owned(),
        json!({ "start": preset.start_timestamp.unwrap_or(now) }),
    );
    activity.insert("instance".to_owned(), json!(true));

    Value::Object(activity)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::{Button, Preset};

    fn preset() -> Preset {
        Preset {
            name: "Gaming".to_owned(),
            details: "Playing".to_owned(),
            state: "In the shell".to_owned(),
            large_image: "neb".to_owned(),
            large_text: "ProjectNeb".to_owned(),
            small_image: String::new(),
            small_text: String::new(),
            buttons: vec![Button {
                label: "Project".to_owned(),
                url: "https://example.com".to_owned(),
            }],
            start_timestamp: Some(1_700_000_000),
        }
    }

    #[test]
    fn a_complete_preset_renders_every_discord_field() {
        let activity = build(&preset(), 1_800_000_000);

        assert_eq!(activity["details"], json!("Playing"));
        assert_eq!(activity["state"], json!("In the shell"));
        assert_eq!(activity["assets"]["large_image"], json!("neb"));
        assert_eq!(activity["assets"]["large_text"], json!("ProjectNeb"));
        assert_eq!(activity["buttons"][0]["label"], json!("Project"));
        assert_eq!(activity["buttons"][0]["url"], json!("https://example.com"));
        assert_eq!(activity["timestamps"]["start"], json!(1_700_000_000_u64));
        assert_eq!(activity["instance"], json!(true));
    }

    #[test]
    fn empty_fields_are_omitted_instead_of_rendered_as_blanks() {
        let activity = build(&Preset::named("Bare"), 42);

        assert!(activity.get("details").is_none());
        assert!(activity.get("state").is_none());
        assert!(activity.get("assets").is_none());
        assert!(activity.get("buttons").is_none());
        assert_eq!(
            activity["timestamps"]["start"],
            json!(42),
            "a preset without a start shows elapsed time from the moment it is applied"
        );
    }

    #[test]
    fn assets_appear_when_only_one_of_them_is_set() {
        let mut preset = Preset::named("One asset");
        preset.small_text = "Ready".to_owned();

        let activity = build(&preset, 1);

        assert_eq!(activity["assets"]["small_text"], json!("Ready"));
        assert!(activity["assets"].get("large_image").is_none());
    }

    #[test]
    fn a_clock_before_the_epoch_saturates_to_zero() {
        assert!(now_unix() > 1_700_000_000);
    }
}
