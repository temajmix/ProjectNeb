#![forbid(unsafe_op_in_unsafe_fn)]

//! Discord Rich Presence for ProjectNeb.
//!
//! The module is a real IPC client, not a status flag: it opens Discord's named pipe, performs the
//! `op:0` handshake, waits for `READY`, and sends `SET_ACTIVITY` with this process's pid. A worker
//! thread owns the connection so the settings window never blocks on Discord, and the service
//! reconnects with a bounded backoff when the pipe disappears.
//!
//! The layers are separable on purpose:
//!
//! * [`protocol`] — the 8-byte header and JSON `d` of one frame;
//! * [`pipe`] — finding and polling `\\.\pipe\discord-ipc-0..9`;
//! * [`presets`] — validation and the `discord-presets\*.json` store;
//! * [`rules`] — the pure foreground-to-preset decision;
//! * [`foreground`] — the 2-second Win32 sample;
//! * [`activity`] — a preset rendered as Discord's activity object;
//! * [`service`] — the `Disconnected → Connecting → Ready → Error` state machine;
//! * [`ipc`] — the `ProjectNeb.discord` surface the dashboard calls.
//!
//! Nothing here can invent a connected state. With an empty `discord.appId` the state stays
//! `disconnected` and the message is `не настроено`.

pub mod activity;
pub mod foreground;
pub mod ipc;
pub mod pipe;
pub mod presets;
pub mod protocol;
pub mod rules;
pub mod service;

#[cfg(test)]
pub(crate) mod fake;

/// Version reported over IPC and in the log.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
