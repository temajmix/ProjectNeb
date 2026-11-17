//! The dock's geometry: how many tiles fit on a monitor, and where the strip goes.
//!
//! Everything here is arithmetic on pixels and allocates nothing, so a test can plan a dock for a
//! 4K monitor without a desktop. The window is always exactly as wide as the content it draws — there
//! is no invisible padding a tile could hide in — and the strip is clamped to the monitor so a dock
//! can never be laid out where nobody can click it.
//!
//! Sizes are declared in device-independent pixels and scaled by the tile size the caller asks for,
//! which is how the same plan works at 100 % and at 150 %: `tile` is 56 at 96 DPI and 84 at 144 DPI,
//! and every inset grows with it.

use crate::geometry::{Monitor, Rect};

/// Side of one tile at 100 % scale, in device-independent pixels.
pub const TILE_DIP: i32 = 56;

/// Space between the tiles and the strip's edge.
pub const PADDING_DIP: i32 = 8;

/// Space between two neighbouring tiles.
pub const GAP_DIP: i32 = 6;

/// Thickness of the strip's own border.
pub const BORDER_DIP: i32 = 1;

/// Breathing room left free on each side of the strip.
pub const SCREEN_MARGIN_DIP: i32 = 12;

/// Diameter of the dot under a running application's tile.
pub const DOT_DIP: i32 = 4;

/// Corner radius of the strip.
pub const RADIUS_DIP: f32 = 18.0;

/// Insets derived from the tile size, in physical pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metrics {
    /// Tile side.
    pub tile: i32,
    /// Padding between a tile and the strip's edge.
    pub padding: i32,
    /// Space between two tiles.
    pub gap: i32,
    /// Strip border thickness.
    pub border: i32,
    /// Free space left on each side of the strip.
    pub margin: i32,
    /// Diameter of the running dot.
    pub dot: i32,
}

impl Metrics {
    /// The insets for `tile` physical pixels wide; every other value follows the same scale.
    pub fn for_tile(tile: i32) -> Self {
        let tile = tile.max(1);
        let scale = |dip: i32| -> i32 {
            let scaled = i64::from(dip) * i64::from(tile) + i64::from(TILE_DIP) / 2;
            i32::try_from(scaled / i64::from(TILE_DIP))
                .unwrap_or(i32::MAX)
                .max(1)
        };

        Self {
            tile,
            padding: scale(PADDING_DIP),
            gap: scale(GAP_DIP),
            border: scale(BORDER_DIP),
            margin: scale(SCREEN_MARGIN_DIP),
            dot: scale(DOT_DIP),
        }
    }

    /// Width of `slots` tiles plus the chrome around them, in physical pixels.
    pub fn width(&self, slots: usize) -> i32 {
        let count = i32::try_from(slots).unwrap_or(i32::MAX);
        let gaps = if count > 1 { count - 1 } else { 0 };

        2 * self.border + 2 * self.padding + count * self.tile + gaps * self.gap
    }

    /// Height of a one-row strip, in physical pixels.
    pub fn height(&self) -> i32 {
        2 * self.border + 2 * self.padding + self.tile
    }

    /// The left edge of slot `slot`, in physical pixels from the strip's left edge.
    pub fn slot_left(&self, slot: usize) -> i32 {
        let index = i32::try_from(slot).unwrap_or(i32::MAX);
        self.border + self.padding + index * (self.tile + self.gap)
    }
}

/// What the dock draws for a given number of items on a given monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DockPlan {
    /// Width of the strip in physical pixels: exactly what the window is made.
    pub width: i32,
    /// Height of the strip in physical pixels.
    pub height: i32,
    /// How many items are drawn as their own tile.
    pub visible: usize,
    /// How many items did not fit and are counted by the overflow tile instead.
    pub overflow: usize,
    /// The insets the plan was built with.
    pub metrics: Metrics,
}

impl DockPlan {
    /// Tiles actually drawn: the visible items plus the overflow tile when there is one.
    pub fn slots(&self) -> usize {
        self.visible + usize::from(self.has_overflow())
    }

    /// Whether the last slot is the `Ещё N` chevron.
    pub fn has_overflow(&self) -> bool {
        self.overflow > 0
    }

    /// The item a click at `(x, y)` — client pixels — landed on.
    ///
    /// `None` for the strip's padding, which is deliberately not a hit: a click on the glass should
    /// do nothing rather than activate whatever tile happens to be nearest.
    pub fn slot_at(&self, x: i32, y: i32) -> Option<usize> {
        let metrics = self.metrics;
        let top = metrics.border + metrics.padding;
        let bottom = top + metrics.tile;

        if y < top || y >= bottom {
            return None;
        }

        for slot in 0..self.slots() {
            let left = metrics.slot_left(slot);
            if x >= left && x < left + metrics.tile {
                return Some(slot);
            }
        }

        None
    }
}

/// Plans the dock for `items` tiles on `monitor`, with tiles `tile` physical pixels wide.
///
/// A row that does not fit keeps its full-size tiles and gives up items to the overflow tile. Tiles
/// are never shrunk: a 56 px dock is a dock the user recognises, and growing a second row would eat
/// the desktop the dock is supposed to float above.
pub fn plan(items: usize, monitor: Rect, tile: i32) -> DockPlan {
    let metrics = Metrics::for_tile(tile);
    let available = (monitor.width() - 2 * metrics.margin).max(metrics.width(1));
    let max_slots = max_slots(available, &metrics);

    let (visible, overflow) = if items == 0 {
        (0, 0)
    } else if items <= max_slots {
        (items, 0)
    } else {
        // The chevron takes the last slot, so at least one item is hidden by definition.
        let visible = max_slots.saturating_sub(1);
        (visible, items - visible)
    };

    DockPlan {
        width: metrics.width(visible + usize::from(overflow > 0)),
        height: metrics.height(),
        visible,
        overflow,
        metrics,
    }
}

/// How many tiles of `metrics.tile` fit side by side in `available` pixels.
fn max_slots(available: i32, metrics: &Metrics) -> usize {
    let fixed = 2 * metrics.border + 2 * metrics.padding;
    let numerator = (available - fixed + metrics.gap).max(0);

    usize::try_from(numerator / (metrics.tile + metrics.gap))
        .unwrap_or(1)
        .max(1)
}

/// Where the strip sits: centred on the monitor, resting on the bottom of the work area.
///
/// The work area, not the monitor: the taskbar is allowed to sit below the dock, and when our own bar
/// takes the bottom edge the dock rises with it instead of being buried under it.
pub fn place(monitor: &Monitor, plan: &DockPlan) -> Rect {
    let work = monitor.work;
    let width = plan.width.min(work.width());
    let height = plan.height.min(work.height());
    let left = work.left + (work.width() - width) / 2;
    let top = work.bottom - height;

    Rect::new(left, top, left + width, top + height)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(width: i32, height: i32) -> Monitor {
        Monitor {
            handle: 1,
            device: r"\\.\DISPLAY1".to_owned(),
            rect: Rect::new(0, 0, width, height),
            work: Rect::new(0, 0, width, height - 48),
            dpi: 96,
        }
    }

    #[test]
    fn a_few_items_fit_in_one_row_of_full_size_tiles() {
        let plan = plan(3, monitor(1920, 1080).rect, TILE_DIP);

        assert_eq!(plan.visible, 3);
        assert!(!plan.has_overflow());
        assert_eq!(plan.slots(), 3);
        // 2 border + 2 padding + 3 tiles + 2 gaps.
        assert_eq!(plan.width, 2 + 16 + 3 * 56 + 2 * 6);
        assert_eq!(plan.height, 2 + 16 + 56);
    }

    #[test]
    fn an_empty_dock_is_just_its_chrome() {
        let plan = plan(0, monitor(1920, 1080).rect, TILE_DIP);

        assert_eq!(plan.slots(), 0);
        assert_eq!(plan.width, 2 + 16);
        assert_eq!(plan.height, 74);
    }

    #[test]
    fn a_hundred_and_twenty_items_overflow_on_1366() {
        let plan = plan(120, monitor(1366, 768).rect, TILE_DIP);

        assert_eq!(plan.metrics.tile, 56);
        assert!(plan.has_overflow());
        assert_eq!(
            plan.slots(),
            max_slots(1366 - 24, &plan.metrics),
            "the row is filled to the last whole tile"
        );
        assert_eq!(plan.visible + plan.overflow, 120);
        assert!(
            plan.width <= 1366 - 2 * plan.metrics.margin,
            "the strip must fit between the margins: {} > 1342",
            plan.width
        );
        assert!(plan.visible >= 1);
    }

    #[test]
    fn the_same_list_overflows_later_on_a_wider_monitor() {
        let small = plan(120, monitor(1366, 768).rect, TILE_DIP);
        let medium = plan(120, monitor(1920, 1080).rect, TILE_DIP);
        let large = plan(120, monitor(3840, 2160).rect, TILE_DIP);

        assert!(
            small.slots() < medium.slots() && medium.slots() < large.slots(),
            "{} {} {}",
            small.slots(),
            medium.slots(),
            large.slots()
        );
        assert!(
            large.visible + large.overflow == 120,
            "every item is either drawn or counted"
        );
        for plan in [small, medium, large] {
            assert!(plan.width > 0 && plan.height > 0);
        }
    }

    #[test]
    fn a_four_k_monitor_fits_a_full_row_at_full_size() {
        // 3840 - 2 × 12 of margin leaves room for 61 tiles of 56 px with a 6 px gap.
        let fits = plan(61, monitor(3840, 2160).rect, TILE_DIP);
        let overflows = plan(62, monitor(3840, 2160).rect, TILE_DIP);

        assert!(!fits.has_overflow(), "{fits:?}");
        assert_eq!(fits.visible, 61);
        assert!(overflows.has_overflow(), "{overflows:?}");
        assert_eq!(overflows.visible, 60);
        assert_eq!(overflows.overflow, 2);
        assert!(overflows.width <= 3840 - 2 * overflows.metrics.margin);
    }

    #[test]
    fn a_high_dpi_tile_scales_every_inset_with_it() {
        let metrics = Metrics::for_tile(84); // 56 DIP at 150 %

        assert_eq!(metrics.padding, 12);
        assert_eq!(metrics.gap, 9);
        assert_eq!(metrics.border, 2);
        assert_eq!(metrics.margin, 18);
        assert_eq!(metrics.dot, 6);
        assert_eq!(metrics.height(), 2 * 2 + 2 * 12 + 84);
    }

    #[test]
    fn a_monitor_narrower_than_one_tile_still_plans_a_strip() {
        let plan = plan(5, Rect::new(0, 0, 48, 40), TILE_DIP);

        assert!(plan.width > 0);
        assert_eq!(plan.visible, 0, "nothing fits, so only the chevron does");
        assert_eq!(plan.overflow, 5);
        assert_eq!(plan.slots(), 1);
    }

    #[test]
    fn the_strip_is_centred_and_rests_on_the_work_area() {
        let screen = monitor(1920, 1080);
        let plan = plan(3, screen.rect, TILE_DIP);
        let rect = place(&screen, &plan);

        assert_eq!(rect.bottom, screen.work.bottom, "above the taskbar");
        assert_eq!(rect.width(), plan.width);
        assert_eq!(rect.height(), plan.height);
        assert_eq!(
            rect.left + rect.width() / 2,
            screen.rect.left + screen.rect.width() / 2,
            "centred on the monitor"
        );
    }

    #[test]
    fn a_strip_wider_than_the_work_area_is_clamped_to_it() {
        let screen = Monitor {
            handle: 1,
            device: r"\\.\DISPLAY1".to_owned(),
            rect: Rect::new(0, 0, 1920, 1080),
            work: Rect::new(0, 0, 400, 1032),
            dpi: 96,
        };
        let mut plan = plan(3, screen.rect, TILE_DIP);
        plan.width = 2_000;

        let rect = place(&screen, &plan);

        assert_eq!(rect.width(), 400);
        assert_eq!(rect.left, 0);
    }

    #[test]
    fn a_slot_is_hit_tested_inside_its_tile_only() {
        let plan = plan(3, monitor(1920, 1080).rect, TILE_DIP);
        let metrics = plan.metrics;
        let y = metrics.border + metrics.padding + metrics.tile / 2;

        assert_eq!(plan.slot_at(metrics.slot_left(0) + 1, y), Some(0));
        assert_eq!(
            plan.slot_at(metrics.slot_left(2) + metrics.tile - 1, y),
            Some(2)
        );
        assert_eq!(plan.slot_at(0, y), None, "the padding is not a tile");
        assert_eq!(
            plan.slot_at(metrics.slot_left(1) - 1, y),
            None,
            "the gap between two tiles is not a tile"
        );
        assert_eq!(
            plan.slot_at(metrics.slot_left(0) + 1, 0),
            None,
            "nor is the top edge"
        );
    }

    #[test]
    fn the_overflow_slot_is_the_last_one() {
        let plan = plan(120, monitor(1366, 768).rect, TILE_DIP);
        let metrics = plan.metrics;
        let y = metrics.border + metrics.padding + metrics.tile / 2;
        let last = plan.slots() - 1;

        assert_eq!(plan.slot_at(metrics.slot_left(last) + 4, y), Some(last));
        assert!(
            last >= plan.visible,
            "the chevron is past the visible items"
        );
    }
}
