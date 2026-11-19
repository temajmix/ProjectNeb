#![forbid(unsafe_op_in_unsafe_fn)]

//! Shared plumbing for the ProjectNeb processes.
//!
//! The crate is deliberately synchronous: every ProjectNeb process runs a plain message loop, so
//! nothing here may pull in an async runtime. Modules arrive task by task — this first slice carries
//! the on-disk contract for paths.

pub mod config;
pub mod ipc;
pub mod license;
pub mod logging;
pub mod metrics;
pub mod paths;
pub mod strings;
pub mod theme;
pub mod theming;
pub mod time;
pub mod windows_mode;
