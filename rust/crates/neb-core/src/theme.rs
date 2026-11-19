//! Theme tokens v2: the appearance contract shared by the settings window and the shell.
//!
//! The on-disk shape is `themes/<id>.json`, the same file the C# build reads, with the seven
//! `#RRGGBB` colours it already knows plus the tokens this slice adds:
//!
//! ```json
//! {
//!   "id": "liquid-glass", "name": "Liquid Glass",
//!   "background": "#0B0A12", "surface": "#1E1A2E", "stroke": "#2B2544",
//!   "text": "#F6F4FF", "textDim": "#B7B3C9", "accent": "#A78BFA", "accent2": "#7C5CFF",
//!   "radius": 14, "opacity": 0.92, "blur": false,
//!   "font": "Segoe UI Variable Text", "barHeight": 40, "dockTile": 56
//! }
//! ```
//!
//! Validation is the whole point of this module. Every colour must be exactly `#RRGGBB`; a document
//! with one bad colour is rejected as a whole, and the caller falls back to [`ThemeTokens::baked_in`].
//! Numeric and string tokens are validated field by field: an out-of-range value costs only that
//! field, because a theme with a strange radius is still a usable theme. `cornerRadius` from the C#
//! files stays readable as an alias for `radius`.
//!
//! The tokens are plain values — no Direct2D, no window handles — so the settings window and the
//! tests can use them without a desktop.

use std::fs;
use std::path::{Path, PathBuf};

use crate::logging::LogHandle;

/// The UI font a theme uses when the file does not name one.
pub const DEFAULT_FONT: &str = "Segoe UI Variable Text";

/// Bar opacity when the file does not name one: the brief's 92 % glass.
pub const DEFAULT_OPACITY: f32 = 0.92;

/// Lowest opacity the settings window offers.
pub const MIN_OPACITY: f32 = 0.4;

/// Highest opacity the settings window offers.
pub const MAX_OPACITY: f32 = 1.0;

/// Corner radius when the file does not name one.
pub const DEFAULT_RADIUS: f32 = 12.0;

/// Largest corner radius a theme may ask for.
pub const MAX_RADIUS: f32 = 32.0;

/// Shortest themed bar, in device-independent pixels.
pub const MIN_THEME_BAR_HEIGHT: i32 = 28;

/// Tallest themed bar, in device-independent pixels.
pub const MAX_THEME_BAR_HEIGHT: i32 = 64;

/// Smallest themed dock tile, in device-independent pixels.
pub const MIN_DOCK_TILE: i32 = 32;

/// Largest themed dock tile, in device-independent pixels.
pub const MAX_DOCK_TILE: i32 = 72;

/// Longest font family name a theme may carry; anything longer is a mistake, not a font.
const MAX_FONT_NAME: usize = 64;

/// One validated `#RRGGBB` colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rgb {
    /// Red channel.
    pub red: u8,
    /// Green channel.
    pub green: u8,
    /// Blue channel.
    pub blue: u8,
}

impl Rgb {
    /// White, which the bar's translucent surface fill is made of.
    pub const WHITE: Self = Self {
        red: 0xFF,
        green: 0xFF,
        blue: 0xFF,
    };
}

/// A validated theme document, before any rendering library is involved.
#[derive(Debug, Clone, PartialEq)]
pub struct ThemeTokens {
    /// Stable id, i.e. the file name when the JSON omits it.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Bar background.
    pub background: Rgb,
    /// Raised surface fill.
    pub surface: Rgb,
    /// Hairline and border.
    pub stroke: Rgb,
    /// Primary text.
    pub text: Rgb,
    /// Secondary text.
    pub text_dim: Rgb,
    /// Primary highlight.
    pub accent: Rgb,
    /// Secondary highlight.
    pub accent2: Rgb,
    /// Corner radius in device-independent pixels, `0.0..=32.0`.
    pub radius: f32,
    /// Background opacity, `0.4..=1.0`.
    pub opacity: f32,
    /// Reserved: whether a future renderer blurs what is behind the bar.
    pub blur: bool,
    /// UI font family; a non-empty name no longer than 64 characters.
    pub font: String,
    /// Themed bar height, `28..=64`, or `None` when the file leaves it to the config.
    pub bar_height: Option<i32>,
    /// Themed dock tile, `32..=72`, or `None` when the file leaves it to the default.
    pub dock_tile: Option<i32>,
}

/// Why a theme file was not used.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    /// The file could not be read.
    #[error("cannot read '{}': {source}", path.display())]
    Io {
        /// File that was tried.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
    /// The file is not the JSON object a theme store expects.
    #[error("'{}' is not a theme document: {reason}", path.display())]
    Shape {
        /// File that was tried.
        path: PathBuf,
        /// What was wrong, in words a log line can carry.
        reason: String,
    },
}

impl ThemeTokens {
    /// The palette the C# build ships as `liquid-glass`, used whenever no file answers.
    pub fn baked_in() -> Self {
        Self {
            id: crate::config::DEFAULT_THEME_ID.to_owned(),
            name: "Liquid Glass".to_owned(),
            background: Rgb {
                red: 0x0B,
                green: 0x0A,
                blue: 0x12,
            },
            surface: Rgb {
                red: 0x1E,
                green: 0x1A,
                blue: 0x2E,
            },
            stroke: Rgb {
                red: 0x2B,
                green: 0x25,
                blue: 0x44,
            },
            text: Rgb {
                red: 0xF6,
                green: 0xF4,
                blue: 0xFF,
            },
            text_dim: Rgb {
                red: 0xB7,
                green: 0xB3,
                blue: 0xC9,
            },
            accent: Rgb {
                red: 0xA7,
                green: 0x8B,
                blue: 0xFA,
            },
            accent2: Rgb {
                red: 0x7C,
                green: 0x5C,
                blue: 0xFF,
            },
            radius: 14.0,
            opacity: DEFAULT_OPACITY,
            blur: false,
            font: DEFAULT_FONT.to_owned(),
            bar_height: None,
            dock_tile: None,
        }
    }

    /// Reads and validates one theme document. Split out so tests can feed it text without a disk.
    ///
    /// # Errors
    ///
    /// [`ThemeError::Shape`] for anything that is not a usable theme document.
    pub fn parse(path: &Path, text: &str) -> Result<Self, ThemeError> {
        // A UTF-8 BOM is what many editors write; `serde_json` refuses it, and a theme that a text
        // editor saved must not cost the user the bar.
        let text = text.strip_prefix('\u{feff}').unwrap_or(text);
        let document: serde_json::Value =
            serde_json::from_str(text).map_err(|error| ThemeError::Shape {
                path: path.to_path_buf(),
                reason: error.to_string(),
            })?;

        // A Theme Manager package nests the tokens; a flat file from an earlier build *is* the
        // token object. Both shapes reach the same validator, so the shell paints either one.
        let tokens = document
            .get("tokens")
            .filter(|value| value.is_object())
            .unwrap_or(&document);

        let field = |name: &str| tokens.get(name).and_then(serde_json::Value::as_str);
        let root_field = |name: &str| document.get(name).and_then(serde_json::Value::as_str);

        // Every colour is mandatory: a file this build accepts is a file the C# store accepts.
        let background = colour(path, field("background"))?;
        let surface = colour(path, field("surface"))?;
        let stroke = colour(path, field("stroke"))?;
        let text = colour(path, field("text"))?;
        let text_dim = colour(path, field("textDim"))?;
        let accent = colour(path, field("accent"))?;
        let accent2 = colour(path, field("accent2"))?;

        let id = root_field("id")
            .or_else(|| field("id"))
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .or_else(|| path.file_stem().and_then(|stem| stem.to_str()))
            .ok_or_else(|| ThemeError::Shape {
                path: path.to_path_buf(),
                reason: "no usable id".to_owned(),
            })?;

        let number = |name: &str| tokens.get(name).and_then(serde_json::Value::as_f64);
        let integer = |name: &str| tokens.get(name).and_then(serde_json::Value::as_i64);

        // `radius` is the v2 spelling; `cornerRadius` is what the shipped C# files carry.
        let radius = number("radius")
            .or_else(|| number("cornerRadius"))
            .filter(|radius| radius.is_finite() && (0.0..=f64::from(MAX_RADIUS)).contains(radius))
            .map_or(DEFAULT_RADIUS, |radius| radius as f32);

        // The bounds are `f32` constants; a JSON literal such as `0.4` is an `f64` slightly below
        // `f64::from(0.4_f32)`, so compare with a tolerance instead of rejecting the boundary.
        let min_opacity = f64::from(MIN_OPACITY) - 1e-6;
        let max_opacity = f64::from(MAX_OPACITY) + 1e-6;
        let opacity = number("opacity")
            .filter(|opacity| opacity.is_finite() && (min_opacity..=max_opacity).contains(opacity))
            .map_or(DEFAULT_OPACITY, |opacity| opacity as f32);

        let blur = tokens
            .get("blur")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);

        let font = field("font")
            .map(str::trim)
            .filter(|font| !font.is_empty() && font.chars().count() <= MAX_FONT_NAME)
            .unwrap_or(DEFAULT_FONT)
            .to_owned();

        let bar_height = integer("barHeight")
            .and_then(|height| i32::try_from(height).ok())
            .filter(|height| (MIN_THEME_BAR_HEIGHT..=MAX_THEME_BAR_HEIGHT).contains(height));

        let dock_tile = integer("dockTile")
            .and_then(|tile| i32::try_from(tile).ok())
            .filter(|tile| (MIN_DOCK_TILE..=MAX_DOCK_TILE).contains(tile));

        Ok(Self {
            id: id.to_owned(),
            name: root_field("name")
                .or_else(|| field("name"))
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or(id)
                .to_owned(),
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

    /// The theme called `id` from the default folders, or the baked-in palette.
    ///
    /// Lookup order: `%LocalAppData%\ProjectNeb\themes`, then a `themes` folder next to the
    /// executable (where the packaged build ships them). A broken file is logged and stepped over.
    pub fn load(id: &str, log: Option<&LogHandle>) -> Self {
        let mut dirs = vec![crate::paths::themes_dir()];

        if let Ok(exe) = std::env::current_exe()
            && let Some(parent) = exe.parent()
        {
            dirs.push(parent.join("themes"));
        }

        Self::load_from(id, &dirs, log)
    }

    /// The theme called `id` in the first of `dirs` that answers, or the baked-in palette.
    pub fn load_from(id: &str, dirs: &[PathBuf], log: Option<&LogHandle>) -> Self {
        for dir in dirs.iter().filter(|dir| dir.is_dir()) {
            let direct = dir.join(format!("{id}.json"));
            if direct.is_file() {
                match parse_file(&direct) {
                    Ok(theme) => return theme,
                    Err(error) => warn(log, &format!("{error}, skipped")),
                }
            }

            if let Some(theme) = scan_for_id(dir, id, log) {
                return theme;
            }
        }

        // Presets are compiled in, so a theme chosen in the settings window works on a fresh
        // installation before any file exists in the themes folder.
        if let Some(theme) = crate::theming::built_in(id) {
            return theme.tokens;
        }

        warn(
            log,
            &format!("no theme file answers for '{id}', using the baked-in palette"),
        );
        Self::baked_in()
    }
}

/// Reports a skipped theme file when there is a log to report it to.
fn warn(log: Option<&LogHandle>, message: &str) {
    if let Some(log) = log {
        log.warn("theme", message);
    }
}

/// Finds the file in `dir` whose `id` field is `id`, for a theme whose file name is not its id.
fn scan_for_id(dir: &Path, id: &str, log: Option<&LogHandle>) -> Option<ThemeTokens> {
    let entries = fs::read_dir(dir).ok()?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    files.sort();

    for path in files {
        match parse_file(&path) {
            Ok(theme) if theme.id == id => return Some(theme),
            Ok(_) => {}
            Err(error) => warn(log, &format!("{error}, skipped")),
        }
    }

    None
}

/// Reads and validates one theme file.
fn parse_file(path: &Path) -> Result<ThemeTokens, ThemeError> {
    let text = fs::read_to_string(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    ThemeTokens::parse(path, &text)
}

/// Accepts exactly `#RRGGBB`, the only spelling both builds accept.
fn colour(path: &Path, value: Option<&str>) -> Result<Rgb, ThemeError> {
    let Some(value) = value else {
        return Err(ThemeError::Shape {
            path: path.to_path_buf(),
            reason: "a colour is missing".to_owned(),
        });
    };

    let trimmed = value.trim();
    let digits = trimmed
        .strip_prefix('#')
        .filter(|digits| digits.len() == 6 && digits.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .ok_or_else(|| ThemeError::Shape {
            path: path.to_path_buf(),
            reason: format!("'{value}' is not #RRGGBB"),
        })?;

    let channel = |at: usize| {
        u8::from_str_radix(&digits[at..at + 2], 16).map_err(|error| ThemeError::Shape {
            path: path.to_path_buf(),
            reason: format!("'{value}' is not #RRGGBB: {error}"),
        })
    };

    Ok(Rgb {
        red: channel(0)?,
        green: channel(2)?,
        blue: channel(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn path() -> PathBuf {
        PathBuf::from("graphite.json")
    }

    const GRAPHITE: &str = r##"{
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
}"##;

    #[test]
    fn a_shipped_theme_file_parses_with_the_v2_defaults() {
        let theme = ThemeTokens::parse(&path(), GRAPHITE).expect("graphite must parse");

        assert_eq!(theme.id, "graphite");
        assert_eq!(theme.name, "Graphite");
        assert_eq!(theme.background.red, 0x13);
        assert!((theme.radius - 8.0).abs() < f32::EPSILON);
        assert!((theme.opacity - DEFAULT_OPACITY).abs() < f32::EPSILON);
        assert!(!theme.blur);
        assert_eq!(theme.font, DEFAULT_FONT);
        assert_eq!(theme.bar_height, None);
        assert_eq!(theme.dock_tile, None);
    }

    #[test]
    fn the_theme_shipped_next_to_the_c_sharp_app_parses() {
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../src/Neb.App/themes/liquid-glass.json");
        let text = std::fs::read_to_string(&shipped).expect("the shipped theme must be readable");

        let theme = ThemeTokens::parse(&shipped, &text).expect("liquid-glass must parse");

        assert_eq!(theme.id, crate::config::DEFAULT_THEME_ID);
        assert_eq!(theme.name, "Liquid Glass");
    }

    #[test]
    fn a_theme_manager_package_parses_through_the_nested_tokens() {
        let text = r##"{
          "schemaVersion": 1,
          "id": "cyberpunk",
          "name": "Cyberpunk",
          "description": "Magenta and cyan on black.",
          "author": "ProjectNeb",
          "tokens": {
            "background": "#000000", "surface": "#14001E", "stroke": "#3A0A4A",
            "text": "#E8F7FF", "textDim": "#8F86A8", "accent": "#FF2FB9",
            "accent2": "#00F0FF", "radius": 4, "opacity": 0.97, "blur": false,
            "font": "Cascadia Mono", "barHeight": 42, "dockTile": 52
          }
        }"##;

        let theme = ThemeTokens::parse(&path(), text).expect("a package must parse");

        assert_eq!(theme.id, "cyberpunk");
        assert_eq!(theme.name, "Cyberpunk");
        assert_eq!(theme.accent.red, 0xFF);
        assert_eq!(theme.bar_height, Some(42));
        assert_eq!(theme.dock_tile, Some(52));
        assert_eq!(theme.font, "Cascadia Mono");
    }

    #[test]
    fn a_preset_answers_when_no_file_exists() {
        let dir = tempfile::tempdir().expect("temp dir");
        let theme = ThemeTokens::load_from("cyberpunk", &[dir.path().to_path_buf()], None);

        assert_eq!(theme.id, "cyberpunk");
        assert_eq!(theme.name, "Cyberpunk");
        assert_ne!(theme, ThemeTokens::baked_in());
    }

    #[test]
    fn every_v2_token_is_read() {
        let text = GRAPHITE.replace(
            "\"cornerRadius\": 8",
            r##""radius": 22, "opacity": 0.75, "blur": true,
                "font": "Cascadia Mono", "barHeight": 48, "dockTile": 48"##,
        );
        let theme = ThemeTokens::parse(&path(), &text).expect("the v2 tokens must parse");

        assert!((theme.radius - 22.0).abs() < f32::EPSILON);
        assert!((theme.opacity - 0.75).abs() < f32::EPSILON);
        assert!(theme.blur);
        assert_eq!(theme.font, "Cascadia Mono");
        assert_eq!(theme.bar_height, Some(48));
        assert_eq!(theme.dock_tile, Some(48));
    }

    #[test]
    fn radius_and_opacity_accept_their_bounds() {
        let with = |radius: &str, opacity: &str| {
            GRAPHITE.replace(
                "\"cornerRadius\": 8",
                &format!(r#""radius": {radius}, "opacity": {opacity}"#),
            )
        };

        for (radius, opacity, expected_radius, expected_opacity) in
            [("0", "0.4", 0.0, 0.4), ("32", "1", 32.0, 1.0)]
        {
            let theme =
                ThemeTokens::parse(&path(), &with(radius, opacity)).expect("bounds must parse");

            assert!((theme.radius - expected_radius).abs() < f32::EPSILON);
            assert!((theme.opacity - expected_opacity).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn an_out_of_range_numeric_token_falls_back_field_by_field() {
        let text = GRAPHITE.replace(
            "\"cornerRadius\": 8",
            r#""radius": 900, "opacity": 0.2, "barHeight": 20, "dockTile": 80"#,
        );
        let theme = ThemeTokens::parse(&path(), &text).expect("the document is still usable");

        assert!((theme.radius - DEFAULT_RADIUS).abs() < f32::EPSILON);
        assert!((theme.opacity - DEFAULT_OPACITY).abs() < f32::EPSILON);
        assert_eq!(theme.bar_height, None);
        assert_eq!(theme.dock_tile, None);
    }

    #[test]
    fn a_blank_or_absurd_font_falls_back_to_the_default() {
        for font in ["", "   ", "x".repeat(MAX_FONT_NAME + 1).as_str()] {
            let text = GRAPHITE.replace(
                "\"cornerRadius\": 8",
                &format!(r#""cornerRadius": 8, "font": "{font}""#),
            );
            let theme = ThemeTokens::parse(&path(), &text).expect("the theme is still usable");

            assert_eq!(theme.font, DEFAULT_FONT, "font {font:?}");
        }
    }

    #[test]
    fn a_non_boolean_blur_is_off() {
        for blur in ["\"yes\"", "1", "null"] {
            let text = GRAPHITE.replace("8\n}", &format!("8,\n  \"blur\": {blur}\n}}"));
            let theme = ThemeTokens::parse(&path(), &text).expect("parse");

            assert!(!theme.blur, "blur {blur}");
        }
    }

    #[test]
    fn a_colour_that_is_not_rrggbb_is_rejected() {
        for broken in ["#FFF", "0x131416", "#13141G", "", "rgb(1,2,3)"] {
            let text = GRAPHITE.replace("\"#131416\"", &format!("\"{broken}\""));
            match ThemeTokens::parse(&path(), &text) {
                Err(ThemeError::Shape { reason, .. }) => {
                    assert!(reason.contains("RRGGBB"), "{broken}: {reason}");
                }
                other => panic!("'{broken}' must be rejected, got {other:?}"),
            }
        }
    }

    #[test]
    fn a_missing_colour_is_rejected() {
        let text = GRAPHITE.replace("\"textDim\": \"#9AA0A6\",", "");

        assert!(matches!(
            ThemeTokens::parse(&path(), &text),
            Err(ThemeError::Shape { .. })
        ));
    }

    #[test]
    fn broken_json_is_rejected_with_the_parse_error() {
        match ThemeTokens::parse(&path(), "{ nope") {
            Err(ThemeError::Shape { reason, .. }) => assert!(!reason.is_empty()),
            other => panic!("expected a shape error, got {other:?}"),
        }
    }

    #[test]
    fn an_id_falls_back_to_the_file_name() {
        let text = GRAPHITE.replace("\"id\": \"graphite\",", "");

        let theme = ThemeTokens::parse(&path(), &text).expect("a document without an id parses");

        assert_eq!(theme.id, "graphite");
        assert_eq!(theme.name, "Graphite");
    }

    #[test]
    fn a_utf8_bom_does_not_hide_a_valid_theme() {
        let theme = ThemeTokens::parse(&path(), &format!("\u{feff}{GRAPHITE}"))
            .expect("a BOM must not make the file unreadable");

        assert_eq!(theme.id, "graphite");
    }

    #[test]
    fn a_broken_file_falls_back_to_the_built_in_palette() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("graphite.json"), "{ nope").expect("write");

        let theme = ThemeTokens::load_from("graphite", &[dir.path().to_path_buf()], None);

        assert_eq!(theme, ThemeTokens::baked_in());
    }

    #[test]
    fn a_theme_with_a_bad_colour_falls_back_to_the_built_in_palette() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        let broken = GRAPHITE.replace("\"#131416\"", "\"#13141G\"");
        std::fs::write(dir.path().join("graphite.json"), broken).expect("write");

        let theme = ThemeTokens::load_from("graphite", &[dir.path().to_path_buf()], None);

        assert_eq!(theme, ThemeTokens::baked_in());
    }

    #[test]
    fn a_good_file_is_found_by_id_when_its_name_differs() {
        let dir = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(dir.path().join("something-else.json"), GRAPHITE).expect("write");

        let theme = ThemeTokens::load_from("graphite", &[dir.path().to_path_buf()], None);

        assert_eq!(theme.id, "graphite");
        assert_eq!(theme.name, "Graphite");
    }

    #[test]
    fn the_baked_in_palette_is_the_shipped_one() {
        let theme = ThemeTokens::baked_in();

        assert_eq!(theme.id, crate::config::DEFAULT_THEME_ID);
        assert_eq!(theme.background.red, 0x0B);
        assert_eq!(theme.accent.red, 0xA7);
        assert!((theme.radius - 14.0).abs() < f32::EPSILON);
        assert!((theme.opacity - DEFAULT_OPACITY).abs() < f32::EPSILON);
        assert_eq!(theme.font, DEFAULT_FONT);
    }
}
