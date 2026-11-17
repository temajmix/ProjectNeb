//! Theme tokens for the bar, read from the same JSON files the C# shell reads.
//!
//! The contract is `Neb.Core.Theming.ThemeStore`/`ThemeTokens`: `themes/<id>.json` with seven `#RRGGBB`
//! colours, an id, a name and a corner radius. A file that is unreadable, unparsable or carries a
//! colour in any other spelling is skipped with a warning — one bad theme must never cost the user the
//! bar — and [`Theme::baked_in`] is always the fallback.
//!
//! Two of the tokens are drawn at fixed alphas rather than as written: the bar overlays a translucent
//! surface and hairline on top of the background, so `surface` and `stroke` only contribute their hue.
//! That is what [`SURFACE_ALPHA`] and [`STROKE_ALPHA`] are for, and it is why a bright stroke in a
//! user theme cannot paint a glaring line across the screen.

use std::fs;
use std::path::PathBuf;

use neb_core::config::DEFAULT_THEME_ID;
use neb_core::logging::LogHandle;
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

/// Opacity of the bar background: the brief asks for the theme colour at 92 %.
pub const BACKGROUND_ALPHA: f32 = 0.92;

/// Opacity of the raised surface fill.
pub const SURFACE_ALPHA: f32 = 0.08;

/// Opacity of the hairline and the bar's bottom border.
pub const STROKE_ALPHA: f32 = 0.10;

/// A palette the bar paints with, already converted to Direct2D colours.
///
/// All seven colour tokens are parsed and validated, because a theme file has to stay valid for the C#
/// build that shares the folder. The bar itself draws four of them: [`background`](Self::background),
/// [`stroke`](Self::stroke) as the bottom hairline, [`text`](Self::text) and
/// [`text_dim`](Self::text_dim). `surface`, `accent` and `accent2` are carried for the settings window
/// and the dock, which are later slices; nothing in this binary fills with them yet.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Stable id, i.e. the file name when the JSON omits it.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Bar background, at [`BACKGROUND_ALPHA`].
    pub background: D2D1_COLOR_F,
    /// Raised fill, white at [`SURFACE_ALPHA`].
    pub surface: D2D1_COLOR_F,
    /// Hairline and bottom border, the theme's stroke hue at [`STROKE_ALPHA`].
    pub stroke: D2D1_COLOR_F,
    /// Primary text.
    pub text: D2D1_COLOR_F,
    /// Secondary text: date, profile name, metrics.
    pub text_dim: D2D1_COLOR_F,
    /// Primary highlight.
    pub accent: D2D1_COLOR_F,
    /// Secondary highlight, reserved for the parts of the shell that arrive later.
    pub accent2: D2D1_COLOR_F,
    /// Corner radius in device-independent pixels.
    pub corner_radius: f32,
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
    /// The file is not the JSON object `ThemeStore` expects.
    #[error("'{}' is not a theme document: {reason}", path.display())]
    Shape {
        /// File that was tried.
        path: PathBuf,
        /// What was wrong, in words a log line can carry.
        reason: String,
    },
}

impl Theme {
    /// The palette the C# build ships as `liquid-glass`, used when no file answers.
    pub fn baked_in() -> Self {
        Self {
            id: DEFAULT_THEME_ID.to_owned(),
            name: "Liquid Glass".to_owned(),
            background: with_alpha((0x0B, 0x0A, 0x12), BACKGROUND_ALPHA),
            surface: with_alpha((0xFF, 0xFF, 0xFF), SURFACE_ALPHA),
            stroke: with_alpha((0x2B, 0x25, 0x44), STROKE_ALPHA),
            text: with_alpha((0xF6, 0xF4, 0xFF), 1.0),
            text_dim: with_alpha((0xB7, 0xB3, 0xC9), 1.0),
            accent: with_alpha((0xA7, 0x8B, 0xFA), 1.0),
            accent2: with_alpha((0x7C, 0x5C, 0xFF), 1.0),
            corner_radius: 14.0,
        }
    }

    /// The theme called `id`, or the closest thing that exists.
    ///
    /// Lookup order: `%LocalAppData%\ProjectNeb\themes`, then a `themes` folder next to the executable
    /// (where the packaged build ships them), then the baked-in palette. A broken file is logged and
    /// stepped over, exactly like the C# store.
    pub fn load(id: &str, log: Option<&LogHandle>) -> Self {
        for dir in theme_dirs() {
            let direct = dir.join(format!("{id}.json"));
            if direct.is_file() {
                match parse_file(&direct) {
                    Ok(theme) => return theme,
                    Err(error) => warn(log, &format!("{error}, skipped")),
                }
            }

            if let Some(theme) = scan_for_id(&dir, id, log) {
                return theme;
            }
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

/// `%LocalAppData%\ProjectNeb\themes`, then `<exe>\themes`; whichever exists first wins.
fn theme_dirs() -> Vec<PathBuf> {
    let mut dirs = vec![neb_core::paths::themes_dir()];

    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        dirs.push(parent.join("themes"));
    }

    dirs.retain(|dir| dir.is_dir());
    dirs
}

/// Finds the file in `dir` whose `id` field is `id`, for a theme whose file name is not its id.
fn scan_for_id(dir: &std::path::Path, id: &str, log: Option<&LogHandle>) -> Option<Theme> {
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
fn parse_file(path: &std::path::Path) -> Result<Theme, ThemeError> {
    let text = fs::read_to_string(path).map_err(|source| ThemeError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    parse(path, &text)
}

/// Validates a theme document. Split out so the tests can feed it text without touching a disk.
fn parse(path: &std::path::Path, text: &str) -> Result<Theme, ThemeError> {
    let document: serde_json::Value =
        serde_json::from_str(text).map_err(|error| ThemeError::Shape {
            path: path.to_path_buf(),
            reason: error.to_string(),
        })?;

    let field = |name: &str| document.get(name).and_then(serde_json::Value::as_str);

    // The C# store validates all seven colours; doing the same means a file this build accepts is a
    // file the other build accepts too.
    let background = colour(path, field("background"))?;
    let surface = colour(path, field("surface"))?;
    let stroke = colour(path, field("stroke"))?;
    let text = colour(path, field("text"))?;
    let text_dim = colour(path, field("textDim"))?;
    let accent = colour(path, field("accent"))?;
    let accent2 = colour(path, field("accent2"))?;

    let id = field("id")
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .or_else(|| path.file_stem().and_then(|stem| stem.to_str()))
        .ok_or_else(|| ThemeError::Shape {
            path: path.to_path_buf(),
            reason: "no usable id".to_owned(),
        })?;

    let corner_radius = document
        .get("cornerRadius")
        .and_then(serde_json::Value::as_f64)
        .filter(|radius| radius.is_finite() && (0.0..=64.0).contains(radius))
        .map_or(12.0, |radius| radius as f32);

    Ok(Theme {
        id: id.to_owned(),
        name: field("name")
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .unwrap_or(id)
            .to_owned(),
        background: with_alpha(background, BACKGROUND_ALPHA),
        surface: with_alpha(surface, SURFACE_ALPHA),
        stroke: with_alpha(stroke, STROKE_ALPHA),
        text: with_alpha(text, 1.0),
        text_dim: with_alpha(text_dim, 1.0),
        accent: with_alpha(accent, 1.0),
        accent2: with_alpha(accent2, 1.0),
        corner_radius,
    })
}

/// Accepts exactly `#RRGGBB`, the only spelling both builds accept.
fn colour(path: &std::path::Path, value: Option<&str>) -> Result<(u8, u8, u8), ThemeError> {
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

    Ok((channel(0)?, channel(2)?, channel(4)?))
}

/// Applies an alpha to an 8-bit RGB triple.
fn with_alpha((red, green, blue): (u8, u8, u8), alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(red) / 255.0,
        g: f32::from(green) / 255.0,
        b: f32::from(blue) / 255.0,
        a: alpha,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

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
    fn a_shipped_theme_file_parses() {
        let theme = parse(&path(), GRAPHITE).expect("graphite must parse");

        assert_eq!(theme.id, "graphite");
        assert_eq!(theme.name, "Graphite");
        assert_eq!(theme.background.r, 0x13 as f32 / 255.0);
        assert!((theme.background.a - BACKGROUND_ALPHA).abs() < f32::EPSILON);
        assert!((theme.surface.a - SURFACE_ALPHA).abs() < f32::EPSILON);
        assert!((theme.stroke.a - STROKE_ALPHA).abs() < f32::EPSILON);
        assert!((theme.text.a - 1.0).abs() < f32::EPSILON);
        assert!((theme.corner_radius - 8.0).abs() < f32::EPSILON);
    }

    #[test]
    fn the_theme_shipped_next_to_the_c_sharp_app_parses() {
        let shipped = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../src/Neb.App/themes/liquid-glass.json");
        let text = std::fs::read_to_string(&shipped).expect("the shipped theme must be readable");

        let theme = parse(&shipped, &text).expect("liquid-glass must parse");

        assert_eq!(theme.id, DEFAULT_THEME_ID);
        assert_eq!(theme.name, "Liquid Glass");
    }

    #[test]
    fn an_id_falls_back_to_the_file_name() {
        let text = r##"{
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

        let theme = parse(&path(), text).expect("a document without an id still parses");

        assert_eq!(theme.id, "graphite");
        assert_eq!(theme.name, "Graphite");
    }

    #[test]
    fn a_colour_that_is_not_rrggbb_is_rejected() {
        for broken in ["#FFF", "0x131416", "#13141G", "", "rgb(1,2,3)"] {
            let text = GRAPHITE.replace("\"#131416\"", &format!("\"{broken}\""));
            match parse(&path(), &text) {
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
            parse(&path(), &text),
            Err(ThemeError::Shape { .. })
        ));
    }

    #[test]
    fn broken_json_is_rejected_with_the_parse_error() {
        match parse(&path(), "{ nope") {
            Err(ThemeError::Shape { reason, .. }) => assert!(!reason.is_empty()),
            other => panic!("expected a shape error, got {other:?}"),
        }
    }

    #[test]
    fn an_out_of_range_corner_radius_falls_back_to_twelve() {
        for radius in ["900", "-4", "\"x\""] {
            let text = GRAPHITE.replace(
                "\"cornerRadius\": 8",
                &format!("\"cornerRadius\": {radius}"),
            );
            let theme = parse(&path(), &text).expect("parse");

            assert!(
                (theme.corner_radius - 12.0).abs() < f32::EPSILON,
                "{radius}"
            );
        }
    }

    #[test]
    fn the_baked_in_palette_is_the_shipped_one() {
        let theme = Theme::baked_in();

        assert_eq!(theme.id, "liquid-glass");
        assert!((theme.background.r - 0x0B as f32 / 255.0).abs() < 1e-6);
        assert!((theme.accent.r - 0xA7 as f32 / 255.0).abs() < 1e-6);
        assert!((theme.corner_radius - 14.0).abs() < f32::EPSILON);
    }
}
