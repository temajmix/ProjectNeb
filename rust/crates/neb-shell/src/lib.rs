//! `neb-shell` — the resident ProjectNeb bar, drawn with Win32 and Direct2D.
//!
//! One process, one UI thread with an ordinary message loop, and exactly two workers: the metrics
//! sampler and the IPC accept loop. There is no WebView2, no .NET and no GUI framework: a bar is a
//! `WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` popup that reserves its strip through the shell AppBar
//! protocol and paints itself once per `WM_PAINT`.
//!
//! Layout of the crate:
//!
//! * [`app`] — the application: configuration, the bar set, and the window loop;
//! * [`window`] — bar and controller windows, their procedures and the messages between them;
//! * [`dock`] — the dock: pinned and running apps, its window, its icons and its auto-hide;
//! * [`launcher`] — the launcher: the Start Menu index, fuzzy search and the `Alt+Space` panel;
//! * [`icons`] — application icons extracted through the shell and cached as BGRA pixels;
//! * [`appbar`] — reserving and releasing the edge;
//! * [`render`] — Direct2D/DirectWrite, with every brush, format and layout cached;
//! * [`theme`] — reading the palette from the same `themes/*.json` the C# build reads;
//! * [`widgets`] — the clock and the metrics readout, formatted into fixed buffers;
//! * [`metrics_feed`] — the CPU and memory sampler;
//! * [`ipc`] — the `ProjectNeb.shell` pipe;
//! * [`geometry`] — monitors and rectangles in physical pixels.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod app;
pub mod appbar;
pub mod dock;
pub mod geometry;
pub mod icons;
pub mod ipc;
pub mod launcher;
pub mod metrics_feed;
pub mod render;
pub mod theme;
pub mod widgets;
pub mod window;

use std::sync::OnceLock;

use neb_core::logging::LogHandle;

/// Version reported over IPC and on the command line.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static LOG: OnceLock<LogHandle> = OnceLock::new();

/// Makes `handle` the process log. A second call is ignored: the first logger owns the file.
pub fn install_log(handle: Option<LogHandle>) {
    if let Some(handle) = handle {
        let _ = LOG.set(handle);
    }
}

/// The process log, or `None` when logging could not be started. The log macros below are silent in
/// that case rather than turning a logging failure into a crash.
pub fn log() -> Option<&'static LogHandle> {
    LOG.get()
}

/// Writes an info line, formatting only when the log is available and the level is enabled.
#[macro_export]
macro_rules! log_info {
    ($module:expr, $($arg:tt)*) => {{
        if let Some(log) = $crate::log() {
            neb_core::log_info!(log, $module, $($arg)*);
        }
    }};
}

/// Writes a warning line, formatting only when the log is available and the level is enabled.
#[macro_export]
macro_rules! log_warn {
    ($module:expr, $($arg:tt)*) => {{
        if let Some(log) = $crate::log() {
            neb_core::log_warn!(log, $module, $($arg)*);
        }
    }};
}
