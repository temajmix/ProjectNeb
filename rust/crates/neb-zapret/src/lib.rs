#![forbid(unsafe_op_in_unsafe_fn)]

//! The ProjectNeb "обход блокировок" module.
//!
//! The module is a manager and an elevated helper in one binary:
//!
//! * [`pack`] finds, validates and imports the user's zapret folder. Nothing third-party is bundled.
//! * [`strategy`] reads `general*.bat` into a program and an argv, without running cmd.
//! * [`helper`] is the elevated half that starts `winws.exe` and writes `status.json`.
//! * [`host`] is the UAC boundary: `IsUserAnAdmin`, `ShellExecuteW(runas)`, pid liveness.
//! * [`manager`] is the non-elevated state machine the `ProjectNeb.zapret` pipe answers from.
//! * [`selftest`] mirrors the pack's own BFE, WinDivert, proxy and hosts diagnostics.
//! * [`ipc`] is the pipe.
//!
//! The manager never needs administrator rights. Only the helper does, and only for as long as
//! `winws.exe` runs.

pub mod helper;
pub mod host;
pub mod ipc;
pub mod manager;
pub mod pack;
pub mod paths;
pub mod selftest;
pub mod settings;
pub mod status;
pub mod strategy;

/// Version reported over IPC and in `--help`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
