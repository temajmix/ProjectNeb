//! The settings window: a plain framed Win32 window with a dark title bar.
//!
//! The window is deliberately a normal `WS_OVERLAPPEDWINDOW`: the dashboard is a web page, the
//! shell already owns the frameless surface, and a real frame gives the user resize borders,
//! snap layouts and a system menu for free. The only cosmetic work is `DWMWA_USE_IMMERSIVE_DARK_MODE`
//! so the caption matches the dark page instead of flashing white.
//!
//! `WM_NCCREATE` takes ownership of a boxed [`Setup`] and builds the window state the rest of the
//! procedure needs; `WM_NCDESTROY` drops it, which is what releases the WebView2 controller.

use std::ffi::c_void;
use std::mem::size_of;

use neb_core::config::{ConfigStore, WindowSettings};
use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GWLP_USERDATA,
    GetClientRect, GetMessageW, GetWindowLongPtrW, GetWindowRect, IDC_ARROW, IsIconic, IsZoomed,
    LoadCursorW, MINMAXINFO, MSG, PostMessageW, PostQuitMessage, RegisterClassW, SPI_GETWORKAREA,
    SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, SW_SHOW, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS,
    SetForegroundWindow, SetWindowLongPtrW, ShowWindow, SystemParametersInfoW, TranslateMessage,
    WM_APP, WM_CLOSE, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_GETMINMAXINFO, WM_MOVE,
    WM_NCCREATE, WM_NCDESTROY, WM_SIZE, WNDCLASSW, WS_OVERLAPPEDWINDOW,
};
use windows::core::PCWSTR;

use crate::webview::WebView;

/// Window class this process registers.
pub const CLASS_NAME: &str = "ProjectNeb.Settings.Rust";

/// Caption text; the dashboard draws its own header, so this is only for the taskbar and Alt+Tab.
pub const TITLE: &str = "ProjectNeb";

/// Size a fresh window opens at, in device-independent pixels.
pub const DEFAULT_WIDTH: f64 = 1100.0;
/// See [`DEFAULT_WIDTH`].
pub const DEFAULT_HEIGHT: f64 = 720.0;

/// Smallest size the window may be dragged to, in device-independent pixels.
pub const MIN_WIDTH: f64 = 900.0;
/// See [`MIN_WIDTH`].
pub const MIN_HEIGHT: f64 = 600.0;

/// Message the bridge posts to minimize the window.
pub const MINIMIZE: u32 = WM_APP + 10;
/// Message the bridge posts to close the window.
pub const CLOSE: u32 = WM_APP + 11;
/// Message the bridge posts to toggle maximize, which the dashboard's title button uses.
pub const TOGGLE_MAXIMIZE: u32 = WM_APP + 12;
/// Message carrying one bridge reply for the UI thread to post to the page.
///
/// WebView2's controller is apartment-threaded (`PostWebMessageAsJson` from the worker fails with
/// `E_INVALID_STATE`), so the worker hands the finished JSON to the window procedure instead.
pub const REPLY: u32 = WM_APP + 13;

/// Where the window should sit, in device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    /// Left edge.
    pub left: f64,
    /// Top edge.
    pub top: f64,
    /// Width.
    pub width: f64,
    /// Height.
    pub height: f64,
}

impl Placement {
    /// The same rectangle in physical pixels for a window on a monitor at `scale`.
    pub fn to_physical(self, scale: f64) -> (i32, i32, i32, i32) {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };

        (
            (self.left * scale).round() as i32,
            (self.top * scale).round() as i32,
            (self.width * scale).round() as i32,
            (self.height * scale).round() as i32,
        )
    }
}

/// The desktop area a window may use, in device-independent pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Screen {
    /// Left edge of the virtual desktop.
    pub left: f64,
    /// Top edge of the virtual desktop.
    pub top: f64,
    /// Width of the virtual desktop.
    pub width: f64,
    /// Height of the virtual desktop.
    pub height: f64,
}

impl Screen {
    /// The work area of the primary monitor, in DIPs.
    pub fn primary(scale: f64) -> Self {
        let mut rect = RECT::default();
        // SAFETY: `rect` is a valid out-parameter for the work area of this process's desktop.
        let queried = unsafe {
            SystemParametersInfoW(
                SPI_GETWORKAREA,
                0,
                Some((&raw mut rect).cast()),
                SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
            )
        }
        .is_ok();

        if !queried || rect.right <= rect.left || rect.bottom <= rect.top {
            return Self {
                left: 0.0,
                top: 0.0,
                width: DEFAULT_WIDTH.max(MIN_WIDTH),
                height: DEFAULT_HEIGHT.max(MIN_HEIGHT),
            };
        }

        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };

        Self {
            left: f64::from(rect.left) / scale,
            top: f64::from(rect.top) / scale,
            width: f64::from(rect.right - rect.left) / scale,
            height: f64::from(rect.bottom - rect.top) / scale,
        }
    }
}

/// Turns stored bounds into a rectangle that is guaranteed to be reachable.
///
/// A window that was never placed (both `left` and `top` are `None`) opens at the default size,
/// centred. A placed window restores its size and position, clamped so an unplugged monitor cannot
/// leave it off-screen. Sizes are sanitised before use: `0`, `NaN` and `Infinity` are configuration
/// corruption, not a request for a one-pixel window.
pub fn resolve(settings: &WindowSettings, screen: Screen) -> Placement {
    let screen = if screen.width.is_finite()
        && screen.height.is_finite()
        && screen.width > 0.0
        && screen.height > 0.0
    {
        screen
    } else {
        Screen {
            left: 0.0,
            top: 0.0,
            width: DEFAULT_WIDTH.max(MIN_WIDTH),
            height: DEFAULT_HEIGHT.max(MIN_HEIGHT),
        }
    };

    let placed = settings.left.is_some() && settings.top.is_some();

    let width = if placed {
        sane(settings.width, DEFAULT_WIDTH)
    } else {
        DEFAULT_WIDTH
    }
    .clamp(MIN_WIDTH, MIN_WIDTH.max(screen.width));

    let height = if placed {
        sane(settings.height, DEFAULT_HEIGHT)
    } else {
        DEFAULT_HEIGHT
    }
    .clamp(MIN_HEIGHT, MIN_HEIGHT.max(screen.height));

    let left = match settings.left.filter(|value| value.is_finite()) {
        Some(left) if placed => clamp_into(
            left,
            screen.left,
            screen.left + (screen.width - width).max(0.0),
        ),
        _ => screen.left + ((screen.width - width) / 2.0).max(0.0),
    };

    let top = match settings.top.filter(|value| value.is_finite()) {
        Some(top) if placed => clamp_into(
            top,
            screen.top,
            screen.top + (screen.height - height).max(0.0),
        ),
        _ => screen.top + ((screen.height - height) / 2.0).max(0.0),
    };

    Placement {
        left,
        top,
        width,
        height,
    }
}

/// A finite positive measurement, or `fallback`.
fn sane(value: f64, fallback: f64) -> f64 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        fallback
    }
}

/// `value` inside `[low, high]`, tolerating an inverted range.
fn clamp_into(value: f64, low: f64, high: f64) -> f64 {
    if high < low {
        low
    } else {
        value.clamp(low, high)
    }
}

/// Converts a screen rectangle into device-independent bounds, or `None` when it is unusable.
///
/// The rectangle must be in *screen* pixels (`GetWindowRect`), not in `GetWindowPlacement`'s
/// workspace coordinates: those are relative to the primary monitor's work-area origin, and feeding
/// them to `CreateWindowExW` moved the window up by the bar's reservation on every start.
pub fn placement(rect: RECT, maximized: bool, scale: f64) -> Option<(Placement, bool)> {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };

    let width = f64::from(rect.right - rect.left) / scale;
    let height = f64::from(rect.bottom - rect.top) / scale;

    if !(width.is_finite() && height.is_finite()) || width <= 0.0 || height <= 0.0 {
        return None;
    }

    Some((
        Placement {
            left: f64::from(rect.left) / scale,
            top: f64::from(rect.top) / scale,
            width,
            height,
        },
        maximized,
    ))
}

/// What the window procedure needs; it crosses `CreateWindowExW` as the create parameter.
#[derive(Debug)]
struct Setup {
    store: ConfigStore,
    scale: f64,
}

/// The per-window state, owned by the window procedure.
#[derive(Debug)]
struct State {
    store: ConfigStore,
    scale: f64,
    webview: Option<WebView>,
    /// The last rectangle the window had while it was neither maximized nor minimized, in screen
    /// pixels. Saving this instead of asking Windows on close is what keeps a maximized window's
    /// restore size, without `GetWindowPlacement`'s workspace coordinates (which are offset by the
    /// taskbar and by ProjectNeb's own reserved bar).
    normal: Option<RECT>,
    /// Set once the bounds have been written, so a second `WM_CLOSE` cannot save a zero rectangle.
    saved: bool,
}

/// A live settings window.
///
/// The state lives in the window procedure's user data, not here, so dropping this value does not
/// tear the window down; [`Window::wait`] returns when the window is gone.
#[derive(Debug)]
pub struct Window {
    hwnd: HWND,
}

/// Why the window could not be created.
#[derive(Debug, thiserror::Error)]
pub enum WindowError {
    /// `RegisterClassW` refused the class, usually because it was already taken.
    #[error("cannot register the window class '{CLASS_NAME}'")]
    Class,
    /// `CreateWindowExW` refused the window.
    #[error("cannot create a '{CLASS_NAME}' window: {0}")]
    Create(#[source] windows::core::Error),
}

/// Makes this process DPI aware before any window exists.
///
/// Returns whether the call succeeded; per-monitor-v2 awareness is refused on very old Windows, and
/// the window still works at system DPI there.
pub fn enable_dpi_awareness() -> bool {
    // SAFETY: the call takes a constant context and applies it to this process only.
    unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) }.is_ok()
}

/// The scale factor of the primary monitor, e.g. `1.5` at 150 %.
pub fn system_scale() -> f64 {
    // SAFETY: reads this thread's desktop DPI; no arguments, no ownership.
    let dpi = unsafe { GetDpiForSystem() }.max(96);

    f64::from(dpi) / 96.0
}

/// Creates the window and shows it.
///
/// # Errors
///
/// [`WindowError::Class`] when the class cannot be registered, [`WindowError::Create`] when the
/// window cannot be created.
pub fn create(settings: &WindowSettings, store: ConfigStore) -> Result<Window, WindowError> {
    let scale = system_scale();
    let placement = resolve(settings, Screen::primary(scale));
    let (left, top, width, height) = placement.to_physical(scale);

    register_class()?;

    let class = wide(CLASS_NAME);
    let title = wide(TITLE);
    let setup = Box::into_raw(Box::new(Setup { store, scale }));

    // SAFETY: both strings are NUL-terminated and outlive the call; `setup` is a heap value that
    // `WM_NCCREATE` takes ownership of, or that is reclaimed below when the call fails.
    let created = unsafe {
        CreateWindowExW(
            Default::default(),
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_OVERLAPPEDWINDOW,
            left,
            top,
            width,
            height,
            None,
            None,
            Some(instance()),
            Some(setup.cast()),
        )
    };

    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(source) => {
            // `WM_NCCREATE` never ran, so the setup is still ours to free.
            drop(unsafe { Box::from_raw(setup) });
            return Err(WindowError::Create(source));
        }
    };

    // SAFETY: `hwnd` is a live window of this process, created on this thread.
    unsafe {
        dark_title_bar(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
    }

    Ok(Window { hwnd })
}

impl Window {
    /// The raw handle, which the WebView needs.
    pub fn hwnd(&self) -> HWND {
        self.hwnd
    }

    /// The handle as an `isize`, the form [`crate::bridge::Context`] carries across threads.
    pub fn raw(&self) -> isize {
        self.hwnd.0 as isize
    }

    /// Puts `webview` inside the window and gives it the current client rectangle.
    ///
    /// `None` means WebView2 could not start; the window then shows a message box instead of a
    /// blank page, and this method is not called.
    pub fn attach(&self, webview: WebView) {
        let mut rect = RECT::default();
        // SAFETY: `hwnd` is live and `rect` is a valid out-parameter.
        let sized = unsafe { GetClientRect(self.hwnd, &mut rect) }.is_ok();
        let webview = webview;
        if sized {
            webview.set_bounds(rect);
        }
        webview.set_visible(true);

        if let Some(state) = state_of(self.hwnd) {
            state.webview = Some(webview);
        }
    }

    /// Runs the message loop until the window is destroyed.
    ///
    /// Returns the process's exit code: `0` for an ordinary close.
    pub fn wait() -> i32 {
        let mut message = MSG::default();

        loop {
            // SAFETY: `message` is a valid out-parameter; a `None` window means "this thread".
            let result = unsafe { GetMessageW(&mut message, None, 0, 0) };
            if result.0 <= 0 {
                return i32::try_from(message.wParam.0).unwrap_or(0);
            }

            // SAFETY: the message belongs to this thread and was filled by `GetMessageW`.
            unsafe {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

/// Saves the window rectangle into `config.json`, merging with whatever the file holds now.
fn save_bounds(hwnd: HWND, state: &mut State) {
    if state.saved {
        return;
    }
    state.saved = true;

    // A maximized window reports the screen rectangle, so the last normal rectangle is what has to
    // be remembered; a window that was never resized falls back to the current one.
    let rect = match state.normal {
        Some(rect) => rect,
        None => {
            let mut rect = RECT::default();
            // SAFETY: `hwnd` is live and `rect` is a valid out-parameter.
            if unsafe { GetWindowRect(hwnd, &mut rect) }.is_err() {
                return;
            }
            rect
        }
    };

    // SAFETY: `hwnd` is live; `IsZoomed` only reads its placement.
    let maximized = unsafe { IsZoomed(hwnd).as_bool() };

    let Some((placement, maximized)) = placement(rect, maximized, state.scale) else {
        return;
    };

    let mut config = state.store.load();
    config.window = WindowSettings {
        left: Some(placement.left),
        top: Some(placement.top),
        width: placement.width,
        height: placement.height,
        maximized,
    };

    let _ = state.store.save(&config);
}

/// Remembers the current rectangle, but only while the window is in its normal state.
fn remember_bounds(hwnd: HWND, state: &mut State) {
    // SAFETY: `hwnd` is live; both queries only read its placement.
    if unsafe { IsZoomed(hwnd).as_bool() || IsIconic(hwnd).as_bool() } {
        return;
    }

    let mut rect = RECT::default();
    // SAFETY: `hwnd` is live and `rect` is a valid out-parameter.
    if unsafe { GetWindowRect(hwnd, &mut rect) }.is_ok()
        && rect.right > rect.left
        && rect.bottom > rect.top
    {
        state.normal = Some(rect);
    }
}

/// The state pointer `WM_NCCREATE` stored, if the window is one of ours.
fn state_of(hwnd: HWND) -> Option<&'static mut State> {
    // SAFETY: reads the user-data slot of a live window created on this thread.
    let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut State;

    // SAFETY: the pointer is either null or the box `WM_NCCREATE` leaked into the slot; it stays
    // valid until `WM_NCDESTROY` clears the slot and frees it.
    unsafe { pointer.as_mut() }
}

/// `DwmSetWindowAttribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE, TRUE)`.
fn dark_title_bar(hwnd: HWND) {
    let enabled: i32 = 1;

    // SAFETY: the attribute expects a four-byte BOOL and the handle is a live window.
    unsafe {
        let _ = DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE,
            (&raw const enabled).cast::<c_void>(),
            size_of::<i32>() as u32,
        );
    }
}

/// The window procedure.
unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_NCCREATE => {
            let create = lparam.0 as *const CREATESTRUCTW;
            if create.is_null() {
                return LRESULT(0);
            }

            // SAFETY: `create` is the structure `CreateWindowExW` filled in for this window.
            let setup = unsafe { (*create).lpCreateParams } as *mut Setup;
            if setup.is_null() {
                return LRESULT(0);
            }

            // `create` boxed the setup and hands it over here; nothing else owns it.
            let setup = unsafe { *Box::from_raw(setup) };
            let state = Box::new(State {
                store: setup.store,
                scale: setup.scale,
                webview: None,
                normal: None,
                saved: false,
            });

            // SAFETY: `hwnd` is the window being created on this thread.
            unsafe {
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
            }

            LRESULT(1)
        }
        WM_NCDESTROY => {
            // SAFETY: the slot belongs to this window; clearing it keeps a stale pointer from
            // outliving the box that is freed below.
            let pointer = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0) } as *mut State;

            if !pointer.is_null() {
                // Dropping the state releases the WebView2 controller, which is the whole point:
                // a closed window must leave no `msedgewebview2` process behind.
                drop(unsafe { Box::from_raw(pointer) });
            }

            LRESULT(0)
        }
        WM_GETMINMAXINFO => {
            // SAFETY: `lparam` is the `MINMAXINFO` Windows passes for this message.
            let info = unsafe { &mut *(lparam.0 as *mut MINMAXINFO) };
            let scale = state_of(hwnd).map_or(1.0, |state| state.scale);
            let (_, _, min_width, min_height) = Placement {
                left: 0.0,
                top: 0.0,
                width: MIN_WIDTH,
                height: MIN_HEIGHT,
            }
            .to_physical(scale);

            info.ptMinTrackSize.x = min_width;
            info.ptMinTrackSize.y = min_height;

            LRESULT(0)
        }
        WM_SIZE => {
            if let Some(state) = state_of(hwnd) {
                remember_bounds(hwnd, state);

                if let Some(webview) = &state.webview {
                    let mut rect = RECT::default();
                    // SAFETY: `hwnd` is live and `rect` is a valid out-parameter.
                    if unsafe { GetClientRect(hwnd, &mut rect) }.is_ok()
                        && rect.right > 0
                        && rect.bottom > 0
                    {
                        webview.set_bounds(rect);
                    }
                }
            }

            LRESULT(0)
        }
        WM_MOVE => {
            if let Some(state) = state_of(hwnd) {
                remember_bounds(hwnd, state);
            }

            LRESULT(0)
        }
        WM_DPICHANGED => {
            if let Some(state) = state_of(hwnd) {
                // SAFETY: `hwnd` is live; the DPI of a live window is always at least 96.
                let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
                state.scale = f64::from(dpi) / 96.0;

                // Windows suggests the rectangle the window should take at the new scale.
                // SAFETY: `lparam` is the suggested `RECT` for this message.
                let suggested = unsafe { &*(lparam.0 as *const RECT) };
                // SAFETY: the suggested rectangle is a well-formed screen rectangle.
                unsafe {
                    let _ = windows::Win32::UI::WindowsAndMessaging::SetWindowPos(
                        hwnd,
                        None,
                        suggested.left,
                        suggested.top,
                        suggested.right - suggested.left,
                        suggested.bottom - suggested.top,
                        windows::Win32::UI::WindowsAndMessaging::SWP_NOZORDER
                            | windows::Win32::UI::WindowsAndMessaging::SWP_NOACTIVATE,
                    );
                }
            }

            LRESULT(0)
        }
        WM_CLOSE => {
            if let Some(state) = state_of(hwnd) {
                save_bounds(hwnd, state);
            }

            // SAFETY: `hwnd` is live; destroying it posts `WM_DESTROY` and, through it, the quit.
            unsafe {
                let _ = DestroyWindow(hwnd);
            }

            LRESULT(0)
        }
        WM_DESTROY => {
            // SAFETY: posts the thread's quit message; the loop is the only reader.
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        WM_ERASEBKGND => {
            // The WebView2 controller paints the client area; erasing it here would flash the
            // window class brush between frames.
            LRESULT(1)
        }
        MINIMIZE => {
            // SAFETY: `hwnd` is live and the command is a documented constant.
            unsafe {
                let _ = ShowWindow(hwnd, SW_MINIMIZE);
            }
            LRESULT(0)
        }
        TOGGLE_MAXIMIZE => {
            // SAFETY: `hwnd` is live; `IsZoomed` only reads its placement.
            let maximized =
                unsafe { windows::Win32::UI::WindowsAndMessaging::IsZoomed(hwnd).as_bool() };

            // SAFETY: `hwnd` is live and the commands are documented constants.
            unsafe {
                let _ = ShowWindow(hwnd, if maximized { SW_RESTORE } else { SW_MAXIMIZE });
            }
            LRESULT(0)
        }
        CLOSE => {
            // Posting `WM_CLOSE` (instead of destroying) keeps the bounds save and the WebView
            // teardown in one place.
            // SAFETY: `hwnd` is live and `WM_CLOSE` carries no data.
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
            LRESULT(0)
        }
        REPLY => {
            // `post_reply` boxed the JSON for this message; the box is reclaimed here, whether or
            // not the webview is still alive to take it.
            let pointer = lparam.0 as *mut String;

            if !pointer.is_null() {
                // SAFETY: the pointer is the box `post_reply` leaked for this one message.
                let message = unsafe { *Box::from_raw(pointer) };

                if let Some(state) = state_of(hwnd)
                    && let Some(webview) = &state.webview
                {
                    let _ = webview.post_json(&message);
                }
            }

            LRESULT(0)
        }
        _ => {
            // SAFETY: every unmatched message belongs to `DefWindowProcW`.
            unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
        }
    }
}

/// Registers [`CLASS_NAME`] once per process.
fn register_class() -> Result<(), WindowError> {
    static REGISTERED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

    let registered = *REGISTERED.get_or_init(|| {
        let class = wide(CLASS_NAME);
        // SAFETY: the class name is NUL-terminated and outlives the call; the cursor is a shared
        // system resource that must not be freed.
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();

        let window_class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance(),
            lpszClassName: PCWSTR(class.as_ptr()),
            hCursor: cursor,
            ..WNDCLASSW::default()
        };

        // SAFETY: `window_class` is fully initialised and its strings outlive the call.
        unsafe { RegisterClassW(&window_class) != 0 }
    });

    if registered {
        Ok(())
    } else {
        Err(WindowError::Class)
    }
}

/// This module's `HINSTANCE`, which Win32 only uses to own the class registration.
fn instance() -> windows::Win32::Foundation::HINSTANCE {
    // SAFETY: asking for the module handle of the running executable cannot fail for a process
    // that is executing this code.
    unsafe { GetModuleHandleW(None) }
        .map(Into::into)
        .unwrap_or_default()
}

/// A NUL-terminated UTF-16 copy of `text` that outlives the call that needs it.
fn wide(text: &str) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;

    std::ffi::OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Posts one of this module's messages to the window from any thread.
///
/// # Errors
///
/// [`windows::core::Error`] when the window is gone or the queue refuses the message.
pub fn post(hwnd: isize, message: u32) -> windows::core::Result<()> {
    let hwnd = HWND(hwnd as *mut c_void);

    // SAFETY: the caller passes a handle that was live when it was recorded; a destroyed handle
    // fails the post instead of being written through.
    unsafe { PostMessageW(Some(hwnd), message, WPARAM(0), LPARAM(0)) }
}

/// Hands one bridge reply to the window thread, which is the only thread allowed to talk to the
/// WebView2 controller.
///
/// The JSON travels as a boxed `String` in the message's `LPARAM`; the window procedure frees it.
///
/// # Errors
///
/// [`windows::core::Error`] when the window is gone. The box is freed here in that case, so nothing
/// leaks when the window closes mid-call.
pub fn post_reply(hwnd: isize, message: &str) -> windows::core::Result<()> {
    let hwnd = HWND(hwnd as *mut c_void);
    let boxed = Box::into_raw(Box::new(message.to_owned())) as isize;

    // SAFETY: `hwnd` is the window the reply belongs to; the pointer is a live box that the window
    // procedure takes ownership of, or that is reclaimed below if the post fails.
    let posted = unsafe { PostMessageW(Some(hwnd), REPLY, WPARAM(0), LPARAM(boxed)) };

    if posted.is_err() {
        // SAFETY: the post failed, so no window procedure will ever see the box.
        drop(unsafe { Box::from_raw(boxed as *mut String) });
    }

    posted
}

/// Shows the reason this window could not start, in both languages.
///
/// A GUI-subsystem process has no console, so a message box is the only way to say anything when
/// WebView2 is missing.
pub fn report_failure(title: &str, message: &str) {
    use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};

    let title = wide(title);
    let message = wide(message);

    // SAFETY: both strings are NUL-terminated and outlive the call; `None` means "no owner".
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen() -> Screen {
        Screen {
            left: 0.0,
            top: 0.0,
            width: 1920.0,
            height: 1040.0,
        }
    }

    #[test]
    fn a_fresh_window_is_1100_by_720_and_centred() {
        let placement = resolve(&WindowSettings::default(), screen());

        assert_eq!(placement.width, 1100.0);
        assert_eq!(placement.height, 720.0);
        assert_eq!(placement.left, (1920.0 - 1100.0) / 2.0);
        assert_eq!(placement.top, (1040.0 - 720.0) / 2.0);
    }

    #[test]
    fn stored_bounds_are_restored() {
        let settings = WindowSettings {
            left: Some(120.0),
            top: Some(80.0),
            width: 1280.0,
            height: 800.0,
            maximized: false,
        };

        let placement = resolve(&settings, screen());

        assert_eq!(
            placement,
            Placement {
                left: 120.0,
                top: 80.0,
                width: 1280.0,
                height: 800.0,
            }
        );
    }

    #[test]
    fn a_window_off_the_plugged_out_monitor_is_pulled_back() {
        let settings = WindowSettings {
            left: Some(5000.0),
            top: Some(-400.0),
            width: 1100.0,
            height: 720.0,
            maximized: false,
        };

        let placement = resolve(&settings, screen());

        assert_eq!(placement.left, 1920.0 - 1100.0);
        assert_eq!(placement.top, 0.0);
        assert_eq!(placement.width, 1100.0);
        assert_eq!(placement.height, 720.0);
    }

    #[test]
    fn a_shrunk_size_is_lifted_to_the_minimum() {
        let settings = WindowSettings {
            left: Some(10.0),
            top: Some(10.0),
            width: 10.0,
            height: 10.0,
            maximized: false,
        };

        let placement = resolve(&settings, screen());

        assert_eq!(placement.width, MIN_WIDTH);
        assert_eq!(placement.height, MIN_HEIGHT);
    }

    #[test]
    fn a_corrupt_size_falls_back_to_the_default_rather_than_the_minimum() {
        for (width, height) in [(0.0, 0.0), (f64::NAN, f64::INFINITY)] {
            let settings = WindowSettings {
                left: Some(10.0),
                top: Some(10.0),
                width,
                height,
                maximized: false,
            };

            let placement = resolve(&settings, screen());

            assert_eq!(placement.width, DEFAULT_WIDTH, "{width}");
            assert_eq!(placement.height, DEFAULT_HEIGHT, "{height}");
        }
    }

    #[test]
    fn a_stored_size_larger_than_the_screen_is_clamped() {
        let settings = WindowSettings {
            left: Some(0.0),
            top: Some(0.0),
            width: 4000.0,
            height: 3000.0,
            maximized: false,
        };

        let placement = resolve(
            &settings,
            Screen {
                left: 0.0,
                top: 0.0,
                width: 1280.0,
                height: 720.0,
            },
        );

        assert_eq!(placement.width, 1280.0);
        assert_eq!(placement.height, 720.0);
    }

    #[test]
    fn a_position_without_a_size_is_not_a_remembered_placement() {
        let settings = WindowSettings {
            left: Some(300.0),
            top: None,
            width: 640.0,
            height: 480.0,
            maximized: false,
        };

        let placement = resolve(&settings, screen());

        assert_eq!(
            placement.width, 1100.0,
            "a half-placed window opens at the default size"
        );
        assert_eq!(placement.left, (1920.0 - 1100.0) / 2.0);
    }

    #[test]
    fn an_empty_screen_still_yields_a_usable_rectangle() {
        let placement = resolve(
            &WindowSettings::default(),
            Screen {
                left: 0.0,
                top: 0.0,
                width: 0.0,
                height: 0.0,
            },
        );

        assert_eq!(placement.width, 1100.0);
        assert_eq!(placement.height, 720.0);
        assert!(placement.left >= 0.0);
        assert!(placement.top >= 0.0);
    }

    #[test]
    fn a_negative_virtual_origin_is_respected() {
        let placement = resolve(
            &WindowSettings {
                left: Some(-1500.0),
                top: Some(40.0),
                width: 1100.0,
                height: 720.0,
                maximized: false,
            },
            Screen {
                left: -1920.0,
                top: 0.0,
                width: 3840.0,
                height: 1080.0,
            },
        );

        assert_eq!(placement.left, -1500.0);
        assert_eq!(placement.top, 40.0);
    }

    #[test]
    fn dip_bounds_become_pixels_at_150_percent() {
        let physical = Placement {
            left: 100.0,
            top: 50.0,
            width: 1100.0,
            height: 720.0,
        }
        .to_physical(1.5);

        assert_eq!(physical, (150, 75, 1650, 1080));
    }

    #[test]
    fn a_screen_rectangle_becomes_dip_bounds() {
        let rect = RECT {
            left: 410,
            top: 176,
            right: 1510,
            bottom: 896,
        };

        let (bounds, maximized) = placement(rect, false, 1.0).expect("a usable rectangle");

        assert_eq!(
            bounds,
            Placement {
                left: 410.0,
                top: 176.0,
                width: 1100.0,
                height: 720.0,
            }
        );
        assert!(!maximized);

        let (scaled, _) = placement(rect, true, 2.0).expect("a usable rectangle");
        assert_eq!(scaled.width, 550.0);
        assert_eq!(scaled.left, 205.0);
    }

    #[test]
    fn an_empty_rectangle_is_not_remembered() {
        for rect in [
            RECT {
                left: 10,
                top: 10,
                right: 10,
                bottom: 400,
            },
            RECT {
                left: 10,
                top: 10,
                right: 400,
                bottom: 10,
            },
        ] {
            assert!(placement(rect, false, 1.0).is_none(), "{rect:?}");
        }
    }

    #[test]
    fn a_broken_scale_is_treated_as_one() {
        assert_eq!(
            Placement {
                left: 10.0,
                top: 20.0,
                width: 30.0,
                height: 40.0,
            }
            .to_physical(f64::NAN),
            (10, 20, 30, 40)
        );
    }
}
