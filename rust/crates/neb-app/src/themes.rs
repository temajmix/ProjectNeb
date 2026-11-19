//! Theme documents: the two the build ships and the ones the user owns.
//!
//! The file contract is `Neb.Core.Theming`'s: `themes/<id>.json` with seven `#RRGGBB` colours, a
//! name and a corner radius. `neb-shell` paints the bar from the same folder, so `themes.save` here
//! writes exactly what that reader accepts and `themes.list` reads files the user edited by hand.
//!
//! Nothing in this module touches Win32, so every rule below is unit-tested.

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A theme as it lives on disk.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeDoc {
    /// Stable id, which is also the file name.
    pub id: String,
    /// Name shown in the settings window.
    pub name: String,
    /// Window and bar background.
    pub background: String,
    /// Raised surface fill.
    pub surface: String,
    /// Hairline and border.
    pub stroke: String,
    /// Primary text.
    pub text: String,
    /// Secondary text.
    pub text_dim: String,
    /// Primary highlight.
    pub accent: String,
    /// Secondary highlight.
    pub accent2: String,
    /// Corner radius in device-independent pixels.
    pub corner_radius: f64,
}

impl ThemeDoc {
    /// Whether this document is one of the palettes compiled into the binary.
    ///
    /// A user file that reuses a shipped id is *not* shipped: the build's file is replaced by the
    /// user's, which is what the settings window has to show.
    pub fn is_shipped(&self, shipped: &[ThemeDoc]) -> bool {
        shipped.contains(self)
    }
}

/// The palettes the build ships, in the order the settings window lists them.
const BUILT_IN_JSON: [&str; 2] = [
    r##"{
  "id": "liquid-glass",
  "name": "Liquid Glass",
  "background": "#0B0A12",
  "surface": "#1E1A2E",
  "stroke": "#2B2544",
  "text": "#F6F4FF",
  "textDim": "#B7B3C9",
  "accent": "#A78BFA",
  "accent2": "#7C5CFF",
  "cornerRadius": 14
}"##,
    r##"{
  "id": "graphite",
  "name": "Graphite",
  "background": "#131416",
  "surface": "#212327",
  "stroke": "#303338",
  "text": "#ECEEF0",
  "textDim": "#9AA0A6",
  "accent": "#D8DEE3",
  "accent2": "#8A9299",
  "cornerRadius": 8
}"##,
];

/// The themes folder under a ProjectNeb data root, the same one `neb-shell` reads.
pub fn dir(root: &Path) -> PathBuf {
    root.join("themes")
}

/// Longest id accepted, so a user theme cannot pick a name no file system likes.
const MAX_ID: usize = 64;

/// Corner radius range the settings window offers.
const MAX_RADIUS: f64 = 64.0;

/// Why a theme document was refused.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    /// The file system refused the operation.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation, e.g. `write`.
        action: &'static str,
        /// File or folder the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The document is not a theme, or carries a value the bar cannot use.
    #[error("invalid theme: {0}")]
    Invalid(String),
}

/// The palettes compiled into the binary, parsed once per call.
///
/// The JSON is compiled in rather than read from a folder so a broken installation still lists the
/// themes the build promises.
pub fn built_in() -> Vec<ThemeDoc> {
    BUILT_IN_JSON
        .iter()
        .filter_map(|text| serde_json::from_str::<Value>(text).ok())
        .filter_map(|value| parse_document(&value, "").ok())
        .collect()
}

/// Every theme the settings window should offer: the built-ins first, then the user's files,
/// replacing a built-in when the user saved one under the same id.
///
/// A folder that does not exist is not an error: that is the state of every fresh profile. A file
/// that is not a theme is skipped, because one hand-edited file must not hide the others.
pub fn list(dir: &Path) -> Vec<ThemeDoc> {
    let mut themes = built_in();

    let Ok(entries) = fs::read_dir(dir) else {
        return themes;
    };

    let mut user: Vec<ThemeDoc> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .filter_map(|path| read_document(&path).ok())
        .collect();

    user.sort_by(|left, right| left.id.cmp(&right.id));

    for theme in user {
        match themes.iter_mut().find(|known| known.id == theme.id) {
            Some(known) => *known = theme,
            None => themes.push(theme),
        }
    }

    themes
}

/// Reads one theme file.
///
/// # Errors
///
/// [`ThemeError::Io`] when the file cannot be read, [`ThemeError::Invalid`] when it is not a theme.
pub fn read_document(path: &Path) -> Result<ThemeDoc, ThemeError> {
    let text = fs::read_to_string(path).map_err(|source| io_error("read", path, source))?;
    let value: Value = serde_json::from_str(&text).map_err(|error| {
        ThemeError::Invalid(format!("'{}' is not json: {error}", path.display()))
    })?;

    let fallback_id = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_default();

    parse_document(&value, &fallback_id)
}

/// Validates one theme document.
///
/// `fallback_id` is the file stem, used when the JSON omits `id` — the same tolerance the bar has.
///
/// # Errors
///
/// [`ThemeError::Invalid`] naming the first field that is not acceptable.
pub fn parse_document(value: &Value, fallback_id: &str) -> Result<ThemeDoc, ThemeError> {
    let Value::Object(object) = value else {
        return Err(ThemeError::Invalid(
            "a theme must be a json object".to_owned(),
        ));
    };

    let id = match object.get("id") {
        Some(Value::String(id)) if !id.trim().is_empty() => id.trim().to_owned(),
        Some(Value::Null) | None => fallback_id.to_owned(),
        Some(_) => return Err(ThemeError::Invalid("id must be a string".to_owned())),
    };

    if !valid_id(&id) {
        return Err(ThemeError::Invalid(format!(
            "'{id}' is not a usable theme id"
        )));
    }

    let name = match object.get("name") {
        Some(Value::String(name)) if !name.trim().is_empty() => name.trim().to_owned(),
        Some(Value::Null) | None => id.clone(),
        Some(_) => return Err(ThemeError::Invalid("name must be a string".to_owned())),
    };

    let color = |field: &str| -> Result<String, ThemeError> {
        match object.get(field) {
            Some(Value::String(value)) if is_hex_color(value) => Ok(value.clone()),
            Some(Value::String(value)) => Err(ThemeError::Invalid(format!(
                "'{field}' is '{value}', which is not a #RRGGBB colour"
            ))),
            Some(_) => Err(ThemeError::Invalid(format!("'{field}' must be a string"))),
            None => Err(ThemeError::Invalid(format!("'{field}' is required"))),
        }
    };

    let corner_radius = match object.get("cornerRadius") {
        Some(Value::Number(number)) => number
            .as_f64()
            .filter(|radius| radius.is_finite() && (0.0..=MAX_RADIUS).contains(radius))
            .ok_or_else(|| ThemeError::Invalid(format!("cornerRadius must be 0..={MAX_RADIUS}")))?,
        Some(Value::Null) | None => 12.0,
        Some(_) => {
            return Err(ThemeError::Invalid(
                "cornerRadius must be a number".to_owned(),
            ));
        }
    };

    Ok(ThemeDoc {
        id,
        name,
        background: color("background")?,
        surface: color("surface")?,
        stroke: color("stroke")?,
        text: color("text")?,
        text_dim: color("textDim")?,
        accent: color("accent")?,
        accent2: color("accent2")?,
        corner_radius,
    })
}

/// Writes `theme` as `<dir>/<id>.json`, atomically.
///
/// # Errors
///
/// [`ThemeError::Invalid`] when the document itself is unusable, [`ThemeError::Io`] when the folder
/// or the file cannot be written.
pub fn save(dir: &Path, theme: &ThemeDoc) -> Result<PathBuf, ThemeError> {
    if !valid_id(&theme.id) {
        return Err(ThemeError::Invalid(format!(
            "'{}' is not a usable theme id",
            theme.id
        )));
    }

    // Re-validate what the caller built, so a bug there cannot write a file the bar will reject.
    let document = serde_json::to_value(theme)
        .map_err(|error| ThemeError::Invalid(format!("theme is not serialisable: {error}")))?;
    let checked = parse_document(&document, &theme.id)?;

    fs::create_dir_all(dir).map_err(|source| io_error("create", dir, source))?;

    let path = dir.join(format!("{}.json", checked.id));
    let mut bytes = serde_json::to_vec_pretty(&document)
        .map_err(|error| ThemeError::Invalid(format!("theme is not serialisable: {error}")))?;
    bytes.push(b'\n');

    let tmp = dir.join(format!("{}.json.tmp-{}", checked.id, std::process::id()));
    fs::write(&tmp, &bytes).map_err(|source| io_error("write", &tmp, source))?;

    if let Err(source) = fs::rename(&tmp, &path) {
        let _ = fs::remove_file(&tmp);
        return Err(io_error("replace", &path, source));
    }

    Ok(path)
}

/// Whether `id` can become a file name in the themes folder.
pub fn valid_id(id: &str) -> bool {
    let trimmed = id.trim();

    !trimmed.is_empty()
        && trimmed.len() <= MAX_ID
        && trimmed != "."
        && trimmed != ".."
        && trimmed
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Whether `value` is a `#RRGGBB` colour, the only spelling the C# theme store accepts.
pub fn is_hex_color(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() == 7 && bytes[0] == b'#' && bytes[1..].iter().all(u8::is_ascii_hexdigit)
}

/// One file-system failure, with the action and the path a log line needs.
fn io_error(action: &'static str, path: &Path, source: std::io::Error) -> ThemeError {
    ThemeError::Io {
        action,
        path: path.to_path_buf(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn graphite() -> Value {
        serde_json::from_str(BUILT_IN_JSON[1]).expect("the shipped graphite document parses")
    }

    #[test]
    fn both_shipped_themes_are_listed_without_a_folder() {
        let themes = list(Path::new(r"Z:\definitely-not-here"));

        assert_eq!(themes.len(), 2);
        assert_eq!(themes[0].id, "liquid-glass");
        assert_eq!(themes[1].id, "graphite");
        assert!(
            themes.iter().all(|theme| theme.is_shipped(&built_in())),
            "both shipped palettes must be recognisable as the build's"
        );
    }

    #[test]
    fn a_user_theme_appears_after_the_built_ins() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut theme = parse_document(&graphite(), "").expect("valid");
        theme.id = "midnight".to_owned();
        theme.name = "Midnight".to_owned();
        save(dir.path(), &theme).expect("save");

        let themes = list(dir.path());

        assert_eq!(themes.len(), 3);
        assert_eq!(
            themes.last().map(|theme| theme.id.as_str()),
            Some("midnight")
        );
        assert!(!themes[2].is_shipped(&built_in()), "a new id is the user's");
    }

    #[test]
    fn a_user_file_overrides_the_built_in_with_the_same_id() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut theme = parse_document(&graphite(), "").expect("valid");
        theme.name = "Graphite (mine)".to_owned();
        save(dir.path(), &theme).expect("save");

        let themes = list(dir.path());

        assert_eq!(
            themes.len(),
            2,
            "the id must not be listed twice: {themes:?}"
        );
        assert_eq!(themes[1].name, "Graphite (mine)");
    }

    #[test]
    fn a_broken_file_is_skipped_but_the_others_survive() {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(dir.path().join("broken.json"), "{ not json").expect("write");
        fs::write(
            dir.path().join("pink.json"),
            r##"{"id":"pink","background":"#FF00FF","surface":"#1E1A2E","stroke":"#2B2544",
                "text":"#FFFFFF","textDim":"#CCCCCC","accent":"#FF00AA","accent2":"#AA00FF",
                "cornerRadius":10}"##,
        )
        .expect("write");

        let ids: Vec<String> = list(dir.path()).into_iter().map(|theme| theme.id).collect();

        assert_eq!(ids, ["liquid-glass", "graphite", "pink"]);
    }

    #[test]
    fn a_bad_colour_is_refused_with_the_field_name() {
        let mut value = graphite();
        value["accent"] = json!("rgb(0,0,0)");

        match parse_document(&value, "") {
            Err(ThemeError::Invalid(message)) => {
                assert!(message.contains("accent"), "{message}");
                assert!(message.contains("rgb(0,0,0)"), "{message}");
            }
            other => panic!("a non-hex colour must be refused, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_field_is_refused_by_name() {
        let mut value = graphite();
        value.as_object_mut().expect("object").remove("textDim");

        match parse_document(&value, "") {
            Err(ThemeError::Invalid(message)) => assert!(message.contains("textDim"), "{message}"),
            other => panic!("a missing token must be refused, got {other:?}"),
        }
    }

    #[test]
    fn the_id_falls_back_to_the_file_stem() {
        let mut value = graphite();
        value.as_object_mut().expect("object").remove("id");

        let theme = parse_document(&value, "sunset").expect("valid");

        assert_eq!(theme.id, "sunset");
        assert_eq!(theme.name, "Graphite");
    }

    #[test]
    fn an_id_that_could_escape_the_folder_is_refused() {
        for id in ["../config", r"a\b", "a/b", "", "   ", &"x".repeat(65)] {
            let mut value = graphite();
            value["id"] = json!(id);

            assert!(
                parse_document(&value, "").is_err(),
                "'{id}' must not become a theme id"
            );
            assert!(!valid_id(id), "'{id}' must not be a valid id");
        }
    }

    #[test]
    fn the_radius_and_colour_round_trip_through_save() {
        let dir = tempfile::tempdir().expect("temp dir");
        let mut theme = parse_document(&graphite(), "").expect("valid");
        theme.corner_radius = 22.0;

        let path = save(dir.path(), &theme).expect("save");
        let back = read_document(&path).expect("read");

        assert_eq!(back, theme);
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some("graphite.json")
        );
    }

    #[test]
    fn an_out_of_range_radius_is_refused() {
        for radius in [-1.0, 1000.0] {
            let mut value = graphite();
            value["cornerRadius"] = json!(radius);

            assert!(
                parse_document(&value, "").is_err(),
                "{radius} must be refused"
            );
        }
    }
}
