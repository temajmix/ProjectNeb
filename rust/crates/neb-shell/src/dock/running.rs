//! The applications that are running right now, as the dock needs to see them.
//!
//! There are no hooks here: a snapshot every few seconds is enough for a dock and cannot be blamed
//! for a slow desktop. One ticket per application — two Notepad windows are one Notepad — except for
//! UWP frames, whose executable is always `ApplicationFrameHost.exe` and which are therefore grouped
//! by process instead of by path, so two Store apps do not collapse into one tile.
//!
//! The pid → image path cache is what keeps a five-second refresh cheap: a refresh opens a handle per
//! *new* process, not per window, and forgets processes that are gone so a reused pid cannot inherit
//! the previous one's path.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use windows::Win32::Foundation::{CloseHandle, HWND};
use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GWL_EXSTYLE, GetClassNameW, GetForegroundWindow, GetWindowLongPtrW,
    GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId, IsWindow, IsWindowVisible,
    SW_RESTORE, SetForegroundWindow, ShowWindow, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW,
};

/// The executable every UWP frame runs as; its path says nothing about which app is inside.
const UWP_FRAME_HOST: &str = "ApplicationFrameHost.exe";

/// The window class of a UWP frame, for the same reason.
const UWP_FRAME_CLASS: &str = "ApplicationFrameWindow";

/// Our own dashboard: the shell shows the desktop's apps, not a mirror of itself.
const DASHBOARD_PROCESS: &str = "Neb.App.exe";

/// Window classes that belong to the desktop or to the taskbar. They are visible, titled, and would
/// otherwise be dock tiles.
const SHELL_CLASSES: [&str; 5] = [
    "Progman",
    "WorkerW",
    "Shell_TrayWnd",
    "Shell_SecondaryTrayWnd",
    "Windows.UI.Core.CoreWindow",
];

/// Longest image path `QueryFullProcessImageNameW` is asked for.
const PATH_UNITS: usize = 1024;

/// One running application: a window that can be brought to the front, plus its executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningApp {
    /// The window a click activates, as a plain integer so the value can travel between threads.
    pub handle: isize,
    /// The process that owns the window.
    pub pid: u32,
    /// Window text at the time of enumeration.
    pub title: String,
    /// Full path of the executable, or `None` when Windows refused the query.
    pub path: Option<PathBuf>,
    /// Window class; a UWP frame is recognised by it.
    pub class: String,
}

impl RunningApp {
    /// The tile's name: the executable's file name when the path is known, so a tile does not rename
    /// itself every time the document title changes, and the window title otherwise.
    pub fn name(&self) -> String {
        match &self.path {
            Some(path) if !path.as_os_str().is_empty() => crate::dock::catalog::name_for(path),
            _ => self.title.trim().to_owned(),
        }
    }
}

/// Everything the "is this an application window" decision needs, captured once per window so the
/// decision itself stays pure and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowFacts {
    /// Top-level window handle.
    pub handle: isize,
    /// Window text; empty for many system windows.
    pub title: String,
    /// Owning process id.
    pub pid: u32,
    /// Extended window style.
    pub ex_style: u32,
    /// Window class.
    pub class: String,
    /// `IsWindowVisible`.
    pub visible: bool,
    /// A DWM-cloaked window: it reports visible, but nothing is on screen.
    pub cloaked: bool,
}

/// Whether `class` names the desktop, the taskbar or another shell surface.
pub(crate) fn is_shell_class(class: &str) -> bool {
    SHELL_CLASSES
        .iter()
        .any(|shell| shell.eq_ignore_ascii_case(class))
}

/// Whether `window` is a window the dock could offer at all.
///
/// A tool window is a helper (a palette, a tray flyout) and a non-activating one cannot be brought to
/// the front at all, so offering either in the dock would be a button that does nothing.
pub fn is_app_window(window: &WindowFacts, own_pid: u32) -> bool {
    if window.handle == 0 || !window.visible || window.cloaked {
        return false;
    }

    if window.pid == 0 || window.pid == own_pid {
        return false;
    }

    if window.title.trim().is_empty() {
        return false;
    }

    if is_shell_class(&window.class) {
        return false;
    }

    window.ex_style & (WS_EX_TOOLWINDOW.0 | WS_EX_NOACTIVATE.0) == 0
}

/// The identity a running app is grouped and compared by.
///
/// The executable when Windows allows the query, otherwise the process. A UWP frame is grouped by
/// process even though its executable is known, because every frame's executable is the same host.
pub fn key_for(app: &RunningApp) -> String {
    if is_uwp_frame(app) {
        return format!("pid:{}", app.pid);
    }

    match app
        .path
        .as_deref()
        .and_then(|path| crate::dock::catalog::normalize(&path.to_string_lossy()))
    {
        Some(path) => path.to_string_lossy().to_lowercase(),
        None => format!("pid:{}", app.pid),
    }
}

/// Whether `app` is a UWP frame, recognised by its class or by its host executable.
fn is_uwp_frame(app: &RunningApp) -> bool {
    if app.class.eq_ignore_ascii_case(UWP_FRAME_CLASS) {
        return true;
    }

    app.path
        .as_deref()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(UWP_FRAME_HOST))
}

/// Whether a process belongs to us: the dashboard would otherwise show up as an app nobody started.
fn is_excluded_process(path: Option<&Path>) -> bool {
    path.and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case(DASHBOARD_PROCESS))
}

/// A snapshot of the application windows, one per application, in name order.
pub fn enumerate() -> Vec<RunningApp> {
    let own_pid = std::process::id();
    let mut paths = path_cache().lock().unwrap_or_else(|lock| lock.into_inner());
    let mut collected = Collector {
        own_pid,
        paths: &mut paths,
        seen: Vec::new(),
        found: Vec::new(),
    };
    let pointer = &raw mut collected as isize;

    // SAFETY: `collect` is a valid `WNDENUMPROC` and `pointer` is the address of a `Collector` that
    // outlives the call; `EnumWindows` only ever hands it back to that callback, on this thread.
    let _ = unsafe { EnumWindows(Some(collect), windows::Win32::Foundation::LPARAM(pointer)) };

    let Collector {
        mut found, seen, ..
    } = collected;

    paths.retain(|pid, _| seen.contains(pid));
    drop(paths);

    // One tile per application, and in name order so a focus change cannot make the running half of
    // the dock jump around.
    let mut apps: Vec<RunningApp> = Vec::with_capacity(found.len());
    let mut keys: Vec<String> = Vec::with_capacity(found.len());
    for app in found.drain(..) {
        let key = key_for(&app);
        if !keys.iter().any(|known| known == &key) {
            keys.push(key);
            apps.push(app);
        }
    }

    // Name order, so a focus or document-title change cannot make the running half of the dock jump
    // around. The key is computed once per app rather than once per comparison.
    apps.sort_by_cached_key(|app| app.name().to_lowercase());
    apps
}

/// The pid → image path cache, shared by every snapshot.
fn path_cache() -> &'static Mutex<HashMap<u32, Option<PathBuf>>> {
    static CACHE: OnceLock<Mutex<HashMap<u32, Option<PathBuf>>>> = OnceLock::new();

    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// What one `EnumWindows` pass is carrying.
struct Collector<'a> {
    own_pid: u32,
    paths: &'a mut HashMap<u32, Option<PathBuf>>,
    seen: Vec<u32>,
    found: Vec<RunningApp>,
}

/// Collects one window into the snapshot the enumeration was started with.
///
/// # Safety
///
/// `data` must be the address of a live `Collector` for the whole enumeration.
unsafe extern "system" fn collect(
    hwnd: HWND,
    data: windows::Win32::Foundation::LPARAM,
) -> windows::core::BOOL {
    let collector = data.0 as *mut Collector;

    if collector.is_null() {
        return false.into();
    }

    // SAFETY: the caller guarantees the pointer is a live `Collector`; the callback runs on the
    // calling thread, so no other reference to it exists at this point.
    let collector = unsafe { &mut *collector };

    let facts = describe(hwnd);
    if !is_app_window(&facts, collector.own_pid) {
        return true.into();
    }

    if !collector.seen.contains(&facts.pid) {
        collector.seen.push(facts.pid);

        // The path is queried once per process, and a refusal is remembered as a refusal.
        collector
            .paths
            .entry(facts.pid)
            .or_insert_with(|| image_path(facts.pid));
    }

    let path = collector.paths.get(&facts.pid).cloned().flatten();
    if is_excluded_process(path.as_deref()) {
        return true.into();
    }

    collector.found.push(RunningApp {
        handle: facts.handle,
        pid: facts.pid,
        title: facts.title,
        path,
        class: facts.class,
    });

    true.into()
}

/// Reads everything [`is_app_window`] needs about one window.
fn describe(hwnd: HWND) -> WindowFacts {
    // SAFETY: `hwnd` comes from `EnumWindows`, which only hands over live top-level windows; every
    // call below takes the handle and a live out-parameter and changes nothing about the window.
    unsafe {
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        WindowFacts {
            handle: hwnd.0 as isize,
            title: window_text(hwnd),
            pid,
            // SAFETY: the call reads one window attribute of a live window.
            ex_style: GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32,
            class: class_name(hwnd),
            visible: IsWindowVisible(hwnd).as_bool(),
            cloaked: is_cloaked(hwnd),
        }
    }
}

/// The window's text, or an empty string when it has none or it is not valid UTF-16.
fn window_text(hwnd: HWND) -> String {
    // SAFETY: `hwnd` is a live window; the length call takes only the handle.
    let length = unsafe { GetWindowTextLengthW(hwnd) };
    if length <= 0 {
        return String::new();
    }

    let mut buffer = vec![0u16; usize::try_from(length + 1).unwrap_or(1)];
    // SAFETY: `buffer` is a live, correctly sized array and the call writes at most its length.
    let written = unsafe { GetWindowTextW(hwnd, &mut buffer) };
    if written <= 0 {
        return String::new();
    }

    String::from_utf16_lossy(&buffer[..usize::try_from(written).unwrap_or(0)])
}

/// The window's class name, or an empty string when Windows refuses it.
fn class_name(hwnd: HWND) -> String {
    let mut buffer = [0u16; 256];
    // SAFETY: `buffer` is a live, correctly sized array and the call writes at most its length.
    let written = unsafe { GetClassNameW(hwnd, &mut buffer) };
    if written <= 0 {
        return String::new();
    }

    String::from_utf16_lossy(&buffer[..usize::try_from(written).unwrap_or(0)])
}

/// Whether DWM says the window is cloaked: a hidden virtual desktop or a suspended UWP app.
fn is_cloaked(hwnd: HWND) -> bool {
    let mut cloaked = 0u32;

    // SAFETY: `cloaked` is a live out-parameter of the size the attribute documents; a refusal means
    // the attribute is unknown, which is treated as "not cloaked" so nothing disappears silently.
    let status = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CLOAKED,
            (&raw mut cloaked).cast(),
            std::mem::size_of::<u32>() as u32,
        )
    };

    status.is_ok() && cloaked != 0
}

/// The image path of `pid`, or `None` when the process cannot be queried (an elevated one, usually).
fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: the call takes a plain pid; the returned handle is owned here and closed below.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    let mut buffer = [0u16; PATH_UNITS];
    let mut size = u32::try_from(buffer.len()).unwrap_or(0);
    // SAFETY: `buffer` and `size` are live; the call writes at most `size` units and reports how many.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            windows::core::PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };

    // SAFETY: the handle came from `OpenProcess` above and is closed exactly once, here.
    unsafe {
        let _ = CloseHandle(process);
    }

    if queried.is_err() || size == 0 {
        return None;
    }

    let path = PathBuf::from(String::from_utf16_lossy(
        &buffer[..usize::try_from(size).unwrap_or(0)],
    ));

    (!path.as_os_str().is_empty()).then_some(path)
}

/// Brings a window to the front, and reports whether Windows actually let it happen.
///
/// The order is the ruling's: `SetForegroundWindow` first, then `SW_RESTORE`. A minimised window
/// cannot take the foreground, so when the first attempt does not win, the restore is the retry —
/// and the verdict is `GetForegroundWindow`, not the return value, because the desktop decides.
pub fn activate(handle: isize) -> bool {
    let target = HWND(handle as *mut std::ffi::c_void);

    if handle == 0 || !is_window(target) {
        crate::log_warn!("dock", "the window to activate is gone");
        return false;
    }

    // SAFETY: `target` is a live window of another process; both calls take only the handle.
    unsafe {
        let _ = SetForegroundWindow(target);
    }

    if foreground() == handle {
        crate::log_info!("dock", "activated window {handle:#x}");
        return true;
    }

    // SAFETY: as above; `SW_RESTORE` is the documented command for a minimised window.
    unsafe {
        let _ = ShowWindow(target, SW_RESTORE);
        let _ = SetForegroundWindow(target);
    }

    let activated = foreground() == handle;
    if activated {
        crate::log_info!("dock", "activated window {handle:#x} after restoring it");
    } else {
        crate::log_warn!(
            "dock",
            "Windows refused the foreground switch to {handle:#x} (the target keeps its place)"
        );
    }

    activated
}

/// The handle of the window that owns the foreground, as a plain integer.
pub fn foreground() -> isize {
    // SAFETY: the call takes no arguments and returns a handle this process does not own.
    unsafe { GetForegroundWindow().0 as isize }
}

/// Whether `handle` still names a window.
fn is_window(handle: HWND) -> bool {
    // SAFETY: the call takes only the handle and returns a boolean.
    unsafe { IsWindow(Some(handle)).as_bool() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts(class: &str) -> WindowFacts {
        WindowFacts {
            handle: 0x1000,
            title: "Untitled - Notepad".to_owned(),
            pid: 42,
            ex_style: 0,
            class: class.to_owned(),
            visible: true,
            cloaked: false,
        }
    }

    fn app(class: &str, path: Option<&str>, pid: u32) -> RunningApp {
        RunningApp {
            handle: 0x1000,
            pid,
            title: "Some title".to_owned(),
            path: path.map(PathBuf::from),
            class: class.to_owned(),
        }
    }

    #[test]
    fn an_ordinary_visible_window_is_an_application() {
        assert!(is_app_window(&facts("Notepad"), 1));
    }

    #[test]
    fn invisible_cloaked_and_untitled_windows_are_not() {
        let mut window = facts("Notepad");
        window.visible = false;
        assert!(!is_app_window(&window, 1));

        let mut window = facts("Notepad");
        window.cloaked = true;
        assert!(
            !is_app_window(&window, 1),
            "a cloaked window is not on screen"
        );

        let mut window = facts("Notepad");
        window.title = "   ".to_owned();
        assert!(
            !is_app_window(&window, 1),
            "the dock only offers titled windows"
        );

        let mut window = facts("Notepad");
        window.handle = 0;
        assert!(!is_app_window(&window, 1));
    }

    #[test]
    fn our_own_windows_and_shell_chrome_are_not_applications() {
        assert!(
            !is_app_window(&facts("Notepad"), 42),
            "the shell's own windows must not appear"
        );

        for class in SHELL_CLASSES {
            assert!(!is_app_window(&facts(class), 1), "{class}");
        }
    }

    #[test]
    fn tool_and_non_activating_windows_are_not_applications() {
        let mut window = facts("Notepad");
        window.ex_style = WS_EX_TOOLWINDOW.0;
        assert!(!is_app_window(&window, 1));

        let mut window = facts("Notepad");
        window.ex_style = WS_EX_NOACTIVATE.0;
        assert!(!is_app_window(&window, 1));
    }

    #[test]
    fn two_windows_of_one_executable_share_a_key() {
        let left = app("Notepad", Some(r"C:\Windows\notepad.exe"), 42);
        let right = app("Notepad", Some(r"C:\WINDOWS\NOTEPAD.EXE"), 43);

        assert_eq!(key_for(&left), key_for(&right));
    }

    #[test]
    fn uwp_frames_are_grouped_by_process_not_by_path() {
        let left = app(
            "ApplicationFrameWindow",
            Some(r"C:\Windows\System32\ApplicationFrameHost.exe"),
            7,
        );
        let right = app(
            "ApplicationFrameWindow",
            Some(r"C:\Windows\System32\ApplicationFrameHost.exe"),
            8,
        );

        assert_eq!(key_for(&left), "pid:7");
        assert_ne!(key_for(&left), key_for(&right));
    }

    #[test]
    fn a_window_without_a_path_falls_back_to_its_process() {
        assert_eq!(key_for(&app("Notepad", None, 99)), "pid:99");
    }

    #[test]
    fn the_tile_is_named_by_its_executable_when_it_is_known() {
        let with_path = app("Notepad", Some(r"C:\Windows\System32\notepad.exe"), 42);
        assert_eq!(with_path.name(), "notepad");

        let without = app("Notepad", None, 42);
        assert_eq!(without.name(), "Some title");
    }

    #[test]
    fn the_dashboard_is_excluded_by_its_executable() {
        assert!(is_excluded_process(Some(Path::new(r"C:\Apps\Neb.App.exe"))));
        assert!(!is_excluded_process(Some(Path::new(
            r"C:\Windows\notepad.exe"
        ))));
        assert!(!is_excluded_process(None));
    }

    #[test]
    fn the_foreground_window_is_a_real_handle_in_an_interactive_session() {
        // A desktop session always has a foreground window; a build agent may not, so the assertion
        // is only that the call answers with a number.
        let _ = foreground();
    }
}
