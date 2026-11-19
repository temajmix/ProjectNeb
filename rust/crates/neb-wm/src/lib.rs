//! `neb-wm` — ProjectNeb's tiling window manager.
//!
//! The manager follows windows through the public `SetWinEventHook` mechanism, keeps its own model
//! of them per workspace, lays the tiles out with four pure layout functions, and applies the
//! result with `SetWindowPos`. It never injects code into another process and never subclasses a
//! window; decorations are the documented DWM attributes Windows 11 exposes to every process.
//!
//! * [`layout`] — Dwindle, MasterStack, Grid and Monocle as pure geometry.
//! * [`model`] — managed windows, virtual workspaces and focus, with no Win32 dependency.
//! * [`engine`] — policy and the [`engine::WindowOps`] seam that makes refusal handling testable.
//! * [`animation`] — the 8 ms stepping plan, capped at 180 ms.
//! * [`decorations`] — the rounded/dark/accent plan and its neutral restore.
//! * [`rules`] — compiled per-application rules, first match wins.
//! * [`fullscreen`] — the fullscreen exclusion shared with the dock's rule.
//! * [`win`] — the real Win32 implementation of the seam.
//! * [`app`] — the resident message loop, hooks, hotkeys and shutdown.
//! * [`ipc`] — the `ProjectNeb.wm` methods.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod animation;
pub mod app;
pub mod decorations;
pub mod engine;
pub mod fullscreen;
pub mod ipc;
pub mod layout;
pub mod model;
pub mod rect;
pub mod rules;
pub mod win;

pub use engine::{AnimationReport, Engine, Refusal, WindowOps};
pub use layout::{LayoutKind, LayoutParams};
pub use model::{Window, WindowId, WorkspaceModel};
pub use rect::Rect;
