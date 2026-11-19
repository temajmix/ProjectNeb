//! The on-disk record of what the Windows replacement mode changed.
//!
//! `windows-mode.json` sits next to `config.json`. Every entry names one registry value, the raw
//! value it had before the mode hid it (`null`/`{"type":"absent"}` when the value did not exist) and
//! when that happened. It is written *before* the registry is touched, so a supervisor that starts
//! after a crash - or after the user turned the mode off - can always put the machine back.
//!
//! A file that cannot be decoded is never guessed at: it reads as "nothing is recorded" and the
//! caller logs the reason. Guessing previous values is what would leave a user with a modified
//! machine and no way to reverse it.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use super::{ModeError, RawValue, Toggle, ToggleSet};

/// One value the mode changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Record {
    /// Whether the hidden value is currently in the registry.
    pub applied: bool,
    /// The raw value from before the first change, or [`RawValue::Absent`].
    pub previous: RawValue,
    /// RFC 3339 stamp of the change, for the log and for support.
    pub saved_at: String,
}

impl Default for Record {
    fn default() -> Self {
        Self {
            applied: false,
            previous: RawValue::Absent,
            saved_at: String::new(),
        }
    }
}

/// Everything `windows-mode.json` carries.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct State {
    /// One entry per toggle the mode touched, keyed by the toggle id.
    pub toggles: BTreeMap<Toggle, Record>,
}

impl State {
    /// Whether `toggle` has a recorded, still-applied change.
    pub fn is_applied(&self, toggle: Toggle) -> bool {
        self.toggles
            .get(&toggle)
            .is_some_and(|record| record.applied)
    }

    /// The toggles that are recorded as applied.
    pub fn applied(&self) -> ToggleSet {
        let mut set = ToggleSet::default();

        for toggle in Toggle::ALL {
            if self.is_applied(toggle) {
                set.set(toggle, true);
            }
        }

        set
    }

    /// The recorded value from before the change, when there is one.
    pub fn previous(&self, toggle: Toggle) -> Option<&RawValue> {
        self.toggles
            .get(&toggle)
            .filter(|record| record.applied)
            .map(|record| &record.previous)
    }

    /// How many applied changes the file holds.
    pub fn applied_count(&self) -> usize {
        Toggle::ALL
            .into_iter()
            .filter(|toggle| self.is_applied(*toggle))
            .count()
    }

    /// Whether there is nothing left to restore.
    pub fn is_empty(&self) -> bool {
        self.toggles.is_empty()
    }
}

/// Reads the state file.
///
/// A missing file is the ordinary "nothing was ever changed"; anything that cannot be decoded reads
/// as empty *and* returns a warning, because the caller has to say so out loud.
pub fn load(path: &Path) -> (State, Option<String>) {
    match fs::read(path) {
        Ok(bytes) => {
            let text = String::from_utf8_lossy(&bytes);
            let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
            let parsed = serde_json::from_str::<serde_json::Value>(text)
                .map_err(|error| error.to_string())
                .and_then(|value| {
                    if value.is_object() {
                        serde_json::from_value::<State>(value).map_err(|error| error.to_string())
                    } else {
                        Err("the file must hold a json object".to_owned())
                    }
                });

            match parsed {
                Ok(state) => (state, None),
                Err(reason) => (
                    State::default(),
                    Some(format!(
                        "'{}' is not valid windows-mode state ({reason}); nothing is recorded as hidden",
                        path.display()
                    )),
                ),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (State::default(), None),
        Err(error) => (
            State::default(),
            Some(format!(
                "cannot read '{}' ({error}); nothing is recorded as hidden",
                path.display()
            )),
        ),
    }
}

/// Writes the state file atomically, or removes it when there is nothing left to remember.
///
/// The bytes go to `windows-mode.json.tmp-<pid>` first and only then replace the real file, so a
/// power cut leaves either the old state or the new one, never half of either.
pub fn save(path: &Path, state: &State) -> Result<(), ModeError> {
    if state.is_empty() {
        return match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ModeError::StateFile {
                action: "remove",
                path: path.to_path_buf(),
                source: error,
            }),
        };
    }

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|source| ModeError::StateFile {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let bytes = serde_json::to_vec_pretty(state).map_err(|error| ModeError::StateJson {
        path: path.to_path_buf(),
        reason: error.to_string(),
    })?;
    let temporary = path.with_extension(format!("json.tmp-{}", std::process::id()));

    fs::write(&temporary, &bytes).map_err(|source| ModeError::StateFile {
        action: "write",
        path: temporary.clone(),
        source,
    })?;
    fs::rename(&temporary, path).map_err(|source| ModeError::StateFile {
        action: "replace",
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(previous: RawValue) -> Record {
        Record {
            applied: true,
            previous,
            saved_at: "2026-11-18T12:00:00.000Z".to_owned(),
        }
    }

    #[test]
    fn a_state_file_round_trips_without_losing_a_value() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("windows-mode.json");
        let mut state = State::default();
        state
            .toggles
            .insert(Toggle::DesktopIcons, record(RawValue::Dword { value: 0 }));
        state
            .toggles
            .insert(Toggle::SearchBox, record(RawValue::Dword { value: 2 }));
        state
            .toggles
            .insert(Toggle::Taskbar, record(RawValue::Absent));

        save(&path, &state).expect("save");
        let (loaded, warning) = load(&path);

        assert_eq!(warning, None);
        assert_eq!(loaded, state);
        assert_eq!(loaded.applied_count(), 3);
        assert_eq!(
            loaded.previous(Toggle::SearchBox),
            Some(&RawValue::Dword { value: 2 })
        );
        assert_eq!(loaded.previous(Toggle::Taskbar), Some(&RawValue::Absent));
    }

    #[test]
    fn absent_values_are_written_as_a_tag_not_as_null() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("windows-mode.json");
        let mut state = State::default();
        state
            .toggles
            .insert(Toggle::Widgets, record(RawValue::Absent));

        save(&path, &state).expect("save");
        let text = fs::read_to_string(&path).expect("read");

        assert!(text.contains("\"type\": \"absent\""), "{text}");
        assert!(text.contains("\"savedAt\""), "{text}");
        assert!(text.contains("\"widgets\""), "{text}");
    }

    #[test]
    fn saving_an_empty_state_removes_the_file() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("windows-mode.json");
        let mut state = State::default();
        state
            .toggles
            .insert(Toggle::News, record(RawValue::Dword { value: 0 }));

        save(&path, &state).expect("save");
        assert!(path.exists());

        save(&path, &State::default()).expect("clear");

        assert!(
            !path.exists(),
            "a fully restored mode leaves no file behind"
        );
    }

    #[test]
    fn a_missing_file_is_an_empty_state_without_a_warning() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("nothing-here.json");

        let (state, warning) = load(&path);

        assert_eq!(state, State::default());
        assert_eq!(warning, None);
    }

    #[test]
    fn a_corrupted_file_reads_as_empty_and_says_so() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("windows-mode.json");

        for text in ["not json", "[]", r#"{"toggles":{"telemetry":{}}}"#] {
            fs::write(&path, text).expect("write");
            let (state, warning) = load(&path);

            assert!(state.is_empty(), "'{text}' must not invent an entry");
            assert!(
                warning
                    .as_ref()
                    .is_some_and(|warning| warning.contains("windows-mode state")),
                "'{text}' must be reported: {warning:?}"
            );
        }
    }

    #[test]
    fn the_file_is_tolerated_with_a_bom_and_a_partial_record() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join("windows-mode.json");
        fs::write(
            &path,
            "\u{feff}{\"toggles\":{\"copilot\":{\"previous\":{\"type\":\"dword\",\"value\":1}}}}",
        )
        .expect("write");

        let (state, warning) = load(&path);

        assert_eq!(warning, None);
        let entry = state.toggles.get(&Toggle::Copilot).expect("an entry");
        assert_eq!(entry.previous, RawValue::Dword { value: 1 });
        assert!(!entry.applied, "a missing applied flag means not applied");
        assert_eq!(state.applied_count(), 0);
    }
}
