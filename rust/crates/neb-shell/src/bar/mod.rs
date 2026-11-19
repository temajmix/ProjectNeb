//! The configurable bar: typed widget specs, their text, and where they go.

pub mod layout;
pub mod widgets;

pub use layout::{Measured, Placement, SubPlacement, place};
pub use widgets::{
    Align, ClockReading, Skipped, WidgetKind, WidgetSpec, WidgetText, format_clock, parse,
};
