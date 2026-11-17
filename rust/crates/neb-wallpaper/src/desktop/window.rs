//! The Win32 half of the desktop layer: classes, windows, styles, monitors and the message pump.
//!
//! Two windows matter. The **control** window is an invisible top-level window that exists only to
//! receive the messages a child inside the desktop never sees - `WM_DISPLAYCHANGE`, the shell's
//! `TaskbarCreated` broadcast and the terminal-services session change - and to be the handle IPC
//! posts to. The **surface** windows are the ones the scene draws into; they are created as plain
//! popups and adopted by the desktop in [`super::attach`].
//!
//! Nothing here calls `SetForegroundWindow`, and every surface carries `WS_EX_NOACTIVATE`: the
//! wallpaper must never take a keystroke from the user.

use std::mem::size_of;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use windows::Win32::Foundation::{
    CloseHandle, ERROR_CLASS_ALREADY_EXISTS, GetLastError, HANDLE, HINSTANCE, HWND, LPARAM,
    LRESULT, POINT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HBRUSH, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{
    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, CreateWaitableTimerExW, INFINITE, SetWaitableTimer,
    TIMER_ALL_ACCESS,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GA_PARENT, GetAncestor,
    GetDesktopWindow, HCURSOR, HICON, HTTRANSPARENT, HWND_TOP, IsWindow, MA_NOACTIVATE,
    MONITORINFOF_PRIMARY, MSG, MsgWaitForMultipleObjects, PM_REMOVE, PeekMessageW, PostMessageW,
    QS_ALLINPUT, RegisterClassW, RegisterWindowMessageW, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_SHOWWINDOW, SetParent,
    SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE,
    WM_APP, WM_CLOSE, WM_DISPLAYCHANGE, WM_ERASEBKGND, WM_MOUSEACTIVATE, WM_NCHITTEST,
    WM_WTSSESSION_CHANGE, WNDCLASSW, WNDPROC, WS_CHILD, WS_CLIPCHILDREN, WS_EX_NOACTIVATE,
    WS_EX_TOOLWINDOW, WS_POPUP, WTS_CONSOLE_CONNECT, WTS_CONSOLE_DISCONNECT, WTS_REMOTE_CONNECT,
    WTS_REMOTE_DISCONNECT, WTS_SESSION_LOCK, WTS_SESSION_UNLOCK,
};
use windows::core::PCWSTR;

/// Class of the windows that show the scene.
pub const SURFACE_CLASS: &str = "ProjectNebWallpaper";
/// Class of the invisible control window.
pub const CONTROL_CLASS: &str = "ProjectNebWallpaperControl";

/// IPC asks the loop to re-read `config.json` without restarting.
pub const WM_RELOAD: u32 = WM_APP + 1;
/// IPC asks the loop to put the user's own wallpaper back and exit.
pub const WM_STOP: u32 = WM_APP + 2;

// The control messages must sit above `WM_APP`: everything below it is reserved by Windows, and a
// private message that collides with one would be handled by the system instead of the loop.
const _: () = assert!(WM_RELOAD > WM_APP && WM_STOP > WM_APP && WM_RELOAD != WM_STOP);

/// `GetLastError` value for "this class is already registered", which is success for us.
const CLASS_EXISTS: u32 = ERROR_CLASS_ALREADY_EXISTS.0;

/// The shell's `TaskbarCreated` broadcast, resolved once per process.
static TASKBAR_CREATED: AtomicU32 = AtomicU32::new(0);

/// Window classes can only be registered once per process.
static CLASSES: OnceLock<Result<(), String>> = OnceLock::new();

/// A screen rectangle in physical pixels, the unit every Win32 placement call uses.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RectI {
    /// Left edge, inclusive; negative on a monitor to the left of the primary one.
    pub left: i32,
    /// Top edge, inclusive.
    pub top: i32,
    /// Right edge, exclusive.
    pub right: i32,
    /// Bottom edge, exclusive.
    pub bottom: i32,
}

impl RectI {
    /// A rectangle from its edges.
    pub const fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left,
            top,
            right,
            bottom,
        }
    }

    /// Width in pixels, at least one: a zero-width window has no back buffer to present.
    pub fn width(&self) -> i32 {
        (self.right - self.left).max(1)
    }

    /// Height in pixels, at least one.
    pub fn height(&self) -> i32 {
        (self.bottom - self.top).max(1)
    }
}

impl From<RECT> for RectI {
    fn from(rect: RECT) -> Self {
        Self::new(rect.left, rect.top, rect.right, rect.bottom)
    }
}

/// One monitor: its device name and the rectangle it covers in virtual-screen coordinates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Monitor {
    /// Device name, e.g. `\\.\DISPLAY1`; the key a resize is matched by.
    pub device: String,
    /// The full monitor rectangle, in physical pixels.
    pub rect: RectI,
    /// Whether Windows calls this the primary monitor.
    pub primary: bool,
}

/// Why a window could not be created.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// The class could not be registered, so no window of it can exist.
    #[error("cannot register the wallpaper window class: {0}")]
    Register(String),

    /// `CreateWindowExW` refused the window.
    #[error("cannot create the '{name}' window: {message}")]
    Create {
        /// Which window was being created.
        name: &'static str,
        /// The Win32 message.
        message: String,
    },
}

/// What one pass over the message queue saw.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Events {
    /// `WM_QUIT`.
    pub quit: bool,
    /// `wallpaper.reload` arrived over IPC.
    pub reload: bool,
    /// `wallpaper.stop` arrived over IPC, or the control window was asked to close.
    pub stop: bool,
    /// The display configuration changed.
    pub display_changed: bool,
    /// Explorer restarted: the desktop our windows live in is gone.
    pub taskbar_recreated: bool,
    /// The session was locked (`Some(true)`) or unlocked (`Some(false)`).
    pub session_locked: Option<bool>,
}

impl Events {
    /// Whether anything happened worth reacting to.
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// Registers both window classes and resolves the shell's `TaskbarCreated` message.
///
/// # Errors
///
/// [`WindowError::Register`] when a class cannot be registered, which no window creation could
/// recover from.
pub fn ensure_classes() -> Result<(), WindowError> {
    match CLASSES.get_or_init(register_all) {
        Ok(()) => Ok(()),
        Err(message) => Err(WindowError::Register(message.clone())),
    }
}

/// The registered `TaskbarCreated` message, or zero before [`ensure_classes`] has run.
pub fn taskbar_created() -> u32 {
    TASKBAR_CREATED.load(Ordering::Relaxed)
}

/// Creates the invisible control window: zero-sized, hidden, out of Alt+Tab, unable to activate.
///
/// It is deliberately a *top-level* window even though nothing is ever shown in it: message-only
/// windows do not receive broadcasts, and `WM_DISPLAYCHANGE` and `TaskbarCreated` are broadcasts.
///
/// # Errors
///
/// [`WindowError::Create`] when Windows refuses the window.
pub fn create_control(instance: HINSTANCE) -> Result<HWND, WindowError> {
    create(
        instance,
        WindowSpec {
            name: "control",
            class: CONTROL_CLASS,
            title: "ProjectNeb wallpaper",
            ex_style: WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            style: WS_POPUP,
            rect: RectI::default(),
            parent: None,
        },
    )
}

/// Creates one surface window, hidden, sized to `rect` and belonging to no parent yet.
///
/// # Errors
///
/// [`WindowError::Create`] when Windows refuses the window.
pub fn create_surface(device: &str, rect: RectI, instance: HINSTANCE) -> Result<HWND, WindowError> {
    let title = format!("ProjectNeb wallpaper [{device}]");

    create(
        instance,
        WindowSpec {
            name: "surface",
            class: SURFACE_CLASS,
            title: &title,
            ex_style: WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            style: WS_POPUP,
            rect,
            parent: None,
        },
    )
}

/// Destroys a window if it still exists.
pub fn destroy(window: HWND) {
    if !is_empty_handle(window) && is_alive(window) {
        // SAFETY: the handle was created by this thread and is destroyed once, here.
        let _ = unsafe { DestroyWindow(window) };
    }
}

/// Whether a handle still refers to a live window.
pub fn is_alive(window: HWND) -> bool {
    // SAFETY: `IsWindow` only inspects the handle and is safe for stale values.
    unsafe { IsWindow(Some(window)) }.as_bool()
}

/// The window's parent, or zero when it has none.
///
/// Uses `GetAncestor(GA_PARENT)` rather than `GetParent`: a wallpaper window carries `WS_POPUP` as
/// well as `WS_CHILD` (that is what makes it a popup before it is adopted), and for such a window
/// `GetParent` answers with the *owner*, which is zero - the very window reports "no parent" while
/// `GetAncestor` correctly names the `WorkerW` it hangs from.
pub fn parent_of(window: HWND) -> HWND {
    // SAFETY: both calls only read window linkage; a stale handle answers zero.
    unsafe {
        let ancestor = GetAncestor(window, GA_PARENT);
        if ancestor == GetDesktopWindow() {
            HWND::default()
        } else {
            ancestor
        }
    }
}

/// The monitors that exist right now, primary first.
pub fn monitors() -> Vec<Monitor> {
    let mut found: Vec<Monitor> = Vec::with_capacity(4);
    let pointer = &mut found as *mut Vec<Monitor> as isize;

    // SAFETY: the callback only pushes into `found`, which outlives the enumeration, and the
    // rectangle argument is unused. The callback cannot unwind.
    unsafe {
        let _ = EnumDisplayMonitors(None, None, Some(collect_monitor), LPARAM(pointer));
    }

    found.sort_by(|left, right| {
        right
            .primary
            .cmp(&left.primary)
            .then_with(|| left.device.cmp(&right.device))
    });

    found
}

/// The window procedure of a surface: it never activates and it never paints.
unsafe extern "system" fn surface_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        // The wallpaper is behind the icons; a click that reaches it belongs to the desktop, so the
        // window is transparent to hit testing as well as unable to activate.
        WM_NCHITTEST => LRESULT(HTTRANSPARENT as isize),
        WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
        WM_ERASEBKGND => LRESULT(1),
        _ => {
            // SAFETY: the arguments are the ones Windows passed for this message.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

/// The window procedure of the control window: it only ever needs to close on request.
unsafe extern "system" fn control_proc(
    window: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_CLOSE => {
            // SAFETY: the handle is this window and the pump ends on the posted quit.
            unsafe {
                let _ = PostMessageW(Some(window), WM_STOP, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        _ => {
            // SAFETY: the arguments are the ones Windows passed for this message.
            unsafe { DefWindowProcW(window, message, wparam, lparam) }
        }
    }
}

/// Waits up to `timeout` for input or the next frame, then drains the queue.
///
/// The wait happens in the kernel, which is what makes a paused wallpaper cost nothing: with the
/// frame timer stopped and a one-second pause poll, this call sleeps for that second.
///
/// When `timer` is given the sleep is armed on it, which is what turns "wait about 33 ms" into a
/// frame that actually lands 33 ms later; a plain message wait is quantised to the system clock
/// tick and would quietly deliver the scene at two-thirds of its frame rate.
pub fn pump(timeout: Duration, timer: Option<&FrameTimer>) -> Events {
    let mut events = Events::default();
    let armed = timer.is_some_and(|timer| timer.arm(timeout));

    // SAFETY: with no handles the null array is valid; with one handle the slice borrows a timer
    // this process owns. `QS_ALLINPUT` is the documented "wake for any message" mask.
    unsafe {
        if armed {
            let handle = timer.map_or(HANDLE::default(), FrameTimer::handle);
            let handles = [handle];

            MsgWaitForMultipleObjects(Some(&handles), false, INFINITE, QS_ALLINPUT);
        } else {
            let milliseconds = u32::try_from(timeout.as_millis()).unwrap_or(u32::MAX - 1);
            MsgWaitForMultipleObjects(None, false, milliseconds.max(1), QS_ALLINPUT);
        }
    }

    let mut message = MSG::default();
    loop {
        // SAFETY: `message` is a live, writable `MSG`; `None` means "any window of this thread".
        let more = unsafe { PeekMessageW(&mut message, None, 0, 0, PM_REMOVE) };
        if !more.as_bool() {
            break;
        }

        if message.message == windows::Win32::UI::WindowsAndMessaging::WM_QUIT {
            events.quit = true;
            continue;
        }

        match message.message {
            WM_DISPLAYCHANGE => events.display_changed = true,
            WM_RELOAD => events.reload = true,
            WM_STOP => events.stop = true,
            WM_WTSSESSION_CHANGE => {
                events.session_locked = Some(match message.wParam.0 as u32 {
                    WTS_SESSION_LOCK | WTS_CONSOLE_DISCONNECT | WTS_REMOTE_DISCONNECT => true,
                    WTS_SESSION_UNLOCK | WTS_CONSOLE_CONNECT | WTS_REMOTE_CONNECT => false,
                    _ => {
                        continue;
                    }
                });
            }
            other if other == taskbar_created() && taskbar_created() != 0 => {
                events.taskbar_recreated = true;
            }
            _ => {
                // SAFETY: the message came from this thread's queue; translation and dispatch are
                // what let `DefWindowProcW` answer the paint and style messages we do not handle.
                unsafe {
                    let _ = TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
            }
        }
    }

    events
}

/// A high-resolution waitable timer the loop sleeps on between frames.
///
/// A wallpaper draws at 30 fps, not at the monitor's rate, so the frame clock has to exist
/// independently of the compositor; the high-resolution timer is the cheapest thing that keeps it
/// within a millisecond without raising the global system timer resolution.
#[derive(Debug)]
pub struct FrameTimer {
    handle: HANDLE,
}

impl FrameTimer {
    /// Creates the timer, or reports that this system will not give us one.
    pub fn new() -> Option<Self> {
        // SAFETY: no name and no security attributes are wanted; the flag asks for the
        // high-resolution variant, which falls back to a normal timer on older builds.
        let handle = unsafe {
            CreateWaitableTimerExW(
                None,
                PCWSTR::null(),
                CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                TIMER_ALL_ACCESS.0,
            )
        };

        match handle {
            Ok(handle) => Some(Self { handle }),
            Err(_) => None,
        }
    }

    /// The raw handle, for the wait.
    pub fn handle(&self) -> HANDLE {
        self.handle
    }

    /// Arms the timer for `after` from now.
    fn arm(&self, after: Duration) -> bool {
        // `SetWaitableTimer` counts 100 ns units and a negative value means "relative".
        let due = -i64::try_from(after.as_nanos() / 100)
            .unwrap_or(i64::MAX / 2)
            .max(1);

        // SAFETY: the timer handle is live and the due time is a valid relative value; no completion
        // routine is passed, so the call cannot re-enter this thread.
        unsafe { SetWaitableTimer(self.handle, &due, 0, None, None, false) }.is_ok()
    }
}

impl Drop for FrameTimer {
    fn drop(&mut self) {
        // SAFETY: the handle is owned by this value and closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Makes `window` a real child of `parent`.
///
/// `SetParent` does not touch the window style, and a `WS_POPUP` window that merely has a parent
/// still answers `GetParent` with its owner - zero - so the style goes on first and the result is
/// verified by asking again.
///
/// # Errors
///
/// The Win32 reason, so a refused attach is logged with the code that refused it rather than a bare
/// "failed".
pub fn adopt(window: HWND, parent: HWND) -> Result<(), String> {
    make_child(window)?;

    // SAFETY: both handles are this thread's windows. The previous parent is not needed.
    let previous = unsafe { SetParent(window, Some(parent)) };
    if let Err(error) = previous {
        return Err(format!("SetParent: {}", error.message()));
    }

    let actual = parent_of(window);
    if actual != parent {
        return Err(format!(
            "GetParent returned 0x{:x} instead of 0x{:x}",
            actual.0 as usize, parent.0 as usize
        ));
    }

    Ok(())
}

/// Detaches `window` from the desktop and hides it, so it cannot flash on screen while it dies.
pub fn release(window: HWND) {
    if is_empty_handle(window) || !is_alive(window) || is_empty_handle(parent_of(window)) {
        return;
    }

    // SAFETY: the handle is a live window of this thread; it has not been destroyed yet.
    unsafe {
        let _ = ShowWindow(window, SW_HIDE);
        let _ = SetParent(window, None);
    }

    make_top_level(window);
}

/// Adds `WS_CLIPCHILDREN` to `window`.
///
/// The 24H2 wallpaper layer is a child of a window without a redirection bitmap, and until it clips
/// its children DWM keeps showing the parent's snapshot of the desktop wallpaper instead of what
/// the child presents.
pub fn clip_children(window: HWND) {
    let _ = set_style_bits(window, WS_CLIPCHILDREN.0 as isize, true);
}

/// Makes the window a child, which `SetParent` alone never does.
/// Makes the window a child, which `SetParent` alone never does.
fn make_child(window: HWND) -> Result<(), String> {
    set_style_bits(window, WS_CHILD.0 as isize, true)
}

/// Clears `WS_CHILD` before the window is destroyed as a top-level one.
fn make_top_level(window: HWND) {
    let _ = set_style_bits(window, WS_CHILD.0 as isize, false);
}

/// Turns style bit `bit` on or off in the window's `GWL_STYLE`.
fn set_style_bits(window: HWND, bit: isize, on: bool) -> Result<(), String> {
    // SAFETY: `GetWindowLongPtrW`/`SetWindowLongPtrW` only read and write this window's style.
    unsafe {
        let style = windows::Win32::UI::WindowsAndMessaging::GetWindowLongPtrW(
            window,
            windows::Win32::UI::WindowsAndMessaging::GWL_STYLE,
        );
        let updated = if on { style | bit } else { style & !bit };

        if updated != style {
            let previous = SetWindowLongPtrW(
                window,
                windows::Win32::UI::WindowsAndMessaging::GWL_STYLE,
                updated,
            );
            // A zero return means "the previous value was zero" or "it failed"; only the error code
            // tells them apart, and a style that never changed would make the attach fail silently.
            if previous == 0 {
                let code = GetLastError().0;
                if code != 0 {
                    return Err(format!(
                        "SetWindowLongPtrW(style) failed: win32 error {code}"
                    ));
                }
            }
        }
    }

    Ok(())
}

/// Places `window` at `rect` in **screen** coordinates, as a child of `parent`.
///
/// After `SetParent` a window's coordinates are relative to its parent's client area, and the
/// desktop does not always start at (0, 0) - a monitor to the left of the primary one has a negative
/// origin - so the rectangle is mapped through the parent instead of copied.
pub fn place_in_parent(window: HWND, parent: HWND, rect: RectI) -> bool {
    let mut points = [
        POINT {
            x: rect.left,
            y: rect.top,
        },
        POINT {
            x: rect.right,
            y: rect.bottom,
        },
    ];

    if !is_empty_handle(parent) {
        // SAFETY: two points are passed with the matching count; both handles are this thread's.
        let _ = unsafe {
            windows::Win32::Graphics::Gdi::MapWindowPoints(None, Some(parent), &mut points[..])
        };
    }

    let width = (points[1].x - points[0].x).max(1);
    let height = (points[1].y - points[0].y).max(1);

    // SAFETY: the target is a live window and the insert-after is `HWND_TOP`, the documented
    // top of the sibling order.
    unsafe {
        SetWindowPos(
            window,
            Some(HWND_TOP),
            points[0].x,
            points[0].y,
            width,
            height,
            SWP_NOACTIVATE | SWP_SHOWWINDOW,
        )
    }
    .is_ok()
}

/// Puts `window` directly behind its sibling `reference`, without moving or resizing either.
///
/// The 24H2 wallpaper layer is a *sibling* of the icon view, so a layer left above it covers every
/// shortcut on the desktop.
pub fn place_behind(window: HWND, reference: HWND) -> bool {
    if is_empty_handle(window) || is_empty_handle(reference) {
        return false;
    }

    // SAFETY: both windows are children of the same parent on this thread; nothing is moved.
    unsafe {
        SetWindowPos(
            window,
            Some(reference),
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE | SWP_NOOWNERZORDER,
        )
    }
    .is_ok()
}

/// The last resort when the desktop offers no slot: a bottom-most, never-activating top-level window.
pub fn place_at_bottom(window: HWND, rect: RectI) -> bool {
    // SAFETY: the window is ours and `HWND_BOTTOM` is the documented bottom of the Z order.
    unsafe {
        SetWindowPos(
            window,
            Some(windows::Win32::UI::WindowsAndMessaging::HWND_BOTTOM),
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            SWP_NOACTIVATE | SWP_SHOWWINDOW | SWP_NOOWNERZORDER,
        )
    }
    .is_ok()
}

/// Hides and re-shows the icon view.
///
/// The hide/show pair makes DWM take a fresh snapshot of the layer below the icons, which is the
/// composition our window joined; without it the desktop keeps showing the pre-attach snapshot. The
/// icons flicker for a frame - that is the documented cost of the 24H2 recipe.
pub fn refresh_desktop_view(desktop_view: HWND) -> bool {
    if is_empty_handle(desktop_view) {
        return false;
    }

    // SAFETY: the handle is the live `SHELLDLL_DefView` child of `Progman`; both calls only change
    // its visibility.
    unsafe {
        let _ = ShowWindow(desktop_view, SW_HIDE);
        let _ = ShowWindow(
            desktop_view,
            windows::Win32::UI::WindowsAndMessaging::SW_SHOWNOACTIVATE,
        );
    }

    true
}

/// Shows a window without activating it.
pub fn show(window: HWND) {
    // SAFETY: `SW_SHOWNOACTIVATE` cannot take the foreground.
    unsafe {
        let _ = ShowWindow(window, SW_SHOWNOACTIVATE);
    }
}

/// Sends `message` to `window` without waiting, which is how IPC talks to the loop thread.
pub fn post(window: HWND, message: u32, payload: usize) -> bool {
    if is_empty_handle(window) {
        return false;
    }

    // SAFETY: the handle is the control window; posting cannot block or fail the caller.
    unsafe { PostMessageW(Some(window), message, WPARAM(payload), LPARAM(0)) }.is_ok()
}

/// The instance handle a window class is registered against.
pub fn instance() -> HINSTANCE {
    // SAFETY: `GetModuleHandleW(None)` returns this executable's module, which always exists while
    // the process runs.
    match unsafe { GetModuleHandleW(None) } {
        Ok(module) => HINSTANCE(module.0),
        Err(_) => HINSTANCE::default(),
    }
}

/// Registers both classes and resolves `TaskbarCreated`.
fn register_all() -> Result<(), String> {
    let instance = instance();
    register_class(SURFACE_CLASS, Some(surface_proc), instance)?;
    register_class(CONTROL_CLASS, Some(control_proc), instance)?;

    let name = wide("TaskbarCreated");
    // SAFETY: the name is a NUL-terminated UTF-16 buffer that outlives the call.
    let message = unsafe { RegisterWindowMessageW(PCWSTR(name.as_ptr())) };
    TASKBAR_CREATED.store(message, Ordering::Release);

    Ok(())
}

/// Registers one class, tolerating "already registered" from a second call in the same process.
fn register_class(class: &str, procedure: WNDPROC, instance: HINSTANCE) -> Result<(), String> {
    let name = wide(class);
    let description = WNDCLASSW {
        style: windows::Win32::UI::WindowsAndMessaging::WNDCLASS_STYLES(0),
        lpfnWndProc: procedure,
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: HICON::default(),
        hCursor: HCURSOR::default(),
        // No background brush: the scene is presented by DXGI, and erasing the window on every
        // paint would be a second, blank frame behind it.
        hbrBackground: HBRUSH::default(),
        lpszMenuName: PCWSTR::null(),
        lpszClassName: PCWSTR(name.as_ptr()),
    };

    // SAFETY: the description points at `name`, which outlives the call.
    if unsafe { RegisterClassW(&description) } == 0 {
        // SAFETY: `GetLastError` reads this thread's last error and changes nothing.
        let code = unsafe { GetLastError() }.0;
        if code != CLASS_EXISTS {
            return Err(format!("win32 error {code}"));
        }
    }

    Ok(())
}

/// What one window is created from; every creation in this module goes through [`create`].
struct WindowSpec<'a> {
    /// Which window this is, for the error message.
    name: &'static str,
    /// Registered class name.
    class: &'a str,
    /// Window title: it never shows, but it is what a debugger lists.
    title: &'a str,
    /// Extended style.
    ex_style: WINDOW_EX_STYLE,
    /// Style.
    style: WINDOW_STYLE,
    /// Where the window is born.
    rect: RectI,
    /// Parent, when the window is born inside another one.
    parent: Option<HWND>,
}

/// Creates one window from a [`WindowSpec`].
fn create(instance: HINSTANCE, spec: WindowSpec<'_>) -> Result<HWND, WindowError> {
    ensure_classes()?;

    let class = wide(spec.class);
    let title = wide(spec.title);

    // SAFETY: both strings outlive the call and are NUL-terminated; the parent, when given, is a
    // window of this thread.
    unsafe {
        CreateWindowExW(
            spec.ex_style,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            spec.style,
            spec.rect.left,
            spec.rect.top,
            spec.rect.width(),
            spec.rect.height(),
            spec.parent,
            None,
            Some(instance),
            None,
        )
    }
    .map_err(|source| WindowError::Create {
        name: spec.name,
        message: source.message(),
    })
}

/// One monitor, pushed into the vector the enumeration was started with.
unsafe extern "system" fn collect_monitor(
    monitor: HMONITOR,
    _device_context: HDC,
    _rect: *mut RECT,
    data: LPARAM,
) -> windows::core::BOOL {
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;

    // SAFETY: `data` is the `&mut Vec<Monitor>` the enumeration is filling, and it outlives the
    // callback. `GetMonitorInfoW` writes into `info`, which `MONITORINFOEXW` starts with.
    unsafe {
        let found = &mut *(data.0 as *mut Vec<Monitor>);

        if GetMonitorInfoW(
            monitor,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        )
        .as_bool()
        {
            found.push(Monitor {
                device: from_wide(&info.szDevice),
                rect: RectI::from(info.monitorInfo.rcMonitor),
                primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
    }

    true.into()
}

/// A NUL-terminated UTF-16 buffer, the form every `*W` call wants.
pub fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The text of a fixed-size UTF-16 buffer, up to its first NUL.
pub fn from_wide(buffer: &[u16]) -> String {
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());

    String::from_utf16_lossy(&buffer[..end])
}

/// Whether a handle is the null handle, i.e. "no window".
///
/// Spelled out here rather than through a handle helper so the answer cannot change with a binding
/// update: every Win32 call in this module already treats zero as "none".
pub fn is_empty_handle(window: HWND) -> bool {
    window.0.is_null()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rectangle_is_never_smaller_than_one_pixel() {
        let empty = RectI::new(10, 10, 10, 10);

        assert_eq!(empty.width(), 1);
        assert_eq!(empty.height(), 1);
        assert_eq!(RectI::new(-1920, 0, 0, 1080).width(), 1920);
        assert_eq!(RectI::new(-1920, 0, 0, 1080).left, -1920);
    }

    #[test]
    fn a_wide_string_round_trips_and_stops_at_the_nul() {
        let buffer = wide("\\\\.\\DISPLAY1");

        assert_eq!(buffer.last(), Some(&0));
        assert_eq!(from_wide(&buffer), "\\\\.\\DISPLAY1");
        assert_eq!(from_wide(&[0x41, 0, 0x42]), "A");
        assert_eq!(from_wide(&[]), "");
    }

    #[test]
    fn an_empty_pass_over_the_queue_reports_nothing() {
        assert!(Events::default().is_empty());
        assert!(
            !Events {
                display_changed: true,
                ..Events::default()
            }
            .is_empty()
        );
    }
}
