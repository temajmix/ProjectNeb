#![forbid(unsafe_op_in_unsafe_fn)]

//! The ProjectNeb living wallpaper.
//!
//! One process, one D3D11 device, one window per monitor, parented into the desktop's own
//! live-wallpaper slot so the scene draws **behind** the icons. No WebView2, no .NET, no async
//! runtime: the scene is a single fullscreen triangle and a pixel shader, and the frame clock is a
//! wait in the message loop.
//!
//! The modules follow the layers a bug can hide in:
//!
//! * [`desktop`] — where the window goes: the `WorkerW` search (written against an injectable tree
//!   so every Explorer layout is a unit test) and the Win32 window plumbing.
//! * [`render`] — what it shows: the scene maths (pure, testable) and the D3D11 half.
//! * [`power`] — when it stops: fullscreen, battery and locked-session rules.
//! * [`ipc`] — the `ProjectNeb.wallpaper` pipe the dashboard talks to.
//! * [`app`] — the message loop that owns all of the above.
//! * [`config`] — the wallpaper slice of `config.json`.

pub mod app;
pub mod config;
pub mod desktop;
pub mod ipc;
pub mod power;
pub mod render;

/// Version reported over IPC, for a client that wants to know what answered.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
