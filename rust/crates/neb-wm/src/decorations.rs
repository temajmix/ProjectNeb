//! DWM attributes the manager sets on windows it does not own.
//!
//! Windows 11 lets any process ask DWM to round a window, darken its title bar and draw a border
//! around it, through the same public attributes the shell uses. No code is injected and no window
//! procedure is touched; the values simply describe what the compositor should draw.
//!
//! The plan is a plain value so it can be tested without a desktop: [`apply`] produces the values
//! to write when a window is managed, and [`restore`] produces the values that put the window back
//! the way Windows had it when the manager lets go.

use neb_core::theme::Rgb;

/// `DWMWA_WINDOW_CORNER_PREFERENCE`.
pub const ATTR_CORNER: u32 = 33;
/// `DWMWA_USE_IMMERSIVE_DARK_MODE`.
pub const ATTR_DARK: u32 = 20;
/// `DWMWA_BORDER_COLOR`.
pub const ATTR_BORDER: u32 = 34;
/// `DWMWA_TRANSITIONS_FORCEDISABLED`.
pub const ATTR_TRANSITIONS: u32 = 3;

/// `DWMWCP_ROUND`: the Windows 11 rounded corner.
pub const CORNER_ROUND: i32 = 2;
/// `DWMWCP_DONOTROUND`: the square corners a restored window gets.
pub const CORNER_DONOTROUND: i32 = 1;

/// `DWMWA_COLOR_DEFAULT`, which means "let Windows decide".
pub const COLOR_DEFAULT: u32 = 0xFFFF_FFFF;

/// What to write to a window's DWM attributes; `None` leaves the attribute alone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecorationPlan {
    /// `DWMWA_WINDOW_CORNER_PREFERENCE`.
    pub corner: Option<i32>,
    /// `DWMWA_USE_IMMERSIVE_DARK_MODE`.
    pub dark: Option<i32>,
    /// `DWMWA_BORDER_COLOR`, as a `COLORREF`.
    pub border: Option<u32>,
}

impl DecorationPlan {
    /// How many attributes the plan writes.
    pub fn len(&self) -> usize {
        usize::from(self.corner.is_some())
            + usize::from(self.dark.is_some())
            + usize::from(self.border.is_some())
    }

    /// Whether the plan writes nothing.
    pub fn is_empty(&self) -> bool {
        self.corner.is_none() && self.dark.is_none() && self.border.is_none()
    }

    /// The attributes as `(id, value)` pairs, in the order they are written.
    pub fn attributes(&self) -> Vec<(u32, u32)> {
        let mut attributes = Vec::with_capacity(3);
        if let Some(corner) = self.corner {
            attributes.push((ATTR_CORNER, corner as u32));
        }
        if let Some(dark) = self.dark {
            attributes.push((ATTR_DARK, dark as u32));
        }
        if let Some(border) = self.border {
            attributes.push((ATTR_BORDER, border));
        }

        attributes
    }
}

/// The attributes applied to a managed window: rounded, dark title bar and the theme's accent.
pub fn apply(accent: Rgb, no_border: bool) -> DecorationPlan {
    DecorationPlan {
        corner: Some(CORNER_ROUND),
        dark: Some(1),
        border: (!no_border).then(|| colorref(accent)),
    }
}

/// The attributes written when the manager lets go of a window.
pub fn restore() -> DecorationPlan {
    DecorationPlan {
        corner: Some(CORNER_DONOTROUND),
        dark: Some(0),
        border: Some(COLOR_DEFAULT),
    }
}

/// A theme colour as a Win32 `COLORREF` (`0x00BBGGRR`).
pub fn colorref(accent: Rgb) -> u32 {
    (u32::from(accent.blue) << 16) | (u32::from(accent.green) << 8) | u32::from(accent.red)
}

/// What Windows currently reports for the attributes the manager can read back.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DecorationReadback {
    /// `DWMWA_WINDOW_CORNER_PREFERENCE`, or `None` when the query was refused.
    pub corner: Option<i32>,
    /// `DWMWA_USE_IMMERSIVE_DARK_MODE`, or `None` when the query was refused.
    pub dark: Option<i32>,
    /// `DWMWA_BORDER_COLOR`; Windows refuses this query on many windows, so it is usually `None`.
    pub border: Option<u32>,
}

impl DecorationReadback {
    /// The readback as IPC JSON, with `null` for an attribute Windows would not report.
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "corner": self.corner,
            "dark": self.dark,
            "border": self.border,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACCENT: Rgb = Rgb {
        red: 0xA7,
        green: 0x8B,
        blue: 0xFA,
    };

    #[test]
    fn a_managed_window_is_rounded_dark_and_bordered() {
        let plan = apply(ACCENT, false);

        assert_eq!(plan.corner, Some(CORNER_ROUND));
        assert_eq!(plan.dark, Some(1));
        assert_eq!(plan.border, Some(0x00FA_8BA7));
        assert_eq!(plan.len(), 3);
        assert!(!plan.is_empty());
    }

    #[test]
    fn a_no_border_rule_still_rounds_and_darkens_but_leaves_the_border_alone() {
        let plan = apply(ACCENT, true);

        assert_eq!(plan.corner, Some(CORNER_ROUND));
        assert_eq!(plan.dark, Some(1));
        assert_eq!(plan.border, None);
        assert_eq!(plan.len(), 2);
        assert_eq!(
            plan.attributes(),
            vec![(ATTR_CORNER, CORNER_ROUND as u32), (ATTR_DARK, 1)]
        );
    }

    #[test]
    fn restoring_writes_the_neutral_values_including_the_default_border() {
        let plan = restore();

        assert_eq!(plan.corner, Some(CORNER_DONOTROUND));
        assert_eq!(plan.dark, Some(0));
        assert_eq!(plan.border, Some(COLOR_DEFAULT));
        assert_eq!(
            plan.attributes(),
            vec![
                (ATTR_CORNER, CORNER_DONOTROUND as u32),
                (ATTR_DARK, 0),
                (ATTR_BORDER, COLOR_DEFAULT),
            ]
        );
    }

    #[test]
    fn the_attribute_ids_are_the_documented_windows_ones() {
        assert_eq!(ATTR_CORNER, 33);
        assert_eq!(ATTR_DARK, 20);
        assert_eq!(ATTR_BORDER, 34);
        assert_eq!(CORNER_ROUND, 2);
        assert_eq!(CORNER_DONOTROUND, 1);
        assert_eq!(COLOR_DEFAULT, 0xFFFF_FFFF);
    }

    #[test]
    fn a_theme_colour_becomes_a_colourref_in_bgr_order() {
        assert_eq!(
            colorref(Rgb {
                red: 0,
                green: 0,
                blue: 0
            }),
            0
        );
        assert_eq!(
            colorref(Rgb {
                red: 0xFF,
                green: 0xFF,
                blue: 0xFF
            }),
            0x00FF_FFFF
        );
        assert_eq!(
            colorref(Rgb {
                red: 0x12,
                green: 0x34,
                blue: 0x56
            }),
            0x0056_3412
        );
    }

    #[test]
    fn the_readback_serialises_missing_attributes_as_null() {
        let readback = DecorationReadback {
            corner: Some(2),
            dark: Some(1),
            border: None,
        };

        let json = readback.to_json();
        assert_eq!(json["corner"], serde_json::json!(2));
        assert_eq!(json["dark"], serde_json::json!(1));
        assert_eq!(json["border"], serde_json::Value::Null);
    }
}
