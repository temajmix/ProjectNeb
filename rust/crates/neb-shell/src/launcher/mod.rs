//! The launcher: `Alt+Space` opens a panel that searches the Start Menu and starts what it finds.
//!
//! The launcher is the third kind of window on the same UI thread as the bar and the dock, and it
//! differs from both in two ways that the whole module is shaped around:
//!
//! * it **takes focus** while it is open, because it has to accept typing, and it closes as soon as
//!   that focus is lost;
//! * its data — several hundred shortcuts — is read on a worker and cached for five minutes, never on
//!   the UI thread and never per frame.
//!
//! Layout of the module:
//!
//! * [`index`] — the Start Menu walk, `.lnk` resolution through `IShellLinkW`, and the five-minute
//!   cache;
//! * [`fuzzy`] — the scoring bands behind the result order;
//! * [`render`] — Direct2D drawing of the panel, with the visible rows' layouts cached;
//! * [`window`] — the window, the native `EDIT` control, the keys and the hotkey.
//!
//! The panel's strings all come from `neb_core::strings`, so the Russian and English shells read the
//! same words as the C# build.

pub mod fuzzy;
pub mod index;
pub mod render;
pub mod window;

use std::path::Path;

use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub use index::Entry;

/// The dock's overflow tile asked for the launcher: `wParam` carries how many apps were behind it.
pub const WM_LAUNCHER_OPEN: u32 = WM_APP + 6;
/// The index worker finished a build; the application adopts it.
pub const WM_LAUNCHER_INDEX: u32 = WM_APP + 7;

/// Starts `path` the way Explorer would, with its own folder as the working directory.
///
/// `ShellExecuteW` is what makes a `.lnk`, an `.exe` and a file association all behave; the working
/// directory matters because an application that loads its data relative to the current folder would
/// otherwise inherit the shell's. A refused launch is a log line, never a panic in the UI thread.
pub fn launch(path: &Path) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    let file = wide(&path.to_string_lossy());
    let verb = wide("open");
    let directory = path
        .parent()
        .map(|parent| wide(&parent.to_string_lossy()))
        .unwrap_or_else(|| vec![0]);

    // SAFETY: every string is a NUL-terminated UTF-16 buffer that outlives the call, the remaining
    // arguments are null or a plain value, and the call only starts a process.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR(directory.as_ptr()),
            SW_SHOWNORMAL,
        )
    };

    // `ShellExecuteW` reports success as a value greater than 32, for historical reasons.
    let success = result.0 as isize > 32;
    if success {
        crate::log_info!("launcher", "launched '{}'", path.display());
    } else {
        crate::log_warn!(
            "launcher",
            "cannot launch '{}' ({})",
            path.display(),
            result.0 as isize
        );
    }

    success
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}
