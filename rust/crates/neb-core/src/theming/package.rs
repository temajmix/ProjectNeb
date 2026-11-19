//! Theme packages: the JSON shape the settings window imports and exports.
//!
//! A package is:
//!
//! ```json
//! {
//!   "schemaVersion": 1,
//!   "id": "cyberpunk",
//!   "name": "Cyberpunk",
//!   "description": "Magenta and cyan on pure black.",
//!   "author": "ProjectNeb",
//!   "tokens": {
//!     "background": "#000000", "surface": "#14001E", "stroke": "#3A0A4A",
//!     "text": "#E8F7FF", "textDim": "#8F86A8", "accent": "#FF2FB9", "accent2": "#00F0FF",
//!     "radius": 4, "opacity": 0.97, "blur": false, "font": "Cascadia Mono",
//!     "barHeight": 42, "dockTile": 52
//!   }
//! }
//! ```
//!
//! Flat token files written by an earlier build (the seven colours at the root) are still accepted,
//! so an existing user theme can be exported without a migration. Import is strict about every value
//! that is present: one bad colour or an out-of-range radius rejects the whole package instead of
//! silently falling back field by field. Unknown keys at the package and token level are carried
//! through untouched, which is what lets a newer build add a token without losing it here.
//!
//! [`ThemeStore::import`] refuses an id that belongs to a compiled-in preset. A user may still drop a
//! file named after a preset into the themes folder by hand; [`ThemeStore::get`] and
//! [`ThemeStore::list`] prefer the preset in that case, so the build's palettes cannot be shadowed.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::{presets, write_atomic};
use crate::config::{Config, ConfigStore};
use crate::theme::{
    DEFAULT_FONT, DEFAULT_OPACITY, DEFAULT_RADIUS, MAX_DOCK_TILE, MAX_OPACITY, MAX_RADIUS,
    MAX_THEME_BAR_HEIGHT, MIN_DOCK_TILE, MIN_OPACITY, MIN_THEME_BAR_HEIGHT, Rgb, ThemeTokens,
};

/// Version of the package shape this build writes.
pub const PACKAGE_SCHEMA_VERSION: u32 = 1;

/// Largest corner radius a package may ask for.
pub const PACKAGE_MAX_RADIUS: f32 = MAX_RADIUS;

/// Lowest opacity a package may ask for.
pub const PACKAGE_MIN_OPACITY: f32 = MIN_OPACITY;

/// Shortest themed bar a package may ask for.
pub const PACKAGE_MIN_THEME_BAR_HEIGHT: i32 = MIN_THEME_BAR_HEIGHT;

/// Tallest themed bar a package may ask for.
pub const PACKAGE_MAX_THEME_BAR_HEIGHT: i32 = MAX_THEME_BAR_HEIGHT;

/// Smallest themed dock tile a package may ask for.
pub const PACKAGE_MIN_DOCK_TILE: i32 = MIN_DOCK_TILE;

/// Largest themed dock tile a package may ask for.
pub const PACKAGE_MAX_DOCK_TILE: i32 = MAX_DOCK_TILE;

/// Longest id accepted; the same ceiling the C# store uses for a file name.
const MAX_ID: usize = 64;

/// Longest display name accepted.
const MAX_NAME: usize = 80;

/// Longest description accepted.
const MAX_DESCRIPTION: usize = 512;

/// Longest author line accepted.
const MAX_AUTHOR: usize = 80;

/// Every token key this build knows, for separating unknown fields.
const TOKEN_FIELDS: [&str; 14] = [
    "background",
    "surface",
    "stroke",
    "text",
    "textDim",
    "accent",
    "accent2",
    "radius",
    "cornerRadius",
    "opacity",
    "blur",
    "font",
    "barHeight",
    "dockTile",
];

/// Every package key this build knows, for separating unknown fields.
const PACKAGE_FIELDS: [&str; 6] = [
    "schemaVersion",
    "id",
    "name",
    "description",
    "author",
    "tokens",
];

/// A validated theme package.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Package schema version, always [`PACKAGE_SCHEMA_VERSION`] for a value this build produced.
    pub schema_version: u32,
    /// Stable id, which is also the file name.
    pub id: String,
    /// Name shown in the settings window.
    pub name: String,
    /// One line describing the palette.
    pub description: String,
    /// Who wrote the package.
    pub author: String,
    /// The validated token set the bar paints with.
    pub tokens: ThemeTokens,
    /// Package keys this build does not know; written back on export.
    pub extra: Map<String, Value>,
    /// Token keys this build does not know; written back on export.
    pub token_extra: Map<String, Value>,
}

/// Why a theme package could not be read, validated or written.
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

    /// The document is not a usable package, or carries a value the bar cannot use.
    #[error("invalid theme: {0}")]
    Invalid(String),

    /// The id belongs to a preset and cannot be replaced by an imported file.
    #[error("'{0}' is a built-in theme and cannot be replaced")]
    BuiltIn(String),

    /// No preset or user file answers for the id.
    #[error("theme '{0}' was not found")]
    NotFound(String),

    /// The configuration could not be loaded or saved while applying a theme.
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

impl Theme {
    /// Parses and validates a package (or a flat token document from an older build).
    ///
    /// `fallback_id` is used when the document has no `id`; [`ThemeStore`] passes the file stem.
    ///
    /// # Errors
    ///
    /// [`ThemeError::Invalid`] naming the first field that is not acceptable.
    pub fn parse(text: &str, fallback_id: Option<&str>) -> Result<Self, ThemeError> {
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let document: Value = serde_json::from_str(text)
            .map_err(|error| ThemeError::Invalid(format!("not json: {error}")))?;

        Self::from_value(&document, fallback_id)
    }

    /// Validates a parsed JSON document.
    ///
    /// # Errors
    ///
    /// [`ThemeError::Invalid`] naming the first field that is not acceptable.
    pub fn from_value(value: &Value, fallback_id: Option<&str>) -> Result<Self, ThemeError> {
        let object = value
            .as_object()
            .ok_or_else(|| ThemeError::Invalid("a theme must be a json object".to_owned()))?;

        let schema_version = match object.get("schemaVersion") {
            None => PACKAGE_SCHEMA_VERSION,
            Some(value) => value
                .as_u64()
                .filter(|version| *version == u64::from(PACKAGE_SCHEMA_VERSION))
                .ok_or_else(|| {
                    ThemeError::Invalid(format!("schemaVersion must be {PACKAGE_SCHEMA_VERSION}"))
                })? as u32,
        };

        let id = match object.get("id") {
            Some(Value::String(id)) if !id.trim().is_empty() => id.trim().to_owned(),
            Some(Value::Null) | None => fallback_id.unwrap_or("").trim().to_owned(),
            Some(_) => return Err(ThemeError::Invalid("id must be a string".to_owned())),
        };

        if !is_valid_id(&id) {
            return Err(ThemeError::Invalid(format!(
                "'{id}' is not a usable theme id"
            )));
        }

        let name = optional_text(object, "name", MAX_NAME)?
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| id.clone());
        let description =
            optional_text(object, "description", MAX_DESCRIPTION)?.unwrap_or_default();
        let author = optional_text(object, "author", MAX_AUTHOR)?.unwrap_or_default();

        // A package nests the tokens; a flat file from an earlier build *is* the token object.
        let nested = object.get("tokens");
        let tokens_value = match nested {
            Some(Value::Object(_)) => nested.expect("checked"),
            Some(_) => {
                return Err(ThemeError::Invalid(
                    "tokens must be a json object".to_owned(),
                ));
            }
            None => value,
        };
        let tokens_object = tokens_value
            .as_object()
            .ok_or_else(|| ThemeError::Invalid("tokens must be a json object".to_owned()))?;

        let mut tokens = parse_tokens(tokens_object)?;
        tokens.id = id.clone();
        tokens.name = name.clone();

        let (extra, token_extra) = if nested.is_some() {
            (
                extras(object, &PACKAGE_FIELDS),
                extras(tokens_object, &TOKEN_FIELDS),
            )
        } else {
            // A flat document has no separate token object: everything this build knows stays in
            // `tokens`, and only genuinely foreign keys are carried as package extras.
            let mut known = PACKAGE_FIELDS.to_vec();
            known.extend_from_slice(&TOKEN_FIELDS);
            (extras(object, &known), Map::new())
        };

        Ok(Self {
            schema_version,
            id,
            name,
            description,
            author,
            tokens,
            extra,
            token_extra,
        })
    }

    /// Renders the package as a JSON value with `schemaVersion` first.
    pub fn to_value(&self) -> Value {
        let mut root = Map::new();
        root.insert(
            "schemaVersion".to_owned(),
            Value::from(u64::from(self.schema_version)),
        );
        root.insert("id".to_owned(), Value::String(self.id.clone()));
        root.insert("name".to_owned(), Value::String(self.name.clone()));
        root.insert(
            "description".to_owned(),
            Value::String(self.description.clone()),
        );
        root.insert("author".to_owned(), Value::String(self.author.clone()));

        let mut tokens = Map::new();
        insert_colour(&mut tokens, "background", self.tokens.background);
        insert_colour(&mut tokens, "surface", self.tokens.surface);
        insert_colour(&mut tokens, "stroke", self.tokens.stroke);
        insert_colour(&mut tokens, "text", self.tokens.text);
        insert_colour(&mut tokens, "textDim", self.tokens.text_dim);
        insert_colour(&mut tokens, "accent", self.tokens.accent);
        insert_colour(&mut tokens, "accent2", self.tokens.accent2);
        tokens.insert(
            "radius".to_owned(),
            number_or_integer(f64::from(self.tokens.radius)),
        );
        // The token is an `f32`, so writing it as a bare `f64` would produce 0.9700000286102295.
        // Three decimals are enough for an opacity and keep the file readable.
        tokens.insert(
            "opacity".to_owned(),
            Value::from(clean_float(self.tokens.opacity)),
        );
        tokens.insert("blur".to_owned(), Value::Bool(self.tokens.blur));
        tokens.insert("font".to_owned(), Value::String(self.tokens.font.clone()));

        if let Some(height) = self.tokens.bar_height {
            tokens.insert("barHeight".to_owned(), Value::from(height));
        }
        if let Some(tile) = self.tokens.dock_tile {
            tokens.insert("dockTile".to_owned(), Value::from(tile));
        }
        for (key, value) in &self.token_extra {
            tokens.insert(key.clone(), value.clone());
        }
        root.insert("tokens".to_owned(), Value::Object(tokens));

        for (key, value) in &self.extra {
            root.insert(key.clone(), value.clone());
        }

        Value::Object(root)
    }

    /// Renders the package as indented JSON with a trailing newline.
    ///
    /// # Errors
    ///
    /// The serialisation error, which is unreachable for a validated [`Theme`].
    pub fn to_pretty_json(&self) -> Result<String, ThemeError> {
        super::pretty_json(&self.to_value())
            .map_err(|error| ThemeError::Invalid(format!("theme is not serialisable: {error}")))
    }
}

/// Loads, lists, imports and exports theme packages in one folder.
#[derive(Debug, Clone)]
pub struct ThemeStore {
    dir: PathBuf,
}

impl ThemeStore {
    /// A store for an explicit folder, used by tests and by portable runs.
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The store for `%LocalAppData%\ProjectNeb\themes`.
    pub fn at_default_location() -> Self {
        Self::new(crate::paths::themes_dir())
    }

    /// The folder this store owns.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Every theme the settings window should offer: the six presets first, then user files sorted
    /// by id. A file that reuses a preset id is skipped, and one broken file never hides the others.
    pub fn list(&self) -> Vec<Theme> {
        let mut themes = presets::all();

        let Ok(entries) = fs::read_dir(&self.dir) else {
            return themes;
        };

        let mut user: Vec<Theme> = entries
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .filter_map(|path| read_theme(&path).ok())
            .filter(|theme| !presets::is_builtin(&theme.id))
            .collect();
        user.sort_by(|left, right| left.id.cmp(&right.id));

        for theme in user {
            if !themes.iter().any(|known| known.id == theme.id) {
                themes.push(theme);
            }
        }

        themes
    }

    /// The preset or user theme called `id`, if there is one.
    ///
    /// A preset always wins over a hand-placed file with the same id.
    pub fn get(&self, id: &str) -> Option<Theme> {
        if let Some(theme) = presets::built_in(id) {
            return Some(theme);
        }

        self.path_for(id).and_then(|path| read_theme(&path).ok())
    }

    /// Whether a preset or user theme answers for `id`.
    pub fn contains(&self, id: &str) -> bool {
        self.get(id).is_some()
    }

    /// Validates `json` and writes it as `<dir>/<id>.json`, atomically.
    ///
    /// # Errors
    ///
    /// [`ThemeError::Invalid`] when the package is unusable, [`ThemeError::BuiltIn`] when the id
    /// belongs to a preset, [`ThemeError::Io`] when the folder or file cannot be written.
    pub fn import(&self, json: &str) -> Result<Theme, ThemeError> {
        let theme = Theme::parse(json, None)?;

        if presets::is_builtin(&theme.id) {
            return Err(ThemeError::BuiltIn(theme.id));
        }

        let text = theme.to_pretty_json()?;
        let path = self
            .path_for(&theme.id)
            .ok_or_else(|| ThemeError::Invalid(format!("'{}' is not a usable id", theme.id)))?;
        write_atomic(&path, text.as_bytes()).map_err(|source| io_error("write", &path, source))?;

        Ok(theme)
    }

    /// Renders the preset or user theme called `id` as a package, `schemaVersion` first.
    ///
    /// # Errors
    ///
    /// [`ThemeError::NotFound`] when no theme answers, [`ThemeError::Invalid`] when a user file is
    /// unreadable, [`ThemeError::Io`] when the file cannot be read.
    pub fn export(&self, id: &str) -> Result<String, ThemeError> {
        let theme = self
            .get(id)
            .ok_or_else(|| ThemeError::NotFound(id.to_owned()))?;

        theme.to_pretty_json()
    }

    /// Removes a user theme. A preset cannot be deleted.
    ///
    /// # Errors
    ///
    /// [`ThemeError::BuiltIn`] for a preset id, [`ThemeError::NotFound`] when there is no file, and
    /// [`ThemeError::Io`] when the file system refuses.
    pub fn delete(&self, id: &str) -> Result<(), ThemeError> {
        if presets::is_builtin(id) {
            return Err(ThemeError::BuiltIn(id.to_owned()));
        }

        let path = self
            .path_for(id)
            .ok_or_else(|| ThemeError::Invalid(format!("'{id}' is not a usable id")))?;
        fs::remove_file(&path).map_err(|source| {
            if source.kind() == std::io::ErrorKind::NotFound {
                ThemeError::NotFound(id.to_owned())
            } else {
                io_error("remove", &path, source)
            }
        })
    }

    /// Sets `shell.themeId` to `id` and writes the config atomically.
    ///
    /// The returned config is what the caller passes to the shell after posting `shell.reload`. The
    /// theme must exist, so a typo cannot leave the bar falling back to the baked-in palette.
    ///
    /// # Errors
    ///
    /// [`ThemeError::NotFound`] when no theme answers, [`ThemeError::Config`] when the config cannot
    /// be read or written.
    pub fn apply_to_config(&self, id: &str, store: &ConfigStore) -> Result<Config, ThemeError> {
        if !self.contains(id) {
            return Err(ThemeError::NotFound(id.to_owned()));
        }

        let mut config = store.load_checked()?;
        config.shell.theme_id = id.to_owned();
        store.save(&config)?;

        Ok(config)
    }

    /// The file for `id`, or `None` when the id could escape the folder.
    fn path_for(&self, id: &str) -> Option<PathBuf> {
        is_valid_id(id).then(|| self.dir.join(format!("{}.json", id.trim())))
    }
}

/// Whether `id` can become a file name in the themes or profiles folder.
///
/// Path separators, drive letters, `..` and control characters are rejected before a path is built,
/// so an import can never write outside the folder it was given.
pub fn is_valid_id(id: &str) -> bool {
    let trimmed = id.trim();

    !trimmed.is_empty()
        && trimmed.len() <= MAX_ID
        && trimmed != "."
        && trimmed != ".."
        && !trimmed.starts_with('.')
        && !trimmed.ends_with('.')
        && trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

/// Reads and validates one file.
fn read_theme(path: &Path) -> Result<Theme, ThemeError> {
    let text = fs::read_to_string(path).map_err(|source| io_error("read", path, source))?;
    let fallback = path.file_stem().and_then(|stem| stem.to_str());

    Theme::parse(&text, fallback)
}

/// Validates every token that is present. Missing optional tokens take their documented defaults.
fn parse_tokens(object: &Map<String, Value>) -> Result<ThemeTokens, ThemeError> {
    let background = colour(object, "background")?;
    let surface = colour(object, "surface")?;
    let stroke = colour(object, "stroke")?;
    let text = colour(object, "text")?;
    let text_dim = colour(object, "textDim")?;
    let accent = colour(object, "accent")?;
    let accent2 = colour(object, "accent2")?;

    // `radius` is the v2 spelling; `cornerRadius` is what the shipped flat files carry.
    let radius = match object.get("radius").or_else(|| object.get("cornerRadius")) {
        None => DEFAULT_RADIUS,
        Some(value) => {
            let radius = value
                .as_f64()
                .ok_or_else(|| ThemeError::Invalid("radius must be a number".to_owned()))?;
            if !radius.is_finite() || !(0.0..=f64::from(MAX_RADIUS)).contains(&radius) {
                return Err(ThemeError::Invalid(format!(
                    "radius must be 0..={MAX_RADIUS}"
                )));
            }
            radius as f32
        }
    };

    let opacity = match object.get("opacity") {
        None => DEFAULT_OPACITY,
        Some(value) => {
            let opacity = value
                .as_f64()
                .ok_or_else(|| ThemeError::Invalid("opacity must be a number".to_owned()))?;
            // `0.4` as an `f64` is just below `f64::from(0.4_f32)`; the boundary is inclusive.
            let minimum = f64::from(MIN_OPACITY) - 1e-6;
            let maximum = f64::from(MAX_OPACITY) + 1e-6;
            if !opacity.is_finite() || !(minimum..=maximum).contains(&opacity) {
                return Err(ThemeError::Invalid(format!(
                    "opacity must be {MIN_OPACITY}..={MAX_OPACITY}"
                )));
            }
            opacity as f32
        }
    };

    let blur = match object.get("blur") {
        None => false,
        Some(Value::Bool(blur)) => *blur,
        Some(_) => return Err(ThemeError::Invalid("blur must be a boolean".to_owned())),
    };

    let font = match object.get("font") {
        None => DEFAULT_FONT.to_owned(),
        Some(Value::String(font)) if !font.trim().is_empty() && font.chars().count() <= 64 => {
            font.trim().to_owned()
        }
        Some(Value::String(_)) => {
            return Err(ThemeError::Invalid(
                "font must be a non-empty name of at most 64 characters".to_owned(),
            ));
        }
        Some(_) => return Err(ThemeError::Invalid("font must be a string".to_owned())),
    };

    let bar_height = optional_integer(
        object,
        "barHeight",
        MIN_THEME_BAR_HEIGHT,
        MAX_THEME_BAR_HEIGHT,
    )?;
    let dock_tile = optional_integer(object, "dockTile", MIN_DOCK_TILE, MAX_DOCK_TILE)?;

    Ok(ThemeTokens {
        id: String::new(),
        name: String::new(),
        background,
        surface,
        stroke,
        text,
        text_dim,
        accent,
        accent2,
        radius,
        opacity,
        blur,
        font,
        bar_height,
        dock_tile,
    })
}

/// Reads a required `#RRGGBB` colour.
fn colour(object: &Map<String, Value>, field: &str) -> Result<Rgb, ThemeError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| ThemeError::Invalid(format!("'{field}' is required")))?;

    parse_hex(value).ok_or_else(|| {
        ThemeError::Invalid(format!(
            "'{field}' is '{value}', which is not a #RRGGBB colour"
        ))
    })
}

/// Parses exactly `#RRGGBB` into channels.
fn parse_hex(value: &str) -> Option<Rgb> {
    let digits = value.strip_prefix('#')?;
    if digits.len() != 6 || !digits.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }

    Some(Rgb {
        red: u8::from_str_radix(&digits[0..2], 16).ok()?,
        green: u8::from_str_radix(&digits[2..4], 16).ok()?,
        blue: u8::from_str_radix(&digits[4..6], 16).ok()?,
    })
}

/// Reads an optional string field, rejecting a value of the wrong type or an oversized one.
fn optional_text(
    object: &Map<String, Value>,
    field: &str,
    limit: usize,
) -> Result<Option<String>, ThemeError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            let text = text.trim();
            if text.chars().count() > limit {
                return Err(ThemeError::Invalid(format!(
                    "'{field}' is longer than {limit} characters"
                )));
            }
            Ok(Some(text.to_owned()))
        }
        Some(_) => Err(ThemeError::Invalid(format!("'{field}' must be a string"))),
    }
}

/// Reads an optional integer token and enforces its range.
fn optional_integer(
    object: &Map<String, Value>,
    field: &str,
    minimum: i32,
    maximum: i32,
) -> Result<Option<i32>, ThemeError> {
    match object.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => {
            let number = value
                .as_i64()
                .ok_or_else(|| ThemeError::Invalid(format!("'{field}' must be a whole number")))?;
            let number = i32::try_from(number)
                .map_err(|_| ThemeError::Invalid(format!("'{field}' is out of range")))?;
            if !(minimum..=maximum).contains(&number) {
                return Err(ThemeError::Invalid(format!(
                    "'{field}' must be {minimum}..={maximum}"
                )));
            }
            Ok(Some(number))
        }
    }
}

/// The keys of `object` that are not in `known`, in their original order.
fn extras(object: &Map<String, Value>, known: &[&str]) -> Map<String, Value> {
    object
        .iter()
        .filter(|(key, _)| !known.contains(&key.as_str()))
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect()
}

/// Writes one colour in the only spelling the C# store accepts.
fn insert_colour(tokens: &mut Map<String, Value>, field: &str, colour: Rgb) {
    tokens.insert(
        field.to_owned(),
        Value::String(format!(
            "#{:02X}{:02X}{:02X}",
            colour.red, colour.green, colour.blue
        )),
    );
}

/// An `f32` token as the shortest useful decimal, without its binary-expansion tail.
fn clean_float(number: f32) -> f64 {
    (f64::from(number) * 1_000.0).round() / 1_000.0
}

/// A number that is written without a pointless `.0` when it is whole.
fn number_or_integer(number: f64) -> Value {
    if number.fract() == 0.0 && number.abs() < 9_007_199_254_740_992.0 {
        Value::from(number as i64)
    } else {
        Value::from(number)
    }
}

/// Builds a typed file-system error.
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
    use crate::config::Config;
    use serde_json::json;
    use tempfile::tempdir;

    fn cyberpunk() -> String {
        presets::built_in("cyberpunk")
            .expect("the preset must parse")
            .to_pretty_json()
            .expect("the preset must render")
    }

    fn store() -> (tempfile::TempDir, ThemeStore) {
        let dir = tempdir().expect("temp dir");
        let store = ThemeStore::new(dir.path().join("themes"));
        (dir, store)
    }

    fn replace(theme: &str, from: &str, to: &str) -> String {
        theme.replace(from, to)
    }

    #[test]
    fn a_package_parses_with_every_token() {
        let theme = Theme::parse(&cyberpunk(), None).expect("the preset exports a package");

        assert_eq!(theme.schema_version, PACKAGE_SCHEMA_VERSION);
        assert_eq!(theme.id, "cyberpunk");
        assert_eq!(theme.name, "Cyberpunk");
        assert_eq!(
            theme.tokens.background,
            Rgb {
                red: 0,
                green: 0,
                blue: 0
            }
        );
        assert_eq!(
            theme.tokens.accent,
            Rgb {
                red: 0xFF,
                green: 0x2F,
                blue: 0xB9
            }
        );
        assert_eq!(theme.tokens.bar_height, Some(42));
        assert_eq!(theme.tokens.dock_tile, Some(52));
        assert!(!theme.tokens.blur);
    }

    #[test]
    fn export_puts_schema_version_first_and_ends_with_a_newline() {
        let text = Theme::parse(&cyberpunk(), None)
            .expect("parse")
            .to_pretty_json()
            .expect("render");

        assert!(text.starts_with("{\n  \"schemaVersion\": 1,"), "{text}");
        assert!(text.ends_with('\n'));
        assert!(text.contains("\"tokens\": {"), "{text}");
    }

    #[test]
    fn import_rejects_a_colour_that_is_not_rrggbb() {
        let (_dir, store) = store();

        for broken in ["#FFF", "0x000000", "#00000G", "", "black"] {
            let text = replace(&cyberpunk(), "\"#000000\"", &format!("\"{broken}\""));

            match store.import(&text) {
                Err(ThemeError::Invalid(message)) => {
                    assert!(message.contains("RRGGBB"), "{broken}: {message}");
                }
                other => panic!("'{broken}' must be rejected, got {other:?}"),
            }
        }

        assert!(
            !store.dir().exists(),
            "a rejected import must not create the folder"
        );
    }

    #[test]
    fn import_rejects_every_out_of_range_number() {
        let (_dir, store) = store();

        for (field, value) in [
            ("radius", "33"),
            ("radius", "-1"),
            ("opacity", "0.39"),
            ("opacity", "1.01"),
            ("barHeight", "27"),
            ("barHeight", "65"),
            ("dockTile", "31"),
            ("dockTile", "73"),
        ] {
            let text = replace(
                &cyberpunk(),
                &format!("\"{field}\": {}", export_number(&cyberpunk(), field)),
                &format!("\"{field}\": {value}"),
            );

            assert!(
                store.import(&text).is_err(),
                "{field}={value} must be rejected"
            );
        }

        assert!(
            !store.dir().exists(),
            "a rejected import must not create the folder"
        );
    }

    fn export_number(theme: &str, field: &str) -> String {
        let value: Value = serde_json::from_str(theme).expect("json");
        value["tokens"][field].to_string()
    }

    #[test]
    fn import_refuses_a_path_traversal_id() {
        let (_dir, store) = store();

        for id in [
            "../config",
            r"a\b",
            "a/b",
            "C:theme",
            "..",
            ".hidden",
            "   ",
        ] {
            let text = replace(&cyberpunk(), "\"cyberpunk\"", &format!("\"{id}\""));

            assert!(
                store.import(&text).is_err(),
                "'{id}' must not become a theme id"
            );
            assert!(!is_valid_id(id), "'{id}' must not be a valid id");
        }

        assert!(
            !store.dir().exists(),
            "a traversal id must not create or escape the folder"
        );
    }

    #[test]
    fn import_refuses_to_replace_a_preset() {
        let (_dir, store) = store();
        let text = replace(&cyberpunk(), "\"cyberpunk\"", "\"hyprland\"");

        match store.import(&text) {
            Err(ThemeError::BuiltIn(id)) => assert_eq!(id, "hyprland"),
            other => panic!("expected BuiltIn, got {other:?}"),
        }
    }

    #[test]
    fn import_writes_a_user_theme_and_export_round_trips_it() {
        let (_dir, store) = store();
        let imported = store
            .import(&cyberpunk())
            .expect_err("cyberpunk is a preset");

        assert!(matches!(imported, ThemeError::BuiltIn(_)));

        let text = replace(&cyberpunk(), "\"cyberpunk\"", "\"neon-night\"");
        let theme = store.import(&text).expect("a new id imports");
        let path = store.dir().join("neon-night.json");
        assert!(path.is_file());
        assert_eq!(store.get("neon-night"), Some(theme.clone()));

        let exported = store.export("neon-night").expect("export");
        assert_eq!(
            Theme::parse(&exported, None).expect("re-import"),
            theme,
            "export → import must be lossless"
        );
    }

    #[test]
    fn unknown_fields_survive_import_and_export() {
        let (_dir, store) = store();
        let text = replace(&cyberpunk(), "\"cyberpunk\"", "\"neon-night\"");
        let text = replace(
            &text,
            "\"author\": \"ProjectNeb\",",
            "\"author\": \"ProjectNeb\",\n  \"futurePackageField\": { \"n\": 7 },",
        );
        let text = replace(
            &text,
            "\"dockTile\": 52",
            "\"dockTile\": 52,\n    \"futureToken\": [1, 2, 3]",
        );

        let theme = store.import(&text).expect("import keeps unknowns");
        let exported = store.export("neon-night").expect("export");
        let value: Value = serde_json::from_str(&exported).expect("json");

        assert_eq!(theme.extra["futurePackageField"]["n"], json!(7));
        assert_eq!(value["futurePackageField"]["n"], json!(7));
        assert_eq!(value["tokens"]["futureToken"], json!([1, 2, 3]));
    }

    #[test]
    fn the_list_is_the_six_presets_then_user_themes() {
        let (_dir, store) = store();
        let text = replace(&cyberpunk(), "\"cyberpunk\"", "\"zeta\"");
        store.import(&text).expect("import");

        let ids: Vec<String> = store.list().into_iter().map(|theme| theme.id).collect();

        assert_eq!(ids[0], "nebula-glass");
        assert_eq!(ids[6], "zeta");
        assert_eq!(store.list().len(), 7);
    }

    #[test]
    fn a_hand_placed_file_cannot_shadow_a_preset() {
        let (_dir, store) = store();
        let text = replace(&cyberpunk(), "\"cyberpunk\"", "\"hyprland\"");
        std::fs::create_dir_all(store.dir()).expect("dir");
        std::fs::write(store.dir().join("hyprland.json"), text).expect("write");

        assert_eq!(store.get("hyprland").expect("preset").author, "ProjectNeb");
        assert!(
            !store
                .list()
                .iter()
                .any(|theme| theme.id == "hyprland" && theme.name != "Hyprland"),
            "the preset must stay the one the build ships"
        );
    }

    #[test]
    fn delete_removes_a_user_theme_but_not_a_preset() {
        let (_dir, store) = store();
        let text = replace(&cyberpunk(), "\"cyberpunk\"", "\"temporary\"");
        store.import(&text).expect("import");

        store.delete("temporary").expect("delete");
        assert!(store.get("temporary").is_none());
        assert!(matches!(
            store.delete("cyberpunk"),
            Err(ThemeError::BuiltIn(_))
        ));
        assert!(matches!(
            store.delete("temporary"),
            Err(ThemeError::NotFound(_))
        ));
    }

    #[test]
    fn apply_to_config_writes_the_theme_id_and_returns_the_config() {
        let (dir, store) = store();
        let config_store = ConfigStore::new(dir.path().join("config.json"));

        let config = store
            .apply_to_config("cyberpunk", &config_store)
            .expect("apply");

        assert_eq!(config.shell.theme_id, "cyberpunk");
        assert_eq!(
            config_store.load_checked().expect("load").shell.theme_id,
            "cyberpunk"
        );
        assert!(matches!(
            store.apply_to_config("nope", &config_store),
            Err(ThemeError::NotFound(_))
        ));
    }

    #[test]
    fn a_flat_theme_from_an_older_build_still_exports_as_a_package() {
        let (_dir, store) = store();
        std::fs::create_dir_all(store.dir()).expect("dir");
        std::fs::write(
            store.dir().join("legacy.json"),
            r##"{
                "id": "legacy", "name": "Legacy",
                "background": "#101010", "surface": "#202020", "stroke": "#303030",
                "text": "#FFFFFF", "textDim": "#AAAAAA", "accent": "#00FFEE", "accent2": "#FF00EE",
                "cornerRadius": 10
            }"##,
        )
        .expect("write");

        let theme = store.get("legacy").expect("a flat file is a theme");
        assert!((theme.tokens.radius - 10.0).abs() < f32::EPSILON);

        let exported = store.export("legacy").expect("export");
        let value: Value = serde_json::from_str(&exported).expect("json");
        assert_eq!(value["schemaVersion"], json!(1));
        assert_eq!(value["tokens"]["background"], json!("#101010"));
    }

    #[test]
    fn the_default_store_points_at_the_data_root() {
        let store = ThemeStore::at_default_location();

        assert_eq!(store.dir(), crate::paths::themes_dir());
    }

    #[test]
    fn a_fresh_store_still_lists_the_presets_without_a_folder() {
        let dir = tempdir().expect("temp dir");
        let store = ThemeStore::new(dir.path().join("absent"));

        assert_eq!(store.list().len(), 6);
        assert!(store.contains("nebula-glass"));
    }

    #[test]
    fn applying_a_theme_uses_the_atomic_config_writer() {
        let (dir, store) = store();
        let config_store = ConfigStore::new(dir.path().join("config.json"));

        store
            .apply_to_config("hyprland", &config_store)
            .expect("apply");

        let names: Vec<String> = std::fs::read_dir(dir.path())
            .expect("dir")
            .map(|entry| {
                entry
                    .expect("entry")
                    .file_name()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();

        assert_eq!(names, vec!["config.json".to_owned()]);
    }

    #[test]
    fn a_config_is_available_after_apply_for_the_shell_reload() {
        let (dir, store) = store();
        let config_store = ConfigStore::new(dir.path().join("config.json"));
        let config_store_2 = ConfigStore::new(dir.path().join("config.json"));

        let config = store
            .apply_to_config("macos", &config_store)
            .expect("apply");

        assert_eq!(config, config_store_2.load_checked().expect("load"));
        assert_eq!(config.shell.theme(), "macos");
        assert_eq!(Config::default().language, config.language);
    }
}
