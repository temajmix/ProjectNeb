#![forbid(unsafe_op_in_unsafe_fn)]

//! `neb-updater` — the tiny process that applies an already-downloaded update.
//!
//! The supervisor cannot replace its own files: Windows keeps every loaded image and every open
//! handle locked until the process that owns it is gone. So the supervisor downloads the portable
//! archive into a staging directory, checks its SHA-256, starts this process and exits. This process
//! then
//!
//! 1. waits for the supervisor — and, because the supervisor owns them, for its children — to die;
//! 2. copies every file from the staging directory into the installation directory;
//! 3. starts the supervisor again and exits.
//!
//! Three rules keep the copy safe:
//!
//! - **Nothing is ever deleted.** A file in the installation directory that the archive does not
//!   contain is left alone, so a user's own notes or a stale plugin cannot be destroyed by an update.
//!   `themes/`, `config.json` and `data/` are outside the directory this process writes into anyway.
//! - **A locked file is retried, not skipped.** A dying child may hold a handle for a few hundred
//!   milliseconds after its process object is signalled; a sharing violation or an access denied is
//!   retried up to [`apply::COPY_ATTEMPTS`] times before the update gives up.
//! - **A timeout touches nothing.** When the supervisor is still alive after the timeout the process
//!   exits with code 2, leaving the installation exactly as it was.
//!
//! The binary is a GUI-subsystem program (`windows_subsystem = "windows"`, i.e. WinExe): an update
//! must not flash a console window. It carries no manifest that asks for elevation, so Windows runs it
//! with the invoker's token (`asInvoker`) — replacing files under `%LOCALAPPDATA%` or a portable
//! folder never needs more.
//!
//! Logging borrows [`neb_core::logging::Level`] and [`neb_core::time::UtcDateTime`] so the lines look
//! exactly like the ones every other ProjectNeb process writes; see [`log`] for why the file itself is
//! named by this crate.

pub mod apply;
pub mod args;
pub mod log;
pub mod process;
