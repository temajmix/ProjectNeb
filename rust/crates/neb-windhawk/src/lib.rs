#![forbid(unsafe_op_in_unsafe_fn)]

//! The ProjectNeb Windhawk integration.
//!
//! Windhawk is a third-party program; ProjectNeb does not reimplement it. This crate is the
//! manager and its elevated helper in one binary:
//!
//! * [`detect`] finds the installation and the data directory it names;
//! * [`catalog`] fetches GitHub's listing of `windhawk-mods/mods` and caches it for six hours;
//! * [`installed`] reads sources, compiled DLLs and the registry's enabled state;
//! * [`plan`] is the validated install/remove plan and its result;
//! * [`helper`] is the elevated half that writes `ModsSource` and proves the enable write;
//! * [`registry`] owns `HKLM\SOFTWARE\Windhawk\Engine\Mods\<id>\Disabled`;
//! * [`host`] is the UAC/network boundary;
//! * [`manager`] is the state machine the `ProjectNeb.windhawk` pipe answers from;
//! * [`ipc`] is the pipe.
//!
//! The manager never needs administrator rights. Only the helper does, and only for the one write
//! that needs it.

pub mod catalog;
pub mod detect;
pub mod helper;
pub mod host;
pub mod http;
pub mod installed;
pub mod ipc;
pub mod manager;
pub mod paths;
pub mod plan;
pub mod registry;

/// Version reported over IPC and in `--help`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
