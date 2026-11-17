//! The scene layer.
//!
//! [`scene`] is pure arithmetic - where the blobs are, what colour they are, how long a loop lasts -
//! so it can be tested without a GPU. [`d3d11`] is the only module in this crate that forwards to
//! D3D11, and it does exactly one thing per frame: map the constant buffer, draw one triangle,
//! present.

pub mod d3d11;
pub mod scene;

pub use d3d11::{RenderError, Renderer};
pub use scene::{Frame, Palette};
