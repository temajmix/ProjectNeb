//! The ProjectNeb tray icon: one hidden window, one notification icon, a menu that never needs a
//! console.
//!
//! The icon lives on the thread that owns the supervisor's message queue. [`Tray::pump`] drains the
//! queue on every supervisor tick, which is where a click becomes a menu and a menu item becomes an
//! action. The worker thread never touches the window directly; it asks [`Notifier`] to show a
//! balloon, and `Notifier` only carries the window handle as an `isize` so it can cross threads.

use std::mem::size_of;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use windows::Win32::Foundation::{
    FreeLibrary, GetLastError, HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM,
};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::UI::Controls::{
    TASKDIALOG_BUTTON, TASKDIALOGCONFIG, TASKDIALOGCONFIG_0, TD_INFORMATION_ICON,
    TDCBF_CLOSE_BUTTON, TDF_ALLOW_DIALOG_CANCELLATION, TDF_SIZE_TO_CONTENT,
};
use windows::Win32::UI::Shell::{
    NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE, NIM_MODIFY,
    NOTIFYICONDATAW, Shell_NotifyIconW, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIcon, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon,
    DestroyMenu, DestroyWindow, DispatchMessageW, GWLP_USERDATA, GetCursorPos, GetWindowLongPtrW,
    HICON, IMAGE_ICON, LR_LOADFROMFILE, LoadImageW, MB_ICONINFORMATION, MB_OK, MF_GRAYED,
    MF_SEPARATOR, MF_STRING, MSG, MessageBoxW, PM_REMOVE, PeekMessageW, PostMessageW,
    RegisterClassW, SW_SHOWNORMAL, SetForegroundWindow, SetWindowLongPtrW, TPM_RETURNCMD,
    TPM_RIGHTBUTTON, TrackPopupMenu, TranslateMessage, WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP,
    WNDCLASSW, WS_POPUP,
};
use windows::core::PCWSTR;

use crate::supervisor::Reporter;
use crate::update::{self, Request, StopFn, WhatsNew};

/// Menu id of the notification icon.
const TRAY_ID: u32 = 1;

/// The message the shell sends for this icon; `WM_APP + 1` keeps it out of every standard range.
const TRAY_MESSAGE: u32 = WM_APP + 1;

/// Window class the hidden tray window belongs to.
const TRAY_CLASS: &str = "ProjectNeb.Tray.Rust";

/// Window title, which only shows in diagnostic tools.
const TRAY_TITLE: &str = "ProjectNeb tray";

/// Button of the task dialog that opens the full changelog.
const CHANGELOG_BUTTON: i32 = 100;

/// Left-overs of an earlier registration are not an error.
static TRAY_CLASS_REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// What a tray click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuCommand {
    /// Open the settings window (`neb-app.exe`).
    Settings,
    /// Ask the update worker to check now.
    CheckUpdates,
    /// Show the notes of the downloaded version.
    WhatsNew,
    /// Open the changelog in the default browser.
    Changelog,
    /// Put every Windows shell value the replacement mode changed back.
    RestoreWindows,
    /// Stop the children and leave.
    Exit,
}

impl MenuCommand {
    /// The `WM_COMMAND` id the popup menu carries.
    fn id(self) -> usize {
        match self {
            Self::Settings => 6,
            Self::CheckUpdates => 1,
            Self::WhatsNew => 2,
            Self::Changelog => 3,
            Self::RestoreWindows => 4,
            Self::Exit => 5,
        }
    }
}

/// One row of the tray menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MenuItem {
    /// What choosing the row does.
    pub command: MenuCommand,
    /// The Russian label the user sees.
    pub label: &'static str,
    /// Whether the row can be chosen.
    pub enabled: bool,
    /// Whether a separator is drawn above it.
    pub separator_before: bool,
}

/// The menu the tray builds. Split out from the Win32 code so the decisions are testable.
///
/// «Что нового» needs something to show: either the notes of a downloaded update or a pending
/// marker. «Проверить обновления» needs a running worker, and «Вернуть всё как было» needs the
/// supervisor's windows-mode session.
pub fn menu_items(
    updates_available: bool,
    whats_new_available: bool,
    restore_available: bool,
) -> Vec<MenuItem> {
    vec![
        MenuItem {
            command: MenuCommand::Settings,
            label: "Настройки",
            enabled: true,
            separator_before: false,
        },
        MenuItem {
            command: MenuCommand::CheckUpdates,
            label: "Проверить обновления",
            enabled: updates_available,
            separator_before: true,
        },
        MenuItem {
            command: MenuCommand::WhatsNew,
            label: "Что нового",
            enabled: whats_new_available,
            separator_before: false,
        },
        MenuItem {
            command: MenuCommand::Changelog,
            label: "Открыть changelog",
            enabled: true,
            separator_before: false,
        },
        MenuItem {
            command: MenuCommand::RestoreWindows,
            label: "Вернуть всё как было",
            enabled: restore_available,
            separator_before: true,
        },
        MenuItem {
            command: MenuCommand::Exit,
            label: "Выход",
            enabled: true,
            separator_before: true,
        },
    ]
}

/// The action behind «Вернуть всё как было»: restore every windows-mode value and say whether there
/// was anything to restore.
pub type RestoreFn = Arc<dyn Fn() -> Result<bool, String> + Send + Sync>;

/// The action behind «Настройки»: ask the supervisor to open or focus the main window.
pub type OpenFn = Arc<dyn Fn() + Send + Sync>;

/// Why no icon appeared.
#[derive(Debug, thiserror::Error)]
pub enum TrayError {
    /// The window class could not be registered.
    #[error("cannot register the tray window class")]
    Class,

    /// The hidden window could not be created.
    #[error("cannot create the tray window: {0}")]
    Window(#[source] windows::core::Error),

    /// The shell refused the icon.
    #[error("Shell_NotifyIconW(NIM_ADD) failed (0x{0:08X})")]
    Notify(u32),
}

/// A cross-thread balloon notifier. It carries no `HWND` (which is not `Send`) but the handle value,
/// which is valid for as long as the tray window exists; the worker is stopped before it is destroyed.
#[derive(Debug, Clone, Copy)]
pub struct Notifier {
    window: isize,
}

impl Notifier {
    /// Shows an information balloon next to the icon. Returns whether the shell accepted it.
    pub fn balloon(&self, title: &str, text: &str) -> bool {
        if self.window == 0 {
            return false;
        }

        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: HWND(self.window as *mut std::ffi::c_void),
            uID: TRAY_ID,
            uFlags: NIF_INFO,
            dwInfoFlags: NIIF_INFO,
            ..NOTIFYICONDATAW::default()
        };
        copy_wide(title, &mut data.szInfoTitle);
        copy_wide(text, &mut data.szInfo);

        // SAFETY: `data` describes this process's own icon and outlives the call.
        unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) }.as_bool()
    }
}

/// The resident icon.
pub struct Tray {
    hwnd: HWND,
    icon: HICON,
    menu_requested: Arc<AtomicBool>,
    root: std::path::PathBuf,
    reporter: Reporter,
    updates: Option<Sender<Request>>,
    restore: Option<RestoreFn>,
    open_settings: OpenFn,
    stop: StopFn,
}

impl std::fmt::Debug for Tray {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Tray")
            .field("hwnd", &self.hwnd)
            .field("updates", &self.updates.is_some())
            .finish_non_exhaustive()
    }
}

impl Tray {
    /// Creates the hidden window and adds the notification icon.
    ///
    /// The icon is `neb.ico` next to the executable when that file exists; otherwise a generated
    /// 16×16 glyph is used, because a tray without an icon is not a tray.
    ///
    /// # Errors
    ///
    /// [`TrayError`] when Windows refuses the class, the window or the icon.
    pub fn new(
        exe_dir: &Path,
        root: std::path::PathBuf,
        reporter: Reporter,
        stop: StopFn,
        open_settings: OpenFn,
    ) -> Result<Self, TrayError> {
        register_class()?;

        let class = wide(TRAY_CLASS);
        let title = wide(TRAY_TITLE);
        // SAFETY: both strings are NUL-terminated and outlive the call. The window is never shown;
        // it exists only to receive the notification callbacks and to anchor the popup menu.
        let hwnd = unsafe {
            CreateWindowExW(
                windows::Win32::UI::WindowsAndMessaging::WINDOW_EX_STYLE(0),
                PCWSTR(class.as_ptr()),
                PCWSTR(title.as_ptr()),
                WS_POPUP,
                0,
                0,
                0,
                0,
                None,
                None,
                None,
                None,
            )
        }
        .map_err(TrayError::Window)?;

        let menu_requested = Arc::new(AtomicBool::new(false));
        let state = Box::into_raw(Box::new(Arc::clone(&menu_requested)));
        // SAFETY: `hwnd` was created on this thread; the pointer is reclaimed in `Drop`.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, state as isize) };

        let (icon, source) = load_icon(exe_dir);
        let mut data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: hwnd,
            uID: TRAY_ID,
            uFlags: NIF_MESSAGE | NIF_ICON | NIF_TIP,
            uCallbackMessage: TRAY_MESSAGE,
            hIcon: icon,
            ..NOTIFYICONDATAW::default()
        };
        copy_wide("ProjectNeb", &mut data.szTip);

        // SAFETY: `data` points at live buffers for the duration of the call.
        let added = unsafe { Shell_NotifyIconW(NIM_ADD, &data) }.as_bool();
        if !added {
            // SAFETY: `hwnd` and `icon` are this process's own resources and are released once.
            unsafe {
                let _ = DestroyWindow(hwnd);
                if !icon.is_invalid() {
                    let _ = DestroyIcon(icon);
                }
                drop(Box::from_raw(state));
            }

            // SAFETY: reads this thread's last error.
            return Err(TrayError::Notify(unsafe { GetLastError() }.0));
        }

        reporter.info(&format!(
            "tray: Shell_NotifyIconW(NIM_ADD) succeeded ({source})"
        ));

        Ok(Self {
            hwnd,
            icon,
            menu_requested,
            root,
            reporter,
            updates: None,
            restore: None,
            open_settings,
            stop,
        })
    }

    /// Gives the menu a worker to ask for a check. A tray created before the worker calls this once.
    pub fn set_updates(&mut self, updates: Option<Sender<Request>>) {
        self.updates = updates;
    }

    /// Gives the menu the action that puts the Windows shell back.
    pub fn set_restore(&mut self, restore: RestoreFn) {
        self.restore = Some(restore);
    }

    /// A notifier the update worker can call from its own thread.
    pub fn notifier(&self) -> Notifier {
        Notifier {
            window: self.hwnd.0 as isize,
        }
    }

    /// Drains the window queue and opens the menu when a click asked for it.
    pub fn pump(&self) {
        let mut message = MSG::default();
        loop {
            // SAFETY: `message` is a valid out-parameter; the filters ask for everything.
            let more = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) };
            if !more.as_bool() {
                break;
            }

            // SAFETY: `message` was filled by `PeekMessageW` and belongs to this thread.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }

        if self.menu_requested.swap(false, Ordering::SeqCst) {
            self.show_menu();
        }
    }

    /// Shows a notes dialog from the tray menu.
    pub fn show_whats_new(&self, notes: &WhatsNew, mark: bool) -> bool {
        // No owner: the tray window is never shown, and a task dialog owned by it opens behind
        // the dashboard instead of in front of it. A top-level dialog is what the user expects
        // after an update, and the loop waits for it before the first tick.
        let shown = show_whats_new_dialog(None, notes);

        if shown && mark {
            if let Err(error) = update::mark_shown(&self.root) {
                self.reporter.warn(&format!(
                    "whats-new: cannot mark the notes as shown: {error}"
                ));
            }
        }

        shown
    }

    /// Builds and runs the popup menu.
    fn show_menu(&self) {
        let notes = update::read_whats_new(&self.root);
        let pending = update::read_pending(&self.root).is_some();
        let items = menu_items(
            self.updates.is_some(),
            notes.is_some() || pending,
            self.restore.is_some(),
        );

        // SAFETY: creates an owned menu; every early return below destroys it.
        let Ok(menu) = (unsafe { CreatePopupMenu() }) else {
            self.reporter
                .warn("tray: the popup menu could not be created");
            return;
        };

        for item in &items {
            if item.separator_before {
                // SAFETY: `menu` is a live menu; a separator has no text.
                unsafe {
                    let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                }
            }

            let label = wide(item.label);
            let flags = if item.enabled {
                MF_STRING
            } else {
                MF_STRING | MF_GRAYED
            };
            // SAFETY: `menu` is live and `label` outlives the call.
            unsafe {
                let _ = AppendMenuW(menu, flags, item.command.id(), PCWSTR(label.as_ptr()));
            }
        }

        let mut point = POINT::default();
        // SAFETY: `point` is a valid out-parameter and `hwnd` is this process's window.
        unsafe {
            let _ = GetCursorPos(&mut point);
            let _ = SetForegroundWindow(self.hwnd);
        }

        // SAFETY: `menu` and `hwnd` are live; `TPM_RETURNCMD` makes the return value the command id.
        let chosen = unsafe {
            TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_RIGHTBUTTON,
                point.x,
                point.y,
                None,
                self.hwnd,
                None,
            )
        };

        // SAFETY: posts the documented "menu dismissed" message and destroys the owned menu.
        unsafe {
            let _ = PostMessageW(Some(self.hwnd), WM_NULL, WPARAM(0), LPARAM(0));
            let _ = DestroyMenu(menu);
        }

        match chosen.0 {
            6 => (self.open_settings)(),
            1 => {
                if let Some(updates) = &self.updates {
                    let _ = updates.send(Request::CheckNow);
                }
            }
            2 => {
                if let Some(notes) = notes {
                    self.show_whats_new(&notes, true);
                }
            }
            3 => {
                let url = notes
                    .as_ref()
                    .map(|notes| notes.changelog_url.as_str())
                    .filter(|url| !url.is_empty())
                    .unwrap_or(update::DEFAULT_CHANGELOG_URL);
                open_url(url);
            }
            4 => {
                if let Some(restore) = &self.restore {
                    match restore() {
                        Ok(true) => {
                            let notifier = self.notifier();
                            let _ = notifier.balloon("ProjectNeb", "Настройки Windows возвращены");
                        }
                        Ok(false) => self
                            .reporter
                            .info("windows-mode: nothing to restore, the mode was not applied"),
                        Err(reason) => self
                            .reporter
                            .warn(&format!("windows-mode: the restore failed ({reason})")),
                    }
                }
            }
            5 => (self.stop)(),
            _ => {}
        }
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        let data = NOTIFYICONDATAW {
            cbSize: size_of::<NOTIFYICONDATAW>() as u32,
            hWnd: self.hwnd,
            uID: TRAY_ID,
            ..NOTIFYICONDATAW::default()
        };

        // SAFETY: the icon was added by `new` and is removed exactly once here.
        unsafe {
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            if !self.icon.is_invalid() {
                let _ = DestroyIcon(self.icon);
            }
        }

        // SAFETY: the boxed flag was stored in `new` and is reclaimed exactly once.
        unsafe {
            let state = GetWindowLongPtrW(self.hwnd, GWLP_USERDATA) as *mut Arc<AtomicBool>;
            if !state.is_null() {
                drop(Box::from_raw(state));
            }
            let _ = DestroyWindow(self.hwnd);
        }

        self.reporter.info("tray: icon removed");
    }
}

/// Registers the tray window class once per process.
fn register_class() -> Result<(), TrayError> {
    let registered = *TRAY_CLASS_REGISTERED.get_or_init(|| {
        let class = wide(TRAY_CLASS);
        let description = WNDCLASSW {
            lpfnWndProc: Some(tray_proc),
            hInstance: HINSTANCE::default(),
            lpszClassName: PCWSTR(class.as_ptr()),
            ..WNDCLASSW::default()
        };

        // SAFETY: `description` is fully initialised and the class name outlives the call.
        unsafe { RegisterClassW(&description) != 0 }
    });

    if registered {
        Ok(())
    } else {
        Err(TrayError::Class)
    }
}

/// The window procedure: a left or right click asks [`Tray::pump`] to open the menu.
unsafe extern "system" fn tray_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == TRAY_MESSAGE {
        let event = lparam.0 as u32;
        if event == WM_LBUTTONUP || event == WM_RBUTTONUP {
            // SAFETY: the pointer was stored with `SetWindowLongPtrW` and lives until `Drop`.
            let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Arc<AtomicBool>;
            if !state.is_null() {
                // SAFETY: the box behind the pointer is alive for the whole window lifetime.
                unsafe { &*state }.store(true, Ordering::SeqCst);
            }
        }

        return LRESULT(0);
    }

    // SAFETY: every other message goes to the default procedure.
    unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
}

/// Loads `neb.ico` from `exe_dir`, or generates a 16×16 icon when it is missing.
fn load_icon(exe_dir: &Path) -> (HICON, String) {
    let path = exe_dir.join("neb.ico");

    if path.is_file() {
        let file = wide(&path.to_string_lossy());
        // SAFETY: the path is NUL-terminated and outlives the call.
        if let Ok(handle) = unsafe {
            LoadImageW(
                None,
                PCWSTR(file.as_ptr()),
                IMAGE_ICON,
                16,
                16,
                LR_LOADFROMFILE,
            )
        } {
            return (
                HICON(handle.0),
                format!("icon loaded from '{}'", path.display()),
            );
        }
    }

    (generated_icon(), "generated 16x16 icon".to_owned())
}

/// Draws a small filled circle in a monochrome icon, so a run without `neb.ico` still shows something.
fn generated_icon() -> HICON {
    const WIDTH: usize = 16;
    const HEIGHT: usize = 16;
    const STRIDE: usize = 2;

    let mut mask = [0u8; WIDTH * HEIGHT / 8];
    let mut colour = [0u8; WIDTH * HEIGHT / 8];

    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let dx = x as i32 - 8;
            let dy = y as i32 - 8;
            if dx * dx + dy * dy <= 42 {
                colour[y * STRIDE + x / 8] |= 1 << (7 - x % 8);
            }
        }
    }

    // SAFETY: the two masks are 16x16 1-bpp bitmaps (two bytes per row) and outlive the call.
    let made = unsafe { CreateIcon(None, 16, 16, 1, 1, mask.as_mut_ptr(), colour.as_mut_ptr()) };

    made.unwrap_or_else(|_| {
        // A missing glyph is better than a crash; `Drop` only destroys icons this process owns.
        HICON::default()
    })
}

/// Opens `url` in the default browser. Returns whether the shell accepted the request.
pub fn open_url(url: &str) -> bool {
    let operation = wide("open");
    let url = wide(url);

    // SAFETY: both strings are NUL-terminated and outlive the call; the return value is an HINSTANCE
    // whose value above 32 is the documented success value.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(url.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    result.0 as isize > 32
}

/// Shows the notes in a task dialog with a changelog button, falling back to a message box.
///
/// `TaskDialogIndirect` is loaded from `comctl32.dll` at run time instead of being imported: the
/// v6 common controls live behind an activation context, and a plain console program has none, so a
/// static import makes the whole executable fail to load. When the export is missing, the message box
/// still says what changed.
///
/// Returns whether a dialog appeared at all.
pub fn show_whats_new_dialog(hwnd: Option<HWND>, notes: &WhatsNew) -> bool {
    let title = wide(&format!("ProjectNeb {}", notes.version));
    let instruction = wide("Что нового");
    let content = wide(&notes.notes);
    let button = wide("Открыть полный changelog");
    let buttons = [TASKDIALOG_BUTTON {
        nButtonID: CHANGELOG_BUTTON,
        pszButtonText: PCWSTR(button.as_ptr()),
    }];
    let config = TASKDIALOGCONFIG {
        cbSize: size_of::<TASKDIALOGCONFIG>() as u32,
        hwndParent: hwnd.unwrap_or_default(),
        hInstance: HINSTANCE::default(),
        dwFlags: TDF_ALLOW_DIALOG_CANCELLATION | TDF_SIZE_TO_CONTENT,
        dwCommonButtons: TDCBF_CLOSE_BUTTON,
        pszWindowTitle: PCWSTR(title.as_ptr()),
        Anonymous1: TASKDIALOGCONFIG_0 {
            pszMainIcon: TD_INFORMATION_ICON,
        },
        pszMainInstruction: PCWSTR(instruction.as_ptr()),
        pszContent: PCWSTR(content.as_ptr()),
        cButtons: 1,
        pButtons: buttons.as_ptr(),
        nDefaultButton: CHANGELOG_BUTTON,
        ..TASKDIALOGCONFIG::default()
    };

    let mut chosen = 0i32;
    if let Some(task_dialog) = task_dialog_indirect() {
        // SAFETY: `config` and every string it points at outlive the call, and the signature is the
        // documented one (`TaskDialogIndirect` from commctrl.h).
        let result = unsafe {
            task_dialog(
                &config,
                &mut chosen,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };

        if result.is_ok() {
            if chosen == CHANGELOG_BUTTON {
                open_url(&notes.changelog_url);
            }
            return true;
        }
    }

    let caption = wide(&format!("ProjectNeb {}", notes.version));
    // SAFETY: both strings outlive the call and are NUL-terminated.
    unsafe {
        MessageBoxW(
            hwnd,
            PCWSTR(content.as_ptr()),
            PCWSTR(caption.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }

    true
}

/// The part of `TaskDialogIndirect` this build uses.
type TaskDialogIndirectFn = unsafe extern "system" fn(
    *const TASKDIALOGCONFIG,
    *mut i32,
    *mut i32,
    *mut windows::core::BOOL,
) -> windows::core::HRESULT;

/// Resolves `TaskDialogIndirect` from `comctl32.dll`, or `None` when this system has no such export.
fn task_dialog_indirect() -> Option<TaskDialogIndirectFn> {
    static RESOLVED: std::sync::OnceLock<Option<TaskDialogIndirectFn>> = std::sync::OnceLock::new();

    *RESOLVED.get_or_init(|| {
        let module_name = wide("comctl32.dll");
        // SAFETY: the name is NUL-terminated and outlives the call. The module is deliberately kept
        // loaded for the rest of the process: the cached function pointer would dangle otherwise.
        let module = unsafe { LoadLibraryW(PCWSTR(module_name.as_ptr())) }.ok()?;

        // SAFETY: the module is loaded and the name is a NUL-terminated ASCII string.
        let procedure = unsafe { GetProcAddress(module, windows::core::s!("TaskDialogIndirect")) };

        match procedure {
            // SAFETY: the export has exactly the documented `TaskDialogIndirect` signature.
            Some(procedure) => Some(unsafe {
                std::mem::transmute::<unsafe extern "system" fn() -> isize, TaskDialogIndirectFn>(
                    procedure,
                )
            }),
            None => {
                // SAFETY: the module was loaded above and is no longer needed.
                unsafe {
                    let _ = FreeLibrary(module);
                }
                None
            }
        }
    })
}

/// Encodes `text` as a NUL-terminated UTF-16 string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Copies `text` into a fixed Win32 buffer, truncating rather than overflowing.
fn copy_wide<const N: usize>(text: &str, target: &mut [u16; N]) {
    let encoded: Vec<u16> = text.encode_utf16().take(N.saturating_sub(1)).collect();
    target[..encoded.len()].copy_from_slice(&encoded);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(menu: &[MenuItem], command: MenuCommand) -> &MenuItem {
        menu.iter()
            .find(|item| item.command == command)
            .expect("the menu has every command")
    }

    #[test]
    fn whats_new_is_disabled_until_there_are_notes() {
        let menu = menu_items(true, false, true);

        assert!(item(&menu, MenuCommand::CheckUpdates).enabled);
        assert!(!item(&menu, MenuCommand::WhatsNew).enabled);
        assert!(item(&menu, MenuCommand::Changelog).enabled);
        assert!(item(&menu, MenuCommand::Exit).enabled);

        let menu = menu_items(true, true, true);
        assert!(item(&menu, MenuCommand::WhatsNew).enabled);
    }

    #[test]
    fn update_checks_are_disabled_without_a_worker() {
        let menu = menu_items(false, true, true);

        assert!(!item(&menu, MenuCommand::CheckUpdates).enabled);
    }

    #[test]
    fn the_windows_restore_is_disabled_without_a_session() {
        assert!(!item(&menu_items(true, true, false), MenuCommand::RestoreWindows).enabled);
        assert!(item(&menu_items(true, true, true), MenuCommand::RestoreWindows).enabled);
    }

    #[test]
    fn the_menu_labels_are_the_russian_ones_the_users_expect() {
        let menu = menu_items(true, true, true);
        let labels: Vec<&str> = menu.iter().map(|item| item.label).collect();

        assert_eq!(
            labels,
            vec![
                "Настройки",
                "Проверить обновления",
                "Что нового",
                "Открыть changelog",
                "Вернуть всё как было",
                "Выход"
            ]
        );
        assert!(item(&menu, MenuCommand::Settings).enabled);
        assert!(
            item(&menu, MenuCommand::CheckUpdates).separator_before,
            "the settings row is its own group"
        );
        assert!(item(&menu, MenuCommand::RestoreWindows).separator_before);
        assert!(item(&menu, MenuCommand::Exit).separator_before);
    }

    #[test]
    fn a_long_tooltip_is_truncated_not_overflowed() {
        let mut buffer = [0u16; 8];
        copy_wide("ProjectNeb tray icon", &mut buffer);

        assert_eq!(&buffer[..7], &[80, 114, 111, 106, 101, 99, 116]);
        assert_eq!(buffer[7], 0, "the copy is NUL-terminated");
    }
}
