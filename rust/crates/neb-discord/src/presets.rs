//! Discord Rich Presence presets and their on-disk store.
//!
//! A preset is a small, user-editable activity document. Presets live in
//! `%LocalAppData%\ProjectNeb\discord-presets\*.json` (or under `NEB_HOME`), one file per preset, so
//! a user can copy one in without the application running and a broken file costs one preset rather
//! than the whole list.
//!
//! Validation is deliberately strict at the boundary: Discord rejects a whole activity if one string
//! is too long, and a `http://` button is refused by the client. Catching both here means the error
//! names the preset field instead of surfacing as silence from Discord.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Longest text field Discord accepts in an activity, in UTF-8 bytes.
pub const MAX_TEXT_BYTES: usize = 128;

/// Most buttons one activity may carry.
pub const MAX_BUTTONS: usize = 2;

/// One button under an activity.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Button {
    /// Text on the button.
    pub label: String,
    /// Destination; must be `https://`.
    pub url: String,
}

/// One named activity the user can apply.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Preset {
    /// Name used by `discord.setPreset` and by rules.
    pub name: String,
    /// First activity line.
    pub details: String,
    /// Second activity line.
    pub state: String,
    /// Large image key or URL.
    pub large_image: String,
    /// Tooltip of the large image.
    pub large_text: String,
    /// Small image key or URL.
    pub small_image: String,
    /// Tooltip of the small image.
    pub small_text: String,
    /// Up to two buttons.
    pub buttons: Vec<Button>,
    /// Unix seconds shown as the elapsed-time origin; `None` uses the moment the preset is applied.
    pub start_timestamp: Option<u64>,
}

impl Preset {
    /// A preset with only a name; the fields a user then fills in.
    pub fn named(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Self::default()
        }
    }

    /// Checks every field Discord would reject.
    ///
    /// # Errors
    ///
    /// [`PresetError`] naming the field, or the button by index, that is outside the limit.
    pub fn validate(&self) -> Result<(), PresetError> {
        if self.name.trim().is_empty() {
            return Err(PresetError::EmptyName);
        }
        if self.name.chars().any(|character| {
            character.is_control()
                || matches!(
                    character,
                    '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|'
                )
        }) {
            return Err(PresetError::InvalidName(self.name.clone()));
        }

        for (field, value) in [
            ("name", &self.name),
            ("details", &self.details),
            ("state", &self.state),
            ("largeImage", &self.large_image),
            ("largeText", &self.large_text),
            ("smallImage", &self.small_image),
            ("smallText", &self.small_text),
        ] {
            check_length(field, value)?;
        }

        if self.buttons.len() > MAX_BUTTONS {
            return Err(PresetError::TooManyButtons {
                count: self.buttons.len(),
                limit: MAX_BUTTONS,
            });
        }

        for (index, button) in self.buttons.iter().enumerate() {
            check_length(&format!("buttons[{index}].label"), &button.label)?;
            check_length(&format!("buttons[{index}].url"), &button.url)?;
            if button.label.trim().is_empty() {
                return Err(PresetError::EmptyButtonLabel { index });
            }
            if !is_https_url(&button.url) {
                return Err(PresetError::InsecureButton {
                    index,
                    url: button.url.clone(),
                });
            }
        }

        Ok(())
    }
}

/// Whether `text` is a usable `https://` URL: the scheme, a host, and no whitespace.
pub fn is_https_url(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("https://") else {
        return false;
    };

    let host = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !host.is_empty()
        && !host.starts_with('.')
        && !text.chars().any(char::is_whitespace)
        && !rest.contains('\\')
}

/// Rejects a field longer than [`MAX_TEXT_BYTES`], counting UTF-8 bytes as Discord does.
fn check_length(field: &str, value: &str) -> Result<(), PresetError> {
    let bytes = value.len();
    if bytes > MAX_TEXT_BYTES {
        return Err(PresetError::TooLong {
            field: field.to_owned(),
            bytes,
            limit: MAX_TEXT_BYTES,
        });
    }
    Ok(())
}

/// Why a preset cannot be applied or saved.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PresetError {
    /// Discord needs a name to answer `setPreset`.
    #[error("preset name is empty")]
    EmptyName,

    /// A name with a path separator would escape the presets directory.
    #[error("preset name '{0}' contains a path separator or a reserved character")]
    InvalidName(String),

    /// One text field is over Discord's limit.
    #[error("preset field '{field}' is {bytes} bytes, limit is {limit}")]
    TooLong {
        /// JSON field name.
        field: String,
        /// Measured UTF-8 length.
        bytes: usize,
        /// Discord's limit.
        limit: usize,
    },

    /// More than [`MAX_BUTTONS`] buttons were supplied.
    #[error("preset has {count} buttons, limit is {limit}")]
    TooManyButtons {
        /// Supplied count.
        count: usize,
        /// Discord's limit.
        limit: usize,
    },

    /// A button has no label.
    #[error("button {index} has an empty label")]
    EmptyButtonLabel {
        /// Zero-based index.
        index: usize,
    },

    /// A button does not point at HTTPS.
    #[error("button {index} must be an https url, got '{url}'")]
    InsecureButton {
        /// Zero-based index.
        index: usize,
        /// Supplied URL.
        url: String,
    },
}

/// Why the preset directory could not be read or written.
#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    /// The directory or one file refused the operation.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation.
        action: &'static str,
        /// File or directory.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// A file is not a preset document.
    #[error("'{}' is not a preset: {source}", path.display())]
    Json {
        /// File that failed to parse.
        path: PathBuf,
        /// Serde's explanation.
        #[source]
        source: serde_json::Error,
    },

    /// A preset failed validation.
    #[error(transparent)]
    Invalid(#[from] PresetError),

    /// `load` found no preset with that name.
    #[error("no preset named '{0}'")]
    Missing(String),
}

/// What one scan of the preset directory found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Scan {
    /// Valid presets, sorted by name.
    pub presets: Vec<Preset>,
    /// Files that could not be used, as sentences for the dashboard.
    pub errors: Vec<String>,
}

/// Reads and writes the preset directory.
#[derive(Debug, Clone)]
pub struct PresetStore {
    directory: PathBuf,
}

impl PresetStore {
    /// A store rooted at an explicit directory, used by tests and by `NEB_HOME` runs.
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    /// The real `%LocalAppData%\ProjectNeb\discord-presets`.
    pub fn at_default_location() -> Self {
        Self::new(neb_core::paths::discord_presets_dir())
    }

    /// Directory this store owns.
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Reads every valid preset and reports the invalid files instead of hiding them.
    pub fn scan(&self) -> Scan {
        let entries = match fs::read_dir(&self.directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Scan::default(),
            Err(error) => {
                return Scan {
                    presets: Vec::new(),
                    errors: vec![format!(
                        "cannot read '{}': {error}",
                        self.directory.display()
                    )],
                };
            }
        };

        let mut scan = Scan::default();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }

            match read_preset(&path) {
                Ok(preset) => match preset.validate() {
                    Ok(()) => scan.presets.push(preset),
                    Err(error) => scan.errors.push(format!("'{}': {error}", path.display())),
                },
                Err(error) => scan.errors.push(error.to_string()),
            }
        }

        scan.presets
            .sort_by(|left, right| left.name.cmp(&right.name));
        scan.errors.sort();
        scan
    }

    /// Every valid preset, sorted by name.
    pub fn list(&self) -> Vec<Preset> {
        self.scan().presets
    }

    /// One preset by exact name.
    ///
    /// # Errors
    ///
    /// [`StoreError::Missing`] when no valid file carries that name.
    pub fn load(&self, name: &str) -> Result<Preset, StoreError> {
        self.scan()
            .presets
            .into_iter()
            .find(|preset| preset.name == name)
            .ok_or_else(|| StoreError::Missing(name.to_owned()))
    }

    /// Validates and writes a preset atomically enough for a hand-edited directory.
    ///
    /// The file name is derived from the preset name with every reserved character replaced, so a
    /// name can never escape this directory.
    ///
    /// # Errors
    ///
    /// [`StoreError::Invalid`] when validation fails, [`StoreError::Io`] when the directory or file
    /// cannot be written.
    pub fn save(&self, preset: &Preset) -> Result<PathBuf, StoreError> {
        preset.validate()?;

        fs::create_dir_all(&self.directory).map_err(|source| StoreError::Io {
            action: "create",
            path: self.directory.clone(),
            source,
        })?;

        let path = self.path_for(preset);
        let text = serde_json::to_string_pretty(preset).map_err(|source| StoreError::Json {
            path: path.clone(),
            source,
        })?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, text.as_bytes()).map_err(|source| StoreError::Io {
            action: "write",
            path: tmp.clone(),
            source,
        })?;

        // `rename` does not replace an existing file on Windows, so the old copy goes first. A crash
        // between the two leaves no preset rather than a half-written one, which the next save fixes.
        if path.exists() {
            fs::remove_file(&path).map_err(|source| StoreError::Io {
                action: "replace",
                path: path.clone(),
                source,
            })?;
        }
        fs::rename(&tmp, &path).map_err(|source| StoreError::Io {
            action: "replace",
            path: path.clone(),
            source,
        })?;

        Ok(path)
    }

    /// File a preset is saved to: the file that already carries this name, else `<slug>.json`.
    ///
    /// Looking at the files rather than only at valid presets means a hand-named file keeps its
    /// name across a save, so one preset cannot become two entries with the same internal name.
    fn path_for(&self, preset: &Preset) -> PathBuf {
        if let Ok(entries) = fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                    continue;
                }
                if read_preset(&path).is_ok_and(|existing| existing.name == preset.name) {
                    return path;
                }
            }
        }

        self.directory.join(format!("{}.json", slug(&preset.name)))
    }
}

/// Reads one JSON file as a preset.
fn read_preset(path: &Path) -> Result<Preset, StoreError> {
    let bytes = fs::read(path).map_err(|source| StoreError::Io {
        action: "read",
        path: path.to_path_buf(),
        source,
    })?;

    // The C# build writes a UTF-8 BOM; a preset copied from it must still parse.
    let text = String::from_utf8_lossy(&bytes);
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);

    serde_json::from_str(text).map_err(|source| StoreError::Json {
        path: path.to_path_buf(),
        source,
    })
}

/// A filesystem-safe stem: letters and digits stay, every run of other characters becomes `-`.
fn slug(name: &str) -> String {
    let mut slug = String::with_capacity(name.len());
    let mut last_dash = false;

    for character in name.trim().chars() {
        if character.is_alphanumeric() || character == '_' {
            slug.extend(character.to_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }

    let slug = slug.trim_matches('-').to_owned();
    if slug.is_empty() {
        "preset".to_owned()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid() -> Preset {
        Preset {
            name: "Gaming".to_owned(),
            details: "Playing ProjectNeb".to_owned(),
            state: "In the shell".to_owned(),
            large_image: "neb".to_owned(),
            large_text: "ProjectNeb".to_owned(),
            small_image: "online".to_owned(),
            small_text: "Ready".to_owned(),
            buttons: vec![Button {
                label: "Project".to_owned(),
                url: "https://example.com/neb".to_owned(),
            }],
            start_timestamp: Some(1_700_000_000),
        }
    }

    #[test]
    fn a_complete_preset_validates() {
        assert_eq!(valid().validate(), Ok(()));
    }

    #[test]
    fn a_129_byte_field_is_refused_by_name() {
        let mut preset = valid();
        preset.details = "x".repeat(MAX_TEXT_BYTES + 1);

        match preset.validate() {
            Err(PresetError::TooLong {
                field,
                bytes,
                limit,
            }) => {
                assert_eq!(field, "details");
                assert_eq!(bytes, MAX_TEXT_BYTES + 1);
                assert_eq!(limit, MAX_TEXT_BYTES);
            }
            other => panic!("expected a length error, got {other:?}"),
        }
    }

    #[test]
    fn the_limit_counts_utf8_bytes_not_characters() {
        let mut preset = valid();
        preset.state = "я".repeat(64);
        assert_eq!(
            preset.validate(),
            Ok(()),
            "64 Cyrillic characters are 128 bytes"
        );

        preset.state = "я".repeat(65);
        assert!(matches!(
            preset.validate(),
            Err(PresetError::TooLong { .. })
        ));
    }

    #[test]
    fn two_buttons_are_allowed_and_a_third_is_not() {
        let mut preset = valid();
        preset.buttons.push(Button {
            label: "Second".to_owned(),
            url: "https://example.com/two".to_owned(),
        });
        assert_eq!(preset.validate(), Ok(()));

        preset.buttons.push(Button {
            label: "Third".to_owned(),
            url: "https://example.com/three".to_owned(),
        });
        match preset.validate() {
            Err(PresetError::TooManyButtons { count, limit }) => {
                assert_eq!((count, limit), (3, MAX_BUTTONS));
            }
            other => panic!("expected a button-count error, got {other:?}"),
        }
    }

    #[test]
    fn plain_http_and_hostless_urls_are_refused() {
        for url in [
            "http://example.com",
            "https://",
            "https:///missing-host",
            "ftp://example.com",
            "https://exa mple.com",
            "https://.example.com",
            "javascript:alert(1)",
        ] {
            let mut preset = valid();
            preset.buttons[0].url = url.to_owned();

            match preset.validate() {
                Err(PresetError::InsecureButton { index, .. }) => assert_eq!(index, 0),
                other => panic!("'{url}' must be refused, got {other:?}"),
            }
            assert!(!is_https_url(url), "'{url}' must not look secure");
        }

        assert!(is_https_url("https://example.com"));
        assert!(is_https_url("https://example.com/a?b=c#d"));
    }

    #[test]
    fn an_empty_name_or_a_path_like_name_is_refused() {
        let mut preset = valid();
        preset.name = "  ".to_owned();
        assert_eq!(preset.validate(), Err(PresetError::EmptyName));

        preset.name = "../escape".to_owned();
        assert!(matches!(
            preset.validate(),
            Err(PresetError::InvalidName(_))
        ));
    }

    #[test]
    fn the_slug_never_contains_a_path_separator() {
        assert_eq!(slug("My Work"), "my-work");
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("???"), "preset");
        assert_eq!(slug("Ночь"), "ночь");
    }

    #[test]
    fn a_preset_round_trips_through_the_store() {
        let directory = tempfile::tempdir().expect("temp dir");
        let store = PresetStore::new(directory.path().join("discord-presets"));
        let preset = valid();

        let path = store.save(&preset).expect("save");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("gaming.json")
        );

        let scan = store.scan();
        assert!(scan.errors.is_empty(), "{:?}", scan.errors);
        assert_eq!(scan.presets, vec![preset.clone()]);
        assert_eq!(store.load("Gaming").expect("load"), preset);
        assert!(matches!(
            store.load("Missing"),
            Err(StoreError::Missing(name)) if name == "Missing"
        ));
    }

    #[test]
    fn saving_the_same_name_replaces_the_file_instead_of_adding_one() {
        let directory = tempfile::tempdir().expect("temp dir");
        let store = PresetStore::new(directory.path());

        store.save(&valid()).expect("first");
        let mut changed = valid();
        changed.details = "Changed".to_owned();
        store.save(&changed).expect("second");

        let presets = store.list();
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].details, "Changed");
    }

    #[test]
    fn a_broken_file_costs_one_preset_not_the_list() {
        let directory = tempfile::tempdir().expect("temp dir");
        let store = PresetStore::new(directory.path());

        store.save(&valid()).expect("save");
        fs::write(directory.path().join("broken.json"), b"{not json").expect("write broken");

        let scan = store.scan();
        assert_eq!(scan.presets.len(), 1, "the valid preset still loads");
        assert_eq!(scan.errors.len(), 1);
        assert!(scan.errors[0].contains("broken.json"), "{:?}", scan.errors);
    }

    #[test]
    fn an_invalid_preset_never_reaches_the_disk() {
        let directory = tempfile::tempdir().expect("temp dir");
        let store = PresetStore::new(directory.path());
        let mut preset = valid();
        preset.buttons[0].url = "http://example.com".to_owned();

        assert!(matches!(
            store.save(&preset),
            Err(StoreError::Invalid(PresetError::InsecureButton { .. }))
        ));
        assert!(store.list().is_empty());
    }

    #[test]
    fn a_missing_directory_is_an_empty_list_not_an_error() {
        let directory = tempfile::tempdir().expect("temp dir");
        let store = PresetStore::new(directory.path().join("never-created"));

        let scan = store.scan();
        assert!(scan.presets.is_empty());
        assert!(scan.errors.is_empty());
    }
}
