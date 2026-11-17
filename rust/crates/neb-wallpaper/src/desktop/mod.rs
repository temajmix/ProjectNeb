//! The desktop layer: where the window goes and how it is created.

pub mod attach;
pub mod window;

pub use attach::{AttachError, Slot, SlotKind, WindowTree};
pub use window::{Monitor, RectI, WindowError};
