//! The bar's palette: [`neb_core::theme::ThemeTokens`] converted to Direct2D colours.
//!
//! The token model, the file search and every validation rule live in `neb-core`, because the
//! settings window reads the same files; this module only maps validated RGB triples to
//! [`D2D1_COLOR_F`] and applies the alphas the bar draws with.
//!
//! Two tokens are drawn at a fixed alpha rather than as written: the bar overlays a translucent
//! surface and hairline on top of the background, so `surface` and `stroke` only contribute their
//! hue. That is what [`SURFACE_ALPHA`] and [`STROKE_ALPHA`] are for, and it is why a bright stroke in
//! a user theme cannot paint a glaring line across the screen. `opacity` scales the background.

use std::path::Path;

use neb_core::logging::LogHandle;
pub use neb_core::theme::ThemeError;
use neb_core::theme::{Rgb, ThemeTokens};
use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

/// Opacity of the bar background when a theme does not name one: the brief asks for 92 %.
pub const BACKGROUND_ALPHA: f32 = 0.92;

/// Opacity of the raised surface fill.
pub const SURFACE_ALPHA: f32 = 0.08;

/// Opacity of the hairline and the bar's bottom border.
pub const STROKE_ALPHA: f32 = 0.10;

/// A palette the bar paints with, already converted to Direct2D colours.
#[derive(Debug, Clone, PartialEq)]
pub struct Theme {
    /// Stable id, i.e. the file name when the JSON omits it.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Bar background, at the theme's `opacity` (92 % when unset).
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
    /// Corner radius in device-independent pixels: the validated `radius` token.
    pub corner_radius: f32,
    /// Background opacity, `0.4..=1.0`.
    pub opacity: f32,
    /// Reserved: whether a future renderer blurs what is behind the bar.
    pub blur: bool,
    /// UI font family.
    pub font: String,
    /// Themed bar height, or `None` when the config's `barHeight` decides.
    pub bar_height: Option<i32>,
    /// Themed dock tile side in DIPs, or `None` for the shipped default.
    pub dock_tile: Option<i32>,
}

impl Theme {
    /// The palette the C# build ships as `liquid-glass`, used when no file answers.
    pub fn baked_in() -> Self {
        Self::from_tokens(ThemeTokens::baked_in())
    }

    /// The theme called `id`, or the closest thing that exists: a broken file is logged and skipped.
    pub fn load(id: &str, log: Option<&LogHandle>) -> Self {
        Self::from_tokens(ThemeTokens::load(id, log))
    }

    /// Validates a theme document without touching a disk.
    ///
    /// # Errors
    ///
    /// The [`ThemeError`] the core parser reports.
    pub fn parse(path: &Path, text: &str) -> Result<Self, ThemeError> {
        ThemeTokens::parse(path, text).map(Self::from_tokens)
    }

    /// Maps a validated token set onto the colours Direct2D draws with.
    pub fn from_tokens(tokens: ThemeTokens) -> Self {
        Self {
            id: tokens.id,
            name: tokens.name,
            background: with_alpha(tokens.background, tokens.opacity),
            surface: with_alpha(Rgb::WHITE, SURFACE_ALPHA),
            stroke: with_alpha(tokens.stroke, STROKE_ALPHA),
            text: with_alpha(tokens.text, 1.0),
            text_dim: with_alpha(tokens.text_dim, 1.0),
            accent: with_alpha(tokens.accent, 1.0),
            accent2: with_alpha(tokens.accent2, 1.0),
            corner_radius: tokens.radius,
            opacity: tokens.opacity,
            blur: tokens.blur,
            font: tokens.font,
            bar_height: tokens.bar_height,
            dock_tile: tokens.dock_tile,
        }
    }
}

/// Applies an alpha to an 8-bit RGB triple.
fn with_alpha(colour: Rgb, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: f32::from(colour.red) / 255.0,
        g: f32::from(colour.green) / 255.0,
        b: f32::from(colour.blue) / 255.0,
        a: alpha.clamp(0.0, 1.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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
        let theme = Theme::parse(&path(), GRAPHITE).expect("graphite must parse");

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

        let theme = Theme::parse(&shipped, &text).expect("liquid-glass must parse");

        assert_eq!(theme.id, neb_core::config::DEFAULT_THEME_ID);
        assert_eq!(theme.name, "Liquid Glass");
    }

    #[test]
    fn the_opacity_token_sets_the_background_alpha() {
        let text = GRAPHITE.replace(
            "\"cornerRadius\": 8",
            r#""cornerRadius": 8, "opacity": 0.7"#,
        );
        let theme = Theme::parse(&path(), &text).expect("parse");

        assert!((theme.opacity - 0.7).abs() < 1e-6);
        assert!((theme.background.a - 0.7).abs() < 1e-6);
    }

    #[test]
    fn the_v2_tokens_reach_the_shell_palette() {
        let text = GRAPHITE.replace(
            "\"cornerRadius\": 8",
            r##""radius": 20, "blur": true, "font": "Cascadia Mono",
                "barHeight": 48, "dockTile": 44"##,
        );
        let theme = Theme::parse(&path(), &text).expect("parse");

        assert!((theme.corner_radius - 20.0).abs() < f32::EPSILON);
        assert!(theme.blur);
        assert_eq!(theme.font, "Cascadia Mono");
        assert_eq!(theme.bar_height, Some(48));
        assert_eq!(theme.dock_tile, Some(44));
    }

    #[test]
    fn a_colour_that_is_not_rrggbb_is_rejected() {
        for broken in ["#FFF", "0x131416", "#13141G", "", "rgb(1,2,3)"] {
            let text = GRAPHITE.replace("\"#131416\"", &format!("\"{broken}\""));
            match Theme::parse(&path(), &text) {
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
            Theme::parse(&path(), &text),
            Err(ThemeError::Shape { .. })
        ));
    }

    #[test]
    fn broken_json_is_rejected_with_the_parse_error() {
        match Theme::parse(&path(), "{ nope") {
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
            let theme = Theme::parse(&path(), &text).expect("parse");

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
        assert_eq!(theme.font, neb_core::theme::DEFAULT_FONT);
    }
}
