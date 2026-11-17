//! The dock: pinned and running applications as a strip of tiles on the bottom edge.
//!
//! The dock is a second kind of window on the same UI thread as the bar, and it follows the same
//! rules: the window procedure owns nothing but its own window's state, and anything that has to
//! happen outside it — a click, a refresh — is recorded in [`crate::window::Pending`] for the message
//! loop to apply between dispatches.
//!
//! Layout of the module:
//!
//! * [`catalog`] — `shell.pinnedApps` turned into launchable entries;
//! * [`running`] — the snapshot of running application windows and the worker that refreshes it;
//! * [`layout`] — how many tiles fit on a monitor, and where the strip goes;
//! * [`autohide`] — when the strip reveals itself and when it goes away;
//! * [`render`] — Direct2D into a layered window's DIB, with every icon and brush cached;
//! * [`window`] — the dock window, its procedure and the messages between it and the application.

pub mod autohide;
pub mod catalog;
pub mod fullscreen;
pub mod layout;
pub mod render;
pub mod running;
pub mod window;

use std::path::{Path, PathBuf};

pub use catalog::AppEntry;
pub use layout::DockPlan;
pub use running::RunningApp;
pub use window::Worker;

/// One tile of the dock: a pinned application, a running one, or both at once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DockItem {
    /// What the tooltip calls it.
    pub name: String,
    /// The executable to launch when the app is not running.
    pub path: Option<PathBuf>,
    /// The window a click brings to the front, as a plain integer.
    pub target: Option<isize>,
    /// The app came from `shell.pinnedApps`.
    pub pinned: bool,
    /// A window of this app was found.
    pub running: bool,
}

/// What a click on a tile asks the application to do.
///
/// It carries no `PathBuf`, because it travels through a `Cell` that has to stay `Copy`: a launch is
/// resolved from the slot index when the click is applied, outside the frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Click {
    /// Bring the window in this slot to the front.
    Activate(isize),
    /// Start the application in this slot, which is pinned but not running.
    Launch(usize),
    /// The `Ещё N` chevron: open the launcher.
    Overflow,
    /// The tile has neither a window nor a path, so there is nothing to do.
    Nothing,
}

/// Builds the tile list: pinned apps first in their configured order, then the running apps that are
/// not pinned yet, one tile per application.
pub fn compose(pinned: &[AppEntry], running: &[RunningApp]) -> Vec<DockItem> {
    let mut items: Vec<DockItem> = Vec::with_capacity(pinned.len() + running.len());
    let mut used: Vec<String> = Vec::with_capacity(pinned.len() + running.len());

    for entry in pinned {
        let key = entry.path.to_string_lossy().to_lowercase();
        let matched = running
            .iter()
            .find(|app| running::key_for(app) == key)
            .map(|app| app.handle);

        used.push(key);
        items.push(DockItem {
            name: entry.name.clone(),
            path: Some(entry.path.clone()),
            target: matched,
            pinned: true,
            running: matched.is_some(),
        });
    }

    for app in running {
        let key = running::key_for(app);
        if used.iter().any(|known| known == &key) {
            continue;
        }

        used.push(key);
        items.push(DockItem {
            name: app.name(),
            path: app.path.clone(),
            target: Some(app.handle),
            pinned: false,
            running: true,
        });
    }

    items
}

/// What a click on `item` does: a running app is brought forward, a pinned app that is not running is
/// started, and a tile with neither does nothing.
pub fn click_for(item: Option<&DockItem>, slot: usize) -> Click {
    match item {
        None => Click::Nothing,
        Some(item) => match (item.target, item.path.as_ref()) {
            (Some(target), _) => Click::Activate(target),
            (None, Some(_)) => Click::Launch(slot),
            (None, None) => Click::Nothing,
        },
    }
}

/// Starts `path` the way Explorer would, so a `.lnk`, an `.exe` and an association all work.
pub fn launch(path: &Path) -> bool {
    use windows::Win32::UI::Shell::ShellExecuteW;
    use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    use windows::core::PCWSTR;

    let file = wide(&path.to_string_lossy());
    let verb = wide("open");

    // SAFETY: both strings are NUL-terminated UTF-16 buffers that outlive the call; the remaining
    // arguments are null or a plain value, and the call only starts a process.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // ShellExecuteW reports success as a value greater than 32, for historical reasons.
    let success = result.0 as isize > 32;
    if success {
        crate::log_info!("dock", "launched '{}'", path.display());
    } else {
        crate::log_warn!(
            "dock",
            "cannot launch '{}' ({})",
            path.display(),
            result.0 as isize
        );
    }

    success
}

/// What the overflow chevron asks for: the launcher, opened with the count as a hint.
///
/// There is no filter yet — the hint is what the log line carries — so this only has to wake the UI
/// thread. Posting the message to the controller (the stable window) rather than opening a window from
/// here keeps the dock's click handler free of anything that can block or re-enter.
pub fn open_launcher(overflow: usize) {
    crate::log_info!(
        "launcher",
        "overflow tile clicked with {overflow} app(s) behind it, opening the launcher"
    );
    crate::window::post_to_control(crate::launcher::WM_LAUNCHER_OPEN, overflow);
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pinned(name: &str, path: &str) -> AppEntry {
        AppEntry {
            name: name.to_owned(),
            path: PathBuf::from(path),
        }
    }

    fn running(app: &str, path: Option<&str>, handle: isize) -> RunningApp {
        RunningApp {
            handle,
            pid: u32::try_from(handle).unwrap_or(1),
            title: format!("{app} - window"),
            path: path.map(PathBuf::from),
            class: app.to_owned(),
        }
    }

    #[test]
    fn pinned_apps_come_first_and_are_marked_running_when_a_window_exists() {
        let pinned_apps = vec![
            pinned("notepad", r"C:\Windows\notepad.exe"),
            pinned("mspaint", r"C:\Windows\mspaint.exe"),
        ];
        let live = vec![running("Notepad", Some(r"C:\WINDOWS\NOTEPAD.EXE"), 0x2000)];

        let items = compose(&pinned_apps, &live);

        assert_eq!(items.len(), 2);
        assert_eq!(items[0].name, "notepad");
        assert!(items[0].pinned && items[0].running);
        assert_eq!(items[0].target, Some(0x2000));
        assert_eq!(items[1].name, "mspaint");
        assert!(items[1].pinned && !items[1].running);
        assert_eq!(items[1].target, None);
    }

    #[test]
    fn a_running_app_that_is_not_pinned_is_appended_once() {
        let live = vec![
            running("Notepad", Some(r"C:\Windows\notepad.exe"), 0x2000),
            running("Notepad", Some(r"C:\Windows\notepad.exe"), 0x2001),
            running("Paint", Some(r"C:\Windows\mspaint.exe"), 0x2002),
        ];

        let items = compose(&[], &live);

        assert_eq!(items.len(), 2, "two Notepads are one tile: {items:?}");
        assert!(items.iter().all(|item| !item.pinned && item.running));
        assert_eq!(
            items[0].target,
            Some(0x2000),
            "the first window of an app wins"
        );
    }

    #[test]
    fn a_window_with_no_path_still_gets_a_tile() {
        let items = compose(&[], &[running("Unknown", None, 0x3000)]);

        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "Unknown - window");
        assert_eq!(items[0].path, None);
    }

    #[test]
    fn a_click_activates_launches_or_does_nothing() {
        let running_item = DockItem {
            name: "notepad".to_owned(),
            path: Some(PathBuf::from(r"C:\Windows\notepad.exe")),
            target: Some(0x2000),
            pinned: true,
            running: true,
        };
        let pinned_item = DockItem {
            target: None,
            running: false,
            ..running_item.clone()
        };
        let empty_item = DockItem {
            path: None,
            ..pinned_item.clone()
        };

        assert_eq!(click_for(Some(&running_item), 0), Click::Activate(0x2000));
        assert_eq!(click_for(Some(&pinned_item), 2), Click::Launch(2));
        assert_eq!(click_for(Some(&empty_item), 1), Click::Nothing);
        assert_eq!(click_for(None, 1), Click::Nothing);
    }
}
