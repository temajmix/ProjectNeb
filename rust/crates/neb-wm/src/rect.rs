//! Integer rectangles and the directions the manager reasons about.
//!
//! Everything here is pure: the layouts, the animation interpolation and the tests all work on
//! [`Rect`] values, and only [`crate::win`] ever converts one into a `WIN32 RECT`.

use serde::{Deserialize, Serialize};

/// An integer rectangle in physical pixels: `left`/`top` inclusive, `right`/`bottom` exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    /// Left edge.
    pub left: i32,
    /// Top edge.
    pub top: i32,
    /// Right edge, exclusive.
    pub right: i32,
    /// Bottom edge, exclusive.
    pub bottom: i32,
}

impl Rect {
    /// A rectangle from its four edges.
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// A rectangle from its origin and size, with a size of at least one pixel.
    pub fn from_size(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self::new(x, y, x + width.max(1), y + height.max(1))
    }

    /// Width in pixels; never negative, so a degenerate rectangle reads as zero wide.
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(0)
    }

    /// Height in pixels; never negative.
    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(0)
    }

    /// Whether the rectangle has no pixels at all.
    pub fn is_empty(&self) -> bool {
        self.width() == 0 || self.height() == 0
    }

    /// Area in square pixels, computed in 64 bits so a giant rectangle cannot overflow.
    pub fn area(&self) -> i64 {
        i64::from(self.width()) * i64::from(self.height())
    }

    /// Whether `point` is inside the half-open rectangle.
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    /// The centre of the rectangle.
    pub fn center(&self) -> (i32, i32) {
        (self.left + self.width() / 2, self.top + self.height() / 2)
    }

    /// A copy shrunk by `dx` horizontally and `dy` vertically.
    ///
    /// A rectangle can never become inside-out: if the inset consumes it, both edges meet and the
    /// result is empty. This is what keeps a tiny monitor or a large gap from producing negative
    /// widths that `SetWindowPos` would reject.
    pub fn deflate(&self, dx: i32, dy: i32) -> Self {
        let dx = dx.max(0);
        let dy = dy.max(0);
        let left = self.left.saturating_add(dx);
        let top = self.top.saturating_add(dy);
        let right = self.right.saturating_sub(dx).max(left);
        let bottom = self.bottom.saturating_sub(dy).max(top);

        Self::new(left, top, right, bottom)
    }

    /// How far apart two rectangles are, in pixels: the largest difference of any edge.
    ///
    /// The manager uses this to decide whether a move is worth animating at all.
    pub fn distance(&self, other: &Self) -> i32 {
        (self.left - other.left)
            .abs()
            .max((self.top - other.top).abs())
            .max((self.right - other.right).abs())
            .max((self.bottom - other.bottom).abs())
    }

    /// Linear interpolation between `from` and `to`; `t` is clamped into `0.0..=1.0`.
    pub fn lerp(from: &Self, to: &Self, t: f64) -> Self {
        let t = t.clamp(0.0, 1.0);
        let blend = |start: i32, end: i32| {
            let value = f64::from(start) + (f64::from(end) - f64::from(start)) * t;
            value.round() as i32
        };

        Self::new(
            blend(from.left, to.left),
            blend(from.top, to.top),
            blend(from.right, to.right),
            blend(from.bottom, to.bottom),
        )
    }

    /// The same rectangle as the Win32 structure.
    pub const fn to_win32(self) -> windows::Win32::Foundation::RECT {
        windows::Win32::Foundation::RECT {
            left: self.left,
            top: self.top,
            right: self.right,
            bottom: self.bottom,
        }
    }

    /// The same rectangle from the Win32 structure.
    pub const fn from_win32(rect: windows::Win32::Foundation::RECT) -> Self {
        Self::new(rect.left, rect.top, rect.right, rect.bottom)
    }

    /// Serialises as `{"left":..,"top":..,"width":..,"height":..}` for IPC.
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({
            "left": self.left,
            "top": self.top,
            "width": self.width(),
            "height": self.height(),
        })
    }
}

/// A direction a focus or move hotkey asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Towards smaller x.
    Left,
    /// Towards larger x.
    Right,
    /// Towards smaller y.
    Up,
    /// Towards larger y.
    Down,
}

impl Direction {
    /// The direction an arrow virtual-key code names, for the hotkey table.
    pub const fn from_vk(vk: u32) -> Option<Self> {
        match vk {
            0x25 => Some(Self::Left),
            0x26 => Some(Self::Up),
            0x27 => Some(Self::Right),
            0x28 => Some(Self::Down),
            _ => None,
        }
    }

    /// The opposite direction, i.e. where focus came from.
    pub const fn opposite(self) -> Self {
        match self {
            Self::Left => Self::Right,
            Self::Right => Self::Left,
            Self::Up => Self::Down,
            Self::Down => Self::Up,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_are_never_negative() {
        let inside_out = Rect::new(10, 10, 5, 5);

        assert_eq!(inside_out.width(), 0);
        assert_eq!(inside_out.height(), 0);
        assert!(inside_out.is_empty());
        assert_eq!(inside_out.area(), 0);
    }

    #[test]
    fn deflating_past_the_edges_leaves_an_empty_rectangle() {
        let rect = Rect::new(0, 0, 10, 10);

        assert_eq!(rect.deflate(2, 3), Rect::new(2, 3, 8, 7));
        assert_eq!(rect.deflate(50, 50), Rect::new(50, 50, 50, 50));
        assert!(rect.deflate(50, 50).is_empty());
        assert_eq!(rect.deflate(-3, -3), rect, "a negative inset is no inset");
    }

    #[test]
    fn distance_is_the_largest_edge_delta() {
        let a = Rect::new(0, 0, 100, 100);

        assert_eq!(a.distance(&a), 0);
        assert_eq!(a.distance(&Rect::new(0, 0, 101, 100)), 1);
        assert_eq!(a.distance(&Rect::new(-4, 3, 100, 90)), 10);
    }

    #[test]
    fn interpolation_hits_both_ends_exactly() {
        let from = Rect::new(0, 0, 100, 100);
        let to = Rect::new(100, 50, 300, 250);

        assert_eq!(Rect::lerp(&from, &to, 0.0), from);
        assert_eq!(Rect::lerp(&from, &to, 1.0), to);
        assert_eq!(Rect::lerp(&from, &to, 0.5), Rect::new(50, 25, 200, 175));
        assert_eq!(Rect::lerp(&from, &to, 9.0), to, "t is clamped");
    }

    #[test]
    fn containment_uses_half_open_edges() {
        let rect = Rect::new(10, 10, 20, 20);

        assert!(rect.contains(10, 10));
        assert!(rect.contains(19, 19));
        assert!(
            !rect.contains(20, 20),
            "the right and bottom edges are outside"
        );
        assert_eq!(rect.center(), (15, 15));
    }

    #[test]
    fn a_rect_round_trips_through_the_win32_shape() {
        let rect = Rect::new(-1920, 0, 0, 1080);

        assert_eq!(Rect::from_win32(rect.to_win32()), rect);
    }

    #[test]
    fn every_arrow_key_has_a_direction() {
        assert_eq!(Direction::from_vk(0x25), Some(Direction::Left));
        assert_eq!(Direction::from_vk(0x26), Some(Direction::Up));
        assert_eq!(Direction::from_vk(0x27), Some(Direction::Right));
        assert_eq!(Direction::from_vk(0x28), Some(Direction::Down));
        assert_eq!(Direction::from_vk(0x41), None);
        assert_eq!(Direction::Left.opposite(), Direction::Right);
    }
}
