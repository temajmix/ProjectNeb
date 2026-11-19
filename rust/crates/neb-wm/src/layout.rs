//! The four layouts and the geometry they produce.
//!
//! A layout is a pure function from a monitor work area to one rectangle per tile. Nothing here
//! knows about window handles, DWM or timing, which is what makes every edge case (zero windows, a
//! one-pixel monitor, an eight-pixel gap on a ten-pixel screen) testable without a desktop.
//!
//! # Geometry contract
//!
//! * `outer_gaps` shrinks the work area once, before anything is split;
//! * `gaps` is the exact distance between two neighbouring tiles;
//! * `border` is the inset of every tile, and it is applied after the split, so two neighbours with
//!   a border of one pixel are three pixels apart where they meet the wall and one gap apart
//!   internally;
//! * every returned rectangle lies inside the work area, and no rectangle is ever inside-out. When
//!   the area is too small for the requested gaps the tiles collapse to zero size instead of
//!   becoming negative, so `SetWindowPos` is never handed a nonsense size.

use serde::{Deserialize, Serialize};

use crate::rect::{Direction, Rect};

/// Fraction of the width the master column takes in [`LayoutKind::MasterStack`].
pub const DEFAULT_MASTER_RATIO: f32 = 0.6;

/// The narrowest master column the ratio may ask for.
pub const MIN_MASTER_RATIO: f32 = 0.2;

/// The widest master column the ratio may ask for.
pub const MAX_MASTER_RATIO: f32 = 0.8;

/// Which arrangement the manager applies to the active workspace.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutKind {
    /// Split the largest tile in half, alternating the axis.
    #[default]
    Dwindle,
    /// A tall master column on the left and the other windows stacked on the right.
    MasterStack,
    /// A near-square grid.
    Grid,
    /// One window fills the area; the rest are stacked behind it.
    Monocle,
}

impl LayoutKind {
    /// Every layout, in the order `Alt+T` cycles through them.
    pub const ALL: [Self; 4] = [Self::Dwindle, Self::MasterStack, Self::Grid, Self::Monocle];

    /// Parses the names the configuration and IPC accept; case-insensitive.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "dwindle" => Some(Self::Dwindle),
            "master" | "masterstack" | "master_stack" => Some(Self::MasterStack),
            "grid" => Some(Self::Grid),
            "monocle" => Some(Self::Monocle),
            _ => None,
        }
    }

    /// The name written into configuration and reported over IPC.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dwindle => "dwindle",
            Self::MasterStack => "masterstack",
            Self::Grid => "grid",
            Self::Monocle => "monocle",
        }
    }

    /// The next layout in the `Alt+T` cycle.
    pub const fn next(self) -> Self {
        match self {
            Self::Dwindle => Self::MasterStack,
            Self::MasterStack => Self::Grid,
            Self::Grid => Self::Monocle,
            Self::Monocle => Self::Dwindle,
        }
    }
}

/// The spacing knobs a layout reads.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LayoutParams {
    /// Space between two neighbouring tiles.
    pub gaps: i32,
    /// Space around the whole tiling area.
    pub outer_gaps: i32,
    /// Inset of each individual tile.
    pub border: i32,
    /// Share of the width the master column takes.
    pub master_ratio: f32,
}

impl Default for LayoutParams {
    fn default() -> Self {
        Self {
            gaps: 8,
            outer_gaps: 12,
            border: 1,
            master_ratio: DEFAULT_MASTER_RATIO,
        }
    }
}

impl LayoutParams {
    /// The same parameters with every value in a range the geometry can survive.
    pub fn sanitized(self) -> Self {
        Self {
            gaps: self.gaps.clamp(0, 256),
            outer_gaps: self.outer_gaps.clamp(0, 512),
            border: self.border.clamp(0, 32),
            master_ratio: if self.master_ratio.is_finite() {
                self.master_ratio.clamp(MIN_MASTER_RATIO, MAX_MASTER_RATIO)
            } else {
                DEFAULT_MASTER_RATIO
            },
        }
    }
}

/// One rectangle per tile, in the same order the tiles were given.
///
/// `n` is the number of tiles, not the number of windows: floating windows are excluded by the
/// caller. A zero tile count returns an empty vector for every layout.
pub fn plan(kind: LayoutKind, area: Rect, n: usize, params: LayoutParams) -> Vec<Rect> {
    match kind {
        LayoutKind::Dwindle => dwindle(area, n, params),
        LayoutKind::MasterStack => master_stack(area, n, params),
        LayoutKind::Grid => grid(area, n, params),
        LayoutKind::Monocle => monocle(area, n, params),
    }
}

/// Splits the largest tile in half, alternating horizontal and vertical splits.
pub fn dwindle(area: Rect, n: usize, params: LayoutParams) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }

    let params = params.sanitized();
    let content = content(area, params);
    if n == 1 {
        return vec![content.deflate(params.border, params.border)];
    }

    // (original index, rectangle). The new window always takes the second half of the split, so
    // the insertion order stays visible in the tiling even though the split target jumps around.
    let mut tiles: Vec<(usize, Rect)> = Vec::with_capacity(n);
    tiles.push((0, content));

    for (split_index, index) in (1..n).enumerate() {
        let position = largest(&tiles);
        let (index_of_tile, tile) = tiles[position];
        let split = params.gaps.max(0);
        // The axis alternates with every split, not with the tile that happens to be largest.
        let (keep, fresh) = if split_index % 2 == 0 {
            split_horizontal(tile, 0.5, split)
        } else {
            split_vertical(tile, 0.5, split)
        };

        tiles[position] = (index_of_tile, keep);
        tiles.push((index, fresh));
    }

    tiles.sort_by_key(|(index, _)| *index);
    tiles
        .into_iter()
        .map(|(_, tile)| tile.deflate(params.border, params.border))
        .collect()
}

/// A master column on the left and the remaining windows stacked on the right.
pub fn master_stack(area: Rect, n: usize, params: LayoutParams) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }

    let params = params.sanitized();
    let content = content(area, params);
    if n == 1 {
        return vec![content.deflate(params.border, params.border)];
    }

    let (master, stack) = split_horizontal(content, f64::from(params.master_ratio), params.gaps);
    let mut rects = Vec::with_capacity(n);
    rects.push(master.deflate(params.border, params.border));
    rects.extend(
        split_even_vertical(stack, n - 1, params.gaps)
            .into_iter()
            .map(|tile| tile.deflate(params.border, params.border)),
    );

    rects
}

/// A grid with `ceil(sqrt(n))` columns; the last row is filled left to right.
pub fn grid(area: Rect, n: usize, params: LayoutParams) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }

    let params = params.sanitized();
    let content = content(area, params);
    let columns = ceil_sqrt(n);
    let rows = n.div_ceil(columns);
    let row_rects = split_even_vertical(content, rows, params.gaps);

    let mut rects = Vec::with_capacity(n);
    let mut remaining = n;

    for row in row_rects {
        let cells = remaining.min(columns);
        if cells == 0 {
            break;
        }

        rects.extend(
            split_even_horizontal(row, cells, params.gaps)
                .into_iter()
                .map(|tile| tile.deflate(params.border, params.border)),
        );
        remaining -= cells;
    }

    rects
}

/// Every window on the same rectangle; the focused one is raised by the caller.
pub fn monocle(area: Rect, n: usize, params: LayoutParams) -> Vec<Rect> {
    if n == 0 {
        return Vec::new();
    }

    let params = params.sanitized();
    let tile = content(area, params).deflate(params.border, params.border);

    vec![tile; n]
}

/// The tile a focus or move-arrow should jump to, given the current rectangles.
///
/// The score is `2 * distance along the requested axis + distance across it`, so a tile straight
/// to the right beats one that is far right and far down. Ties go to the earlier tile, which keeps
/// the behaviour deterministic for the tests and for the user.
pub fn neighbour(rects: &[Rect], focused: usize, direction: Direction) -> Option<usize> {
    let origin = rects.get(focused)?;
    let (origin_x, origin_y) = origin.center();

    rects
        .iter()
        .enumerate()
        .filter(|(index, _)| *index != focused)
        .filter_map(|(index, candidate)| {
            let (x, y) = candidate.center();
            let dx = x - origin_x;
            let dy = y - origin_y;

            let (along, across, forward) = match direction {
                Direction::Left => (-dx, dy.abs(), dx < 0),
                Direction::Right => (dx, dy.abs(), dx > 0),
                Direction::Up => (-dy, dx.abs(), dy < 0),
                Direction::Down => (dy, dx.abs(), dy > 0),
            };

            if !forward || along <= 0 {
                return None;
            }

            Some((i64::from(along) * 2 + i64::from(across), index))
        })
        .min_by_key(|(score, index)| (*score, *index))
        .map(|(_, index)| index)
}

/// The work area shrunk by the outer gap.
fn content(area: Rect, params: LayoutParams) -> Rect {
    area.deflate(params.outer_gaps, params.outer_gaps)
}

/// The index of the tile with the largest area; the earliest tile wins a tie.
fn largest(tiles: &[(usize, Rect)]) -> usize {
    let mut best = 0;
    let mut best_area = -1;

    for (position, (_, tile)) in tiles.iter().enumerate() {
        let area = tile.area();
        if area > best_area {
            best = position;
            best_area = area;
        }
    }

    best
}

/// `ceil(sqrt(n))` in integer arithmetic, so a 64-bit float cannot round the wrong way.
fn ceil_sqrt(n: usize) -> usize {
    let mut side = 1;
    while side * side < n {
        side += 1;
    }

    side
}

/// Splits `rect` into a left and a right part, leaving exactly `gap` pixels between them.
fn split_horizontal(rect: Rect, ratio: f64, gap: i32) -> (Rect, Rect) {
    let usable = (rect.width() - gap.max(0)).max(0);
    let left_width = (f64::from(usable) * ratio).round() as i32;
    let edge = rect.left.saturating_add(left_width).min(rect.right);
    let right_start = edge.saturating_add(gap.max(0)).min(rect.right);

    (
        Rect::new(rect.left, rect.top, edge, rect.bottom),
        Rect::new(right_start, rect.top, rect.right, rect.bottom),
    )
}

/// Splits `rect` into a top and a bottom part, leaving exactly `gap` pixels between them.
fn split_vertical(rect: Rect, ratio: f64, gap: i32) -> (Rect, Rect) {
    let usable = (rect.height() - gap.max(0)).max(0);
    let top_height = (f64::from(usable) * ratio).round() as i32;
    let edge = rect.top.saturating_add(top_height).min(rect.bottom);
    let bottom_start = edge.saturating_add(gap.max(0)).min(rect.bottom);

    (
        Rect::new(rect.left, rect.top, rect.right, edge),
        Rect::new(rect.left, bottom_start, rect.right, rect.bottom),
    )
}

/// `count` equally wide columns.
fn split_even_horizontal(rect: Rect, count: usize, gap: i32) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![rect];
    }

    let gap = gap.max(0);
    let usable = (rect.width() - gap * (count as i32 - 1)).max(0);
    let cell = usable / count as i32;

    (0..count)
        .map(|index| {
            let start = rect
                .left
                .saturating_add(index as i32 * (cell + gap))
                .min(rect.right);
            let end = if index + 1 == count {
                rect.right
            } else {
                start.saturating_add(cell).min(rect.right)
            };

            Rect::new(start, rect.top, end, rect.bottom)
        })
        .collect()
}

/// `count` equally tall rows.
fn split_even_vertical(rect: Rect, count: usize, gap: i32) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    if count == 1 {
        return vec![rect];
    }

    let gap = gap.max(0);
    let usable = (rect.height() - gap * (count as i32 - 1)).max(0);
    let cell = usable / count as i32;

    (0..count)
        .map(|index| {
            let start = rect
                .top
                .saturating_add(index as i32 * (cell + gap))
                .min(rect.bottom);
            let end = if index + 1 == count {
                rect.bottom
            } else {
                start.saturating_add(cell).min(rect.bottom)
            };

            Rect::new(rect.left, start, rect.right, end)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCREEN: Rect = Rect::new(0, 0, 1920, 1080);

    fn tight() -> LayoutParams {
        LayoutParams {
            gaps: 0,
            outer_gaps: 0,
            border: 0,
            master_ratio: 0.5,
        }
    }

    fn roomy() -> LayoutParams {
        LayoutParams {
            gaps: 8,
            outer_gaps: 12,
            border: 1,
            master_ratio: 0.6,
        }
    }

    /// Every rectangle must be inside `area`, ordered, and non-negative.
    fn assert_sane(rects: &[Rect], area: Rect, params: LayoutParams) {
        let inner = area.deflate(params.outer_gaps, params.outer_gaps);

        for rect in rects {
            assert!(rect.width() >= 0 && rect.height() >= 0, "{rect:?}");
            assert!(rect.left >= inner.left, "{rect:?} escapes {inner:?}");
            assert!(rect.top >= inner.top, "{rect:?} escapes {inner:?}");
            assert!(rect.right <= inner.right, "{rect:?} escapes {inner:?}");
            assert!(rect.bottom <= inner.bottom, "{rect:?} escapes {inner:?}");
        }
    }

    /// No two tiles may share a pixel; overlapping tiles are what a layout bug looks like on screen.
    fn assert_disjoint(rects: &[Rect]) {
        for (index, left) in rects.iter().enumerate() {
            for right in &rects[index + 1..] {
                let overlaps = left.left < right.right
                    && right.left < left.right
                    && left.top < right.bottom
                    && right.top < left.bottom;
                assert!(!overlaps, "{left:?} overlaps {right:?}");
            }
        }
    }

    #[test]
    fn every_layout_answers_zero_and_one_window_without_a_desktop() {
        for kind in LayoutKind::ALL {
            assert!(plan(kind, SCREEN, 0, roomy()).is_empty(), "{kind:?}");

            let one = plan(kind, SCREEN, 1, roomy());
            assert_eq!(one.len(), 1, "{kind:?}");
            assert_eq!(
                one[0],
                Rect::new(12, 12, 1908, 1068).deflate(1, 1),
                "{kind:?} must fill the inset work area"
            );
        }
    }

    #[test]
    fn every_layout_tiles_two_to_twelve_windows_inside_the_work_area() {
        for kind in LayoutKind::ALL {
            for n in 2..=12 {
                for params in [tight(), roomy()] {
                    let rects = plan(kind, SCREEN, n, params);
                    assert_eq!(rects.len(), n, "{kind:?} n={n}");
                    assert_sane(&rects, SCREEN, params);
                    if kind != LayoutKind::Monocle {
                        assert_disjoint(&rects);
                    }
                }
            }
        }
    }

    #[test]
    fn the_dwindle_split_alternates_the_axis() {
        let rects = dwindle(SCREEN, 2, tight());
        assert_eq!(rects[0], Rect::new(0, 0, 960, 1080));
        assert_eq!(rects[1], Rect::new(960, 0, 1920, 1080));

        let three = dwindle(SCREEN, 3, tight());
        // The second split is vertical and lands on the first half, because the first two tiles
        // are equally large and the earliest one wins the tie.
        assert_eq!(three[0], Rect::new(0, 0, 960, 540));
        assert_eq!(three[1], Rect::new(960, 0, 1920, 1080));
        assert_eq!(three[2], Rect::new(0, 540, 960, 1080));
    }

    #[test]
    fn the_dwindle_gap_is_the_distance_between_two_tiles() {
        let params = LayoutParams {
            gaps: 8,
            outer_gaps: 0,
            border: 0,
            master_ratio: 0.5,
        };
        let rects = dwindle(SCREEN, 2, params);

        assert_eq!(rects[1].left - rects[0].right, 8);
        assert_eq!(rects[0].left, 0);
        assert_eq!(rects[1].right, 1920);
        assert_eq!(rects[0].width(), 956);
    }

    #[test]
    fn the_master_stack_honours_the_ratio_and_stacks_on_the_right() {
        let params = LayoutParams {
            gaps: 0,
            outer_gaps: 0,
            border: 0,
            master_ratio: 0.6,
        };
        let rects = master_stack(Rect::new(0, 0, 1000, 600), 3, params);

        assert_eq!(rects[0], Rect::new(0, 0, 600, 600));
        assert_eq!(rects[1], Rect::new(600, 0, 1000, 300));
        assert_eq!(rects[2], Rect::new(600, 300, 1000, 600));
    }

    #[test]
    fn the_master_ratio_is_clamped_into_a_usable_range() {
        let params = LayoutParams {
            master_ratio: 9.0,
            ..tight()
        };
        let rects = master_stack(Rect::new(0, 0, 1000, 600), 2, params);

        assert_eq!(rects[0].width(), 800, "the master column is at most 80%");
        assert_eq!(rects[1].left, 800);

        let nan = LayoutParams {
            master_ratio: f32::NAN,
            ..tight()
        };
        assert_eq!(
            master_stack(Rect::new(0, 0, 1000, 600), 2, nan)[0].width(),
            600
        );
    }

    #[test]
    fn the_grid_uses_ceil_sqrt_columns_and_fills_the_last_row() {
        let four = grid(SCREEN, 4, tight());
        assert_eq!(four[0], Rect::new(0, 0, 960, 540));
        assert_eq!(four[1], Rect::new(960, 0, 1920, 540));
        assert_eq!(four[2], Rect::new(0, 540, 960, 1080));
        assert_eq!(four[3], Rect::new(960, 540, 1920, 1080));

        let five = grid(SCREEN, 5, tight());
        assert_eq!(five[0].width(), 640, "five windows make three columns");
        assert_eq!(
            five[3],
            Rect::new(0, 540, 960, 1080),
            "the last row has two"
        );
        assert_eq!(five[4], Rect::new(960, 540, 1920, 1080));
    }

    #[test]
    fn monocle_stacks_every_window_on_the_same_rectangle() {
        let rects = monocle(SCREEN, 4, roomy());

        assert_eq!(rects.len(), 4);
        assert!(rects.iter().all(|rect| *rect == rects[0]));
        assert_eq!(rects[0], Rect::new(12, 12, 1908, 1068).deflate(1, 1));
    }

    #[test]
    fn a_weird_aspect_ratio_produces_sane_tiles() {
        let ultrawide = Rect::new(0, 0, 3840, 400);
        let portrait = Rect::new(0, 0, 400, 3840);
        let offset = Rect::new(-1920, -200, 0, 880);

        for area in [ultrawide, portrait, offset] {
            for kind in LayoutKind::ALL {
                for n in 0..=9 {
                    let params = roomy();
                    let rects = plan(kind, area, n, params);
                    assert_eq!(rects.len(), n);
                    assert_sane(&rects, area, params);
                }
            }
        }
    }

    #[test]
    fn a_tiny_area_collapses_to_empty_tiles_instead_of_negative_sizes() {
        let tiny = Rect::new(10, 10, 18, 18);

        for kind in LayoutKind::ALL {
            let rects = plan(kind, tiny, 4, roomy());
            assert_eq!(rects.len(), 4);
            for rect in rects {
                assert!(
                    rect.width() >= 0 && rect.height() >= 0,
                    "{kind:?}: {rect:?}"
                );
            }
        }
    }

    #[test]
    fn a_zero_area_never_panics_or_escapes() {
        let empty = Rect::new(100, 100, 100, 100);

        for kind in LayoutKind::ALL {
            let rects = plan(kind, empty, 5, roomy());
            assert_eq!(rects.len(), 5);
            assert!(rects.iter().all(Rect::is_empty), "{kind:?}: {rects:?}");
        }
    }

    #[test]
    fn the_layout_names_round_trip_and_cycle() {
        for kind in LayoutKind::ALL {
            assert_eq!(LayoutKind::parse(kind.as_str()), Some(kind));
            assert_eq!(kind.next().next().next().next(), kind);
        }

        assert_eq!(
            LayoutKind::parse("MasterStack"),
            Some(LayoutKind::MasterStack)
        );
        assert_eq!(
            LayoutKind::parse("master_stack"),
            Some(LayoutKind::MasterStack)
        );
        assert_eq!(LayoutKind::parse("tall"), None);
    }

    #[test]
    fn focus_moves_to_the_tile_in_the_requested_direction() {
        let rects = grid(SCREEN, 4, tight());

        assert_eq!(neighbour(&rects, 0, Direction::Right), Some(1));
        assert_eq!(neighbour(&rects, 0, Direction::Down), Some(2));
        assert_eq!(neighbour(&rects, 1, Direction::Left), Some(0));
        assert_eq!(neighbour(&rects, 3, Direction::Up), Some(1));
        assert_eq!(neighbour(&rects, 0, Direction::Left), None);
        assert_eq!(neighbour(&rects, 0, Direction::Up), None);
        assert_eq!(neighbour(&[], 0, Direction::Right), None);
    }

    #[test]
    fn focus_prefers_the_tile_aligned_with_the_request_even_in_monocle() {
        // Monocle rectangles all share a centre, so no direction can win.
        let stacked = monocle(SCREEN, 3, tight());
        assert_eq!(neighbour(&stacked, 0, Direction::Right), None);

        // A row of three: right from the middle is the right-hand one, not the far left one.
        let row = vec![
            Rect::new(0, 0, 100, 100),
            Rect::new(100, 0, 200, 100),
            Rect::new(200, 0, 300, 100),
        ];
        assert_eq!(neighbour(&row, 0, Direction::Right), Some(1));
    }

    #[test]
    fn the_plans_are_deterministic() {
        for kind in LayoutKind::ALL {
            let first = plan(kind, SCREEN, 7, roomy());
            let second = plan(kind, SCREEN, 7, roomy());
            assert_eq!(first, second, "{kind:?}");
        }
    }
}
