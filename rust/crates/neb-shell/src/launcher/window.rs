//! The launcher window: a panel with a native `EDIT`, owner-drawn results, and the keys.
//!
//! It is a frameless top-level popup, centred on the monitor the pointer is on. Unlike the bar and the
//! dock it *is* activatable: it has to take the keyboard for the query, and it closes as soon as that
//! activation is lost. `WS_EX_TOOLWINDOW` keeps it out of Alt+Tab and off the taskbar.
//!
//! The query is a real Win32 `EDIT` control rather than hand-drawn text, which is what gives the panel
//! IME and Cyrillic input for free. Its keys are intercepted by a subclass — `Esc`, `Enter`, `Up` and
//! `Down` never reach the edit control — and posted to the panel as one small message, so the panel's
//! procedure owns the state and the subclass owns nothing.
//!
//! As in the bar and the dock, the procedure never reaches into the application: a close, a launch and
//! a changed query are recorded in [`Pending`](crate::window::Pending) for the message loop to apply.

use std::ops::{Deref, DerefMut};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::OnceLock;

use neb_core::strings::{Language, Text, get};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreateRoundRectRgn, CreateSolidBrush,
    DEFAULT_CHARSET, DEFAULT_PITCH, DeleteObject, FW_NORMAL, HBRUSH, HFONT, HGDIOBJ,
    OUT_DEFAULT_PRECIS, SetBkColor, SetTextColor, SetWindowRgn,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Threading::{AttachThreadInput, GetCurrentThreadId};
use windows::Win32::UI::Controls::{EM_SETCUEBANNER, EM_SETMARGINS};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, RegisterHotKey, SetFocus, UnregisterHotKey, VK_DOWN,
    VK_ESCAPE, VK_RETURN, VK_SPACE, VK_UP,
};
use windows::Win32::UI::Shell::{DefSubclassProc, RemoveWindowSubclass, SetWindowSubclass};
use windows::Win32::UI::WindowsAndMessaging::{
    CREATESTRUCTW, CS_DBLCLKS, CreateWindowExW, DefWindowProcW, DestroyWindow, EN_CHANGE,
    ES_AUTOHSCROLL, GWLP_USERDATA, GetClientRect, GetForegroundWindow, GetParent,
    GetWindowLongPtrW, GetWindowTextW, GetWindowThreadProcessId, HMENU, IDC_ARROW, LWA_ALPHA,
    LoadCursorW, PostMessageW, RegisterClassW, SW_SHOW, SWP_NOACTIVATE, SendMessageW,
    SetForegroundWindow, SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
    WA_INACTIVE, WINDOW_EX_STYLE, WINDOW_STYLE, WM_ACTIVATE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLOREDIT, WM_DESTROY, WM_DPICHANGED, WM_ERASEBKGND, WM_KEYDOWN, WM_LBUTTONDBLCLK,
    WM_LBUTTONDOWN, WM_NCCREATE, WM_NCDESTROY, WM_PAINT, WM_PRINTCLIENT, WM_SETFONT, WM_SIZE,
    WNDCLASSW, WS_CHILD, WS_EX_LAYERED, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
};
use windows::core::PCWSTR;

use crate::geometry::{Monitor, Rect};
use crate::render::{PaintOutcome, RenderError};
use crate::theme::Theme;
use crate::window::{self as shell_window, WindowError};

use super::render::{
    CORNER_RADIUS, GLASS_ALPHA, LauncherLayout, LauncherRenderer, PANEL_HEIGHT, PANEL_WIDTH,
};
use super::{Entry, fuzzy};

/// Class name of a launcher window.
pub const LAUNCHER_CLASS: &str = "ProjectNeb.NativeLauncher";

/// The query control's child id.
pub const EDIT_ID: usize = 1;

/// The one hotkey id this process registers for the launcher.
pub const HOTKEY_ID: i32 = 0x4E42;

/// How many results the panel keeps; the C# launcher keeps the same fifty.
pub const MAX_RESULTS: usize = 50;

/// `wParam` is a virtual key the query control handed over.
pub const WM_LAUNCHER_KEY: u32 = WM_APP + 8;

/// The query control's text changed after a `WM_SETTEXT`; the panel should re-read it.
pub const WM_LAUNCHER_QUERY: u32 = WM_APP + 9;

/// How tall the query control's font is, in DIPs.
const QUERY_FONT_DIP: f32 = 20.0;

/// What a launcher is created with; it travels through `CreateWindowExW` as the create parameter.
#[derive(Debug, Clone)]
pub struct LauncherSpec {
    /// The monitor the panel is centred on.
    pub monitor: Monitor,
    /// The palette it paints with.
    pub theme: Theme,
    /// The language its strings are in.
    pub language: Language,
    /// The index to search; empty while the worker is still walking the Start Menu.
    pub entries: Arc<Vec<Entry>>,
    /// Whether a build is in flight, which the empty state says out loud.
    pub building: bool,
}

/// Everything one launcher window owns.
#[derive(Debug)]
struct LauncherState {
    hwnd: HWND,
    edit: HWND,
    monitor: Monitor,
    theme: Theme,
    language: Language,
    entries: Arc<Vec<Entry>>,
    /// The entries' names, trimmed and lower-cased once, so a keystroke does not lower them again.
    names: Vec<String>,
    /// Whether each entry launches an executable rather than opening a document.
    applications: Vec<bool>,
    query: String,
    results: Vec<usize>,
    selection: Option<usize>,
    top: usize,
    building: bool,
    renderer: Option<LauncherRenderer>,
    layout: LauncherLayout,
    dpi: u32,
    font: HFONT,
    brush: HBRUSH,
    text_colour: COLORREF,
    text_buffer: Vec<u16>,
    /// Set once the window has actually been activated; a deactivation before that is not a reason to
    /// close, because the first `ShowWindow` can race the foreground handover.
    active: bool,
    /// Set by the first close request, so a second one is ignored.
    closing: bool,
    /// Set while a window procedure holds this state, so a re-entrant message cannot borrow it again.
    busy: bool,
    /// How many frames this panel has drawn, which is the only proof a paint test can offer.
    frames: u64,
}

impl LauncherState {
    fn new(hwnd: HWND, spec: LauncherSpec) -> Self {
        let dpi = spec.monitor.dpi.max(96);
        let layout = LauncherLayout::new((
            spec.monitor.to_pixels(PANEL_WIDTH as i32) as f32,
            spec.monitor.to_pixels(PANEL_HEIGHT as i32) as f32,
        ));

        Self {
            hwnd,
            edit: HWND::default(),
            monitor: spec.monitor,
            theme: spec.theme,
            language: spec.language,
            entries: Arc::new(Vec::new()),
            names: Vec::new(),
            applications: Vec::new(),
            query: String::new(),
            results: Vec::new(),
            selection: None,
            top: 0,
            building: spec.building,
            renderer: None,
            layout,
            dpi,
            font: HFONT::default(),
            brush: HBRUSH::default(),
            text_colour: COLORREF(0),
            text_buffer: vec![0u16; 256],
            active: false,
            closing: false,
            busy: false,
            frames: 0,
        }
        .with_entries(spec.entries)
    }

    /// Replaces the index and rebuilds the names, without recomputing the list.
    fn with_entries(mut self, entries: Arc<Vec<Entry>>) -> Self {
        self.names = entries
            .iter()
            .map(|entry| entry.name.trim().to_lowercase())
            .collect();
        self.applications = entries
            .iter()
            .map(|entry| is_application(&entry.path))
            .collect();
        self.entries = entries;
        self
    }

    /// Positions the child query control, creates the font and shows the panel.
    fn attach(&mut self) {
        self.dpi = dpi_of(self.hwnd);

        let pixel = self.client_size();
        self.renderer = make_renderer(self.hwnd, pixel, self.dpi, &self.theme, self.language);
        self.relayout();
        self.apply_region();
        self.create_font();
        self.create_edit();

        // SAFETY: every call takes this window, which exists, and plain values. The constant alpha is
        // what turns the opaque surface into 96 % glass; the window style is already layered.
        unsafe {
            let _ = SetLayeredWindowAttributes(self.hwnd, COLORREF(0), GLASS_ALPHA, LWA_ALPHA);
        }

        self.recompute();

        // SAFETY: `hwnd` is a live window of this process.
        unsafe {
            let _ = ShowWindow(self.hwnd, SW_SHOW);
        }
        force_foreground(self.hwnd);

        if !self.edit.0.is_null() {
            // SAFETY: `edit` is a live child of this window.
            unsafe {
                let _ = set_focus(self.edit);
            }
        }

        self.paint();

        crate::log_info!(
            "launcher",
            "opened on {}: {} entry(ies), {} result(s), panel {}x{} px, {} rows",
            self.monitor.device,
            self.entries.len(),
            self.results.len(),
            self.layout.size.0,
            self.layout.size.1,
            self.layout.rows
        );
    }

    /// Creates the native query control and gives it the dark palette.
    fn create_edit(&mut self) {
        let (left, top, width, height) = self.layout.query;
        let scale = self.dpi as f32 / 96.0;
        let class = wide("EDIT");
        let placeholder = wide(get(self.language, Text::LauncherSearch));

        // SAFETY: the class name is a static NUL-terminated string, the parent is this live window,
        // and every coordinate is a plain value. The edit is a child, so it is destroyed with the
        // window.
        let created = unsafe {
            CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                PCWSTR(class.as_ptr()),
                PCWSTR::null(),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(ES_AUTOHSCROLL as u32),
                (left * scale).round() as i32,
                (top * scale).round() as i32,
                (width * scale).round() as i32,
                (height * scale).round() as i32,
                Some(self.hwnd),
                Some(HMENU(EDIT_ID as *mut std::ffi::c_void)),
                None,
                None,
            )
        };

        let edit = match created {
            Ok(edit) => edit,
            Err(error) => {
                crate::log_warn!("launcher", "cannot create the query control: {error}");
                return;
            }
        };

        self.edit = edit;

        // SAFETY: `edit` is the control just created, the font and the cue-banner string outlive the
        // calls, and the subclass is this module's own procedure.
        unsafe {
            let _ = SendMessageW(
                edit,
                WM_SETFONT,
                Some(WPARAM(self.font.0 as usize)),
                Some(LPARAM(1)),
            );
            let _ = SendMessageW(
                edit,
                EM_SETCUEBANNER,
                Some(WPARAM(1)),
                Some(LPARAM(placeholder.as_ptr() as isize)),
            );
            let _ = SendMessageW(
                edit,
                EM_SETMARGINS,
                Some(WPARAM((0x0001 | 0x0002) as usize)),
                Some(LPARAM(((12i32) | (12i32 << 16)) as isize)),
            );
            let _ = SetWindowSubclass(edit, Some(edit_proc), EDIT_ID, 0);
        }
    }

    /// Creates the font the query control draws with.
    fn create_font(&mut self) {
        if !self.font.is_invalid() {
            // SAFETY: the font is this state's own object and is replaced exactly once, here.
            unsafe {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
        }

        let height = -(QUERY_FONT_DIP * self.dpi as f32 / 96.0).round() as i32;
        let family = wide("Segoe UI Variable Text");

        // SAFETY: the family name is a NUL-terminated buffer that outlives the call and every other
        // argument is a plain value; the returned font is owned by this state.
        self.font = unsafe {
            CreateFontW(
                height,
                0,
                0,
                0,
                FW_NORMAL.0 as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                OUT_DEFAULT_PRECIS,
                CLIP_DEFAULT_PRECIS,
                CLEARTYPE_QUALITY,
                u32::from(DEFAULT_PITCH.0),
                PCWSTR(family.as_ptr()),
            )
        };

        if self.brush.is_invalid() {
            self.brush = solid_brush(&self.theme.background);
        }

        self.text_colour = colour_of(&self.theme.text);
    }

    /// The window's client size in physical pixels.
    fn client_size(&self) -> (u32, u32) {
        let mut rect = RECT::default();
        // SAFETY: `hwnd` is live and `rect` is a live out-parameter.
        let _ = unsafe { GetClientRect(self.hwnd, &mut rect) };

        (
            u32::try_from(rect.right.max(1)).unwrap_or(1),
            u32::try_from(rect.bottom.max(1)).unwrap_or(1),
        )
    }

    /// Re-measures the panel from its render target and places the query control.
    fn relayout(&mut self) {
        let size = self
            .renderer
            .as_ref()
            .map_or((PANEL_WIDTH, PANEL_HEIGHT), LauncherRenderer::size_dip);
        self.layout = LauncherLayout::new(size);
        self.place_edit();
    }

    /// Moves the query control over the search row.
    fn place_edit(&self) {
        if self.edit.0.is_null() {
            return;
        }

        let (left, top, width, height) = self.layout.query;
        let scale = self.dpi as f32 / 96.0;

        // SAFETY: `edit` is a live child of this window and the rectangle is a plain value.
        unsafe {
            let _ = SetWindowPos(
                self.edit,
                None,
                (left * scale).round() as i32,
                (top * scale).round() as i32,
                (width * scale).round() as i32,
                (height * scale).round() as i32,
                SWP_NOACTIVATE,
            );
        }
    }

    /// Clips the window to its rounded rectangle.
    fn apply_region(&self) {
        let (width, height) = self.client_size();
        let radius = (CORNER_RADIUS * self.dpi as f32 / 96.0).round() as i32;

        // SAFETY: the coordinates describe this window's client area and the returned region is
        // handed to `SetWindowRgn`, which takes ownership when it succeeds.
        let region = unsafe {
            CreateRoundRectRgn(
                0,
                0,
                width as i32 + 1,
                height as i32 + 1,
                radius * 2,
                radius * 2,
            )
        };

        // SAFETY: `region` is a live region and `hwnd` is this window; on failure the region is still
        // ours and is deleted here.
        unsafe {
            if SetWindowRgn(self.hwnd, Some(region), true) == 0 {
                let _ = DeleteObject(HGDIOBJ(region.0));
            }
        }
    }

    /// Re-reads the query from the edit control and filters when it actually changed.
    fn sync_query(&mut self) {
        if self.edit.0.is_null() {
            return;
        }

        // SAFETY: `edit` is a live control and `text_buffer` is a live, correctly sized buffer.
        let length = unsafe { GetWindowTextW(self.edit, &mut self.text_buffer) };
        let length = usize::try_from(length.max(0))
            .unwrap_or(0)
            .min(self.text_buffer.len());
        let query = String::from_utf16_lossy(&self.text_buffer[..length]);

        if query == self.query {
            return;
        }

        self.query = query;
        self.recompute();
    }

    /// Filters the index and puts the best fifty results in order.
    fn recompute(&mut self) {
        let needle = self.query.trim().to_lowercase();
        let ranked = fuzzy::rank(&needle, &self.names, &self.applications, MAX_RESULTS);

        self.results.clear();
        self.results.extend(ranked);
        self.selection = (!self.results.is_empty()).then_some(0);
        self.top = 0;
        self.sync_renderer();
        self.invalidate();
    }

    /// Adopts a new index, from the cache or from a finished worker.
    fn set_entries(&mut self, entries: Arc<Vec<Entry>>, building: bool) {
        self.names = entries
            .iter()
            .map(|entry| entry.name.trim().to_lowercase())
            .collect();
        self.applications = entries
            .iter()
            .map(|entry| is_application(&entry.path))
            .collect();
        self.entries = entries;
        self.building = building;
        self.recompute();

        crate::log_info!(
            "launcher",
            "index adopted: {} entry(ies), {} result(s){}",
            self.entries.len(),
            self.results.len(),
            if building { ", still building" } else { "" }
        );
    }

    /// Hands the current rows to the renderer, outside any frame.
    fn sync_renderer(&mut self) {
        let layout = self.layout;
        let Some(renderer) = self.renderer.as_mut() else {
            return;
        };

        renderer.sync(
            layout,
            &self.entries,
            &self.results,
            self.top,
            self.building,
        );
    }

    /// Moves the selection by `delta`, clamped, and scrolls the list to keep it visible.
    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }

        let current = self.selection.unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, self.results.len() as isize - 1) as usize;

        self.selection = Some(next);
        self.scroll_to(next);
        self.invalidate();
    }

    /// Selects the result in `slot` and keeps it on screen.
    fn select(&mut self, slot: usize) {
        if slot >= self.results.len() {
            return;
        }

        self.selection = Some(slot);
        self.scroll_to(slot);
        self.invalidate();
    }

    /// Scrolls so `slot` is inside the visible window of results.
    fn scroll_to(&mut self, slot: usize) {
        let rows = self.layout.rows.max(1);

        if slot < self.top {
            self.top = slot;
        } else if slot >= self.top + rows {
            self.top = slot + 1 - rows;
        }

        self.sync_renderer();
    }

    /// The action a key asks for.
    fn key(&mut self, key: u32) {
        if key == u32::from(VK_ESCAPE.0) {
            self.request_close();
        } else if key == u32::from(VK_RETURN.0) {
            self.launch_selected();
        } else if key == u32::from(VK_UP.0) {
            self.move_selection(-1);
        } else if key == u32::from(VK_DOWN.0) {
            self.move_selection(1);
        }
    }

    /// Records that the selected result should be launched and that the panel should close.
    fn launch_selected(&mut self) {
        let Some(slot) = self.selection else {
            crate::log_warn!("launcher", "Enter with no result selected");
            return;
        };

        if slot >= self.results.len() {
            return;
        }

        // Recorded, not launched here: `ShellExecuteW` may block and the loop applies it with
        // nothing borrowed.
        shell_window::set_pending(|pending| pending.set_launcher_launch(slot));
        self.request_close();
    }

    /// Asks the application to destroy the panel, once.
    fn request_close(&mut self) {
        if self.closing {
            return;
        }

        self.closing = true;
        shell_window::set_pending(|pending| pending.set_launcher_close());
    }

    /// `WM_ACTIVATE`: the panel closes when the user moves to another window.
    fn activated(&mut self, wparam: usize) {
        let active = (wparam & 0xFFFF) != WA_INACTIVE as usize;

        if active {
            self.active = true;
        } else if self.active {
            self.request_close();
        }
    }

    /// A pointer press: select the row under it and hand the keyboard back to the query.
    fn clicked(&mut self, lparam: LPARAM, launch: bool) {
        let scale = self.dpi as f32 / 96.0;
        let packed = lparam.0 as u32;
        let y = f32::from(((packed >> 16) & 0xFFFF) as u16 as i16);

        if let Some(row) = self.layout.row_at(y / scale) {
            self.select(self.top + row);

            if launch {
                self.launch_selected();
            }
        }

        if !self.edit.0.is_null() {
            // SAFETY: `edit` is a live control; returning the keyboard there is what keeps typing
            // possible after a click on a row.
            unsafe {
                let _ = set_focus(self.edit);
            }
        }
    }

    /// Draws one frame through the renderer, and counts it.
    fn paint(&mut self) {
        let outcome = match self.renderer.as_mut() {
            Some(renderer) => renderer.paint(self.selection, self.top),
            None => PaintOutcome::Failed(RenderError::Unavailable(
                "the launcher has no renderer".to_owned(),
            )),
        };

        match outcome {
            PaintOutcome::Painted => self.frames += 1,
            PaintOutcome::Recreate => {
                crate::log_info!("launcher", "the render target was lost, rebuilding");
                self.recreate_renderer();
                if let Some(renderer) = self.renderer.as_mut()
                    && let PaintOutcome::Painted = renderer.paint(self.selection, self.top)
                {
                    self.frames += 1;
                }
            }
            PaintOutcome::Failed(error) => crate::log_warn!("launcher", "paint: {error}"),
        }
    }

    /// Rebuilds the target after `D2DERR_RECREATE_TARGET`.
    fn recreate_renderer(&mut self) {
        let pixel = self.client_size();
        self.renderer = make_renderer(self.hwnd, pixel, self.dpi, &self.theme, self.language);
        self.relayout();
        self.sync_renderer();
    }

    /// `WM_SIZE`: the target and the query control follow the window.
    fn resized(&mut self) {
        let pixel = self.client_size();

        if let Some(renderer) = self.renderer.as_mut()
            && let Err(error) = renderer.resize(pixel)
        {
            crate::log_warn!("launcher", "resize: {error}");
        }

        self.relayout();
        self.apply_region();
        self.sync_renderer();
        self.invalidate();
    }

    /// `WM_DPICHANGED`: the panel moved to a display of another scale.
    fn dpi_changed(&mut self, dpi: u32, suggested: Option<Rect>) {
        self.dpi = dpi.max(96);

        if let Some(rect) = suggested {
            // SAFETY: `hwnd` is live and the rectangle is a plain value.
            unsafe {
                let _ = SetWindowPos(
                    self.hwnd,
                    None,
                    rect.left,
                    rect.top,
                    rect.width(),
                    rect.height(),
                    SWP_NOACTIVATE,
                );
            }
        }

        self.recreate_renderer();
        self.create_font();
        self.apply_region();
        self.resized();
    }

    /// Asks for a repaint, which Windows may coalesce with another one.
    fn invalidate(&self) {
        // SAFETY: `hwnd` is a live window.
        unsafe {
            let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(self.hwnd), None, false);
        }
    }

    /// Paints the control's background and text in the panel's palette.
    fn control_colour(&self, hdc: windows::Win32::Graphics::Gdi::HDC) -> LRESULT {
        // SAFETY: `hdc` is the device context Windows handed the `WM_CTLCOLOREDIT` message for.
        unsafe {
            let _ = SetBkColor(hdc, colour_of(&self.theme.background));
            let _ = SetTextColor(hdc, self.text_colour);
        }

        LRESULT(self.brush.0 as isize)
    }
}

impl Drop for LauncherState {
    fn drop(&mut self) {
        // SAFETY: both objects were created by this state and are deleted exactly once, here.
        unsafe {
            if !self.font.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.font.0));
            }
            if !self.brush.is_invalid() {
                let _ = DeleteObject(HGDIOBJ(self.brush.0));
            }
        }
    }
}

/// Exclusive access to one launcher's state for the length of one window message.
#[derive(Debug)]
struct LauncherGuard {
    state: *mut LauncherState,
}

impl LauncherGuard {
    /// Takes the state of `hwnd`, or `None` when it is missing or already borrowed.
    ///
    /// # Safety
    ///
    /// `hwnd` must be a window this module created on this thread.
    unsafe fn enter(hwnd: HWND) -> Option<Self> {
        // SAFETY: the caller guarantees `hwnd` is one of our windows, whose user data is either null
        // or the `Box<LauncherState>` that `WM_NCCREATE` installed.
        let pointer = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };

        if pointer == 0 {
            return None;
        }

        let state = pointer as *mut LauncherState;

        // SAFETY: the pointer is that box, and it is dropped only in `WM_NCDESTROY`, which cannot run
        // while a guard is alive. `busy` is the proof that no other guard holds it.
        if unsafe { (*state).busy } {
            return None;
        }

        // SAFETY: as above; this frame now owns the state until the guard drops.
        unsafe {
            (*state).busy = true;
        }

        Some(Self { state })
    }
}

impl Deref for LauncherGuard {
    type Target = LauncherState;

    fn deref(&self) -> &Self::Target {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe { &*self.state }
    }
}

impl DerefMut for LauncherGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe { &mut *self.state }
    }
}

impl Drop for LauncherGuard {
    fn drop(&mut self) {
        // SAFETY: the guard owns the state; see `enter`.
        unsafe {
            (*self.state).busy = false;
        }
    }
}

/// The query control's subclass: it hands the launcher's keys to the panel and reports text changes.
unsafe extern "system" fn edit_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
    _id: usize,
    _data: usize,
) -> LRESULT {
    // SAFETY: the caller is Windows, delivering one message for the edit control this module
    // subclassed.
    unsafe {
        if message == WM_KEYDOWN {
            let key = wparam.0 as u32;

            if [VK_ESCAPE, VK_RETURN, VK_UP, VK_DOWN]
                .iter()
                .any(|wanted| key == u32::from(wanted.0))
            {
                if let Ok(parent) = GetParent(hwnd) {
                    let _ = PostMessageW(
                        Some(parent),
                        WM_LAUNCHER_KEY,
                        WPARAM(key as usize),
                        LPARAM(0),
                    );
                }

                return LRESULT(0);
            }
        }

        let result = DefSubclassProc(hwnd, message, wparam, lparam);

        if message == windows::Win32::UI::WindowsAndMessaging::WM_SETTEXT
            && let Ok(parent) = GetParent(hwnd)
        {
            // A programmatic `WM_SETTEXT` (the acceptance harness) does not always notify the
            // parent; posting our own query message makes the filter follow the control either way.
            let _ = PostMessageW(Some(parent), WM_LAUNCHER_QUERY, WPARAM(0), LPARAM(0));
        }

        result
    }
}

/// The launcher window procedure.
unsafe extern "system" fn launcher_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    // SAFETY: the caller is Windows, delivering one message for one of our windows.
    unsafe {
        match message {
            WM_NCCREATE => {
                let create = lparam.0 as *const CREATESTRUCTW;
                if create.is_null() {
                    return LRESULT(0);
                }

                // `create_launcher` boxed the spec and hands it over here; nothing else owns it.
                let spec = *Box::from_raw((*create).lpCreateParams.cast::<LauncherSpec>());
                let state = Box::new(LauncherState::new(hwnd, spec));
                SetWindowLongPtrW(hwnd, GWLP_USERDATA, Box::into_raw(state) as isize);
                return LRESULT(1);
            }
            WM_NCDESTROY => {
                let pointer = SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
                if pointer != 0 {
                    drop(Box::from_raw(pointer as *mut LauncherState));
                }
                return LRESULT(0);
            }
            _ => {}
        }

        let Some(mut state) = LauncherGuard::enter(hwnd) else {
            return DefWindowProcW(hwnd, message, wparam, lparam);
        };

        match message {
            WM_PAINT => {
                let mut paint = windows::Win32::Graphics::Gdi::PAINTSTRUCT::default();
                let _ = windows::Win32::Graphics::Gdi::BeginPaint(hwnd, &mut paint);
                state.paint();
                let _ = windows::Win32::Graphics::Gdi::EndPaint(hwnd, &paint);
                LRESULT(0)
            }
            WM_PRINTCLIENT => {
                let hdc = windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _);
                let _ = state
                    .renderer
                    .as_ref()
                    .is_some_and(|renderer| renderer.blit_to(hdc));
                LRESULT(0)
            }
            WM_ERASEBKGND => LRESULT(1),
            WM_SIZE => {
                state.resized();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                let dpi = (wparam.0 & 0xFFFF) as u32;
                let suggested = if lparam.0 == 0 {
                    None
                } else {
                    // SAFETY: `WM_DPICHANGED` carries a pointer to the rectangle Windows suggests for
                    // the window at the new scale.
                    Some(Rect::from(*(lparam.0 as *const RECT)))
                };

                state.dpi_changed(dpi, suggested);
                LRESULT(0)
            }
            WM_ACTIVATE => {
                state.activated(wparam.0);
                LRESULT(0)
            }
            WM_COMMAND => {
                let id = wparam.0 & 0xFFFF;
                let code = (wparam.0 >> 16) & 0xFFFF;

                if id == EDIT_ID && code == EN_CHANGE as usize {
                    state.sync_query();
                }

                LRESULT(0)
            }
            WM_LAUNCHER_QUERY => {
                state.sync_query();
                LRESULT(0)
            }
            WM_KEYDOWN => {
                state.key(wparam.0 as u32);
                LRESULT(0)
            }
            WM_LAUNCHER_KEY => {
                state.key(wparam.0 as u32);
                LRESULT(0)
            }
            WM_CTLCOLOREDIT => {
                state.control_colour(windows::Win32::Graphics::Gdi::HDC(wparam.0 as *mut _))
            }
            WM_LBUTTONDOWN => {
                state.clicked(lparam, false);
                LRESULT(0)
            }
            WM_LBUTTONDBLCLK => {
                state.clicked(lparam, true);
                LRESULT(0)
            }
            WM_CLOSE => {
                state.request_close();
                LRESULT(0)
            }
            WM_DESTROY => {
                if !state.edit.0.is_null() {
                    // SAFETY: the control is still alive during `WM_DESTROY`; removing the subclass
                    // here means it cannot be called after the state is gone.
                    let _ = RemoveWindowSubclass(state.edit, Some(edit_proc), EDIT_ID);
                }

                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, message, wparam, lparam),
        }
    }
}

static LAUNCHER_CLASS_REGISTERED: OnceLock<bool> = OnceLock::new();

/// This process's `HINSTANCE`.
fn instance() -> windows::Win32::Foundation::HINSTANCE {
    // SAFETY: a null module name asks for this process's own module.
    match unsafe { GetModuleHandleW(None) } {
        Ok(module) => windows::Win32::Foundation::HINSTANCE(module.0),
        Err(_) => windows::Win32::Foundation::HINSTANCE::default(),
    }
}

/// Registers the launcher class once per process.
fn register_class() -> Result<(), WindowError> {
    let registered = *LAUNCHER_CLASS_REGISTERED.get_or_init(|| {
        let name = wide(LAUNCHER_CLASS);

        // SAFETY: a null module and the arrow cursor are the system's own; the call only returns a
        // shared cursor handle.
        let cursor = unsafe { LoadCursorW(None, IDC_ARROW) }.unwrap_or_default();

        let window_class = WNDCLASSW {
            style: CS_DBLCLKS,
            lpfnWndProc: Some(launcher_proc),
            hInstance: instance(),
            hCursor: cursor,
            lpszClassName: PCWSTR(name.as_ptr()),
            ..WNDCLASSW::default()
        };

        // SAFETY: `window_class` is fully initialised and the class name it points at outlives the
        // call; a null background brush keeps the panel from flashing before the first frame.
        unsafe { RegisterClassW(&window_class) != 0 }
    });

    if registered {
        Ok(())
    } else {
        Err(WindowError::Class {
            class: LAUNCHER_CLASS,
        })
    }
}

/// The panel's rectangle in physical pixels, centred on `monitor`.
pub fn panel_rect(monitor: &Monitor) -> Rect {
    let width = monitor
        .to_pixels(PANEL_WIDTH as i32)
        .min(monitor.rect.width())
        .max(1);
    let height = monitor
        .to_pixels(PANEL_HEIGHT as i32)
        .min(monitor.rect.height())
        .max(1);

    Rect::new(
        monitor.rect.left + (monitor.rect.width() - width) / 2,
        monitor.rect.top + (monitor.rect.height() - height) / 2,
        monitor.rect.left + (monitor.rect.width() + width) / 2,
        monitor.rect.top + (monitor.rect.height() + height) / 2,
    )
}

/// Creates the launcher for `spec`, shows it and gives it the keyboard.
///
/// # Errors
///
/// [`WindowError`] when Windows refuses the class or the window, or the state never arrives.
pub fn create_launcher(spec: LauncherSpec) -> Result<HWND, WindowError> {
    register_class()?;

    let rect = panel_rect(&spec.monitor);
    let class = wide(LAUNCHER_CLASS);
    let title = wide("ProjectNeb launcher");
    let parameter = Box::into_raw(Box::new(spec));

    // SAFETY: both strings are NUL-terminated and outlive the call; `parameter` is a heap
    // `LauncherSpec` that `WM_NCCREATE` takes ownership of, or that is reclaimed below when the call
    // fails.
    let created = unsafe {
        CreateWindowExW(
            WS_EX_TOOLWINDOW | WS_EX_TOPMOST | WS_EX_LAYERED,
            PCWSTR(class.as_ptr()),
            PCWSTR(title.as_ptr()),
            WS_POPUP,
            rect.left,
            rect.top,
            rect.width(),
            rect.height(),
            None,
            None,
            Some(instance()),
            Some(parameter.cast()),
        )
    };

    let hwnd = match created {
        Ok(hwnd) => hwnd,
        Err(source) => {
            // `WM_NCCREATE` never ran, so the spec is still ours to free.
            drop(unsafe { Box::from_raw(parameter) });
            return Err(WindowError::Create {
                class: LAUNCHER_CLASS,
                source,
            });
        }
    };

    // SAFETY: `hwnd` was created a line above, on this thread.
    let mut state =
        unsafe { LauncherGuard::enter(hwnd) }.ok_or(WindowError::NoState(LAUNCHER_CLASS))?;
    state.attach();

    Ok(hwnd)
}

/// Destroys one launcher.
pub fn destroy_launcher(hwnd: HWND) {
    // SAFETY: `hwnd` is a live launcher window of this process.
    unsafe {
        let _ = DestroyWindow(hwnd);
    }
}

/// Replaces the index of an open panel.
pub fn set_entries(hwnd: HWND, entries: Arc<Vec<Entry>>, building: bool) {
    // SAFETY: `hwnd` is a live launcher window of this process, created on this thread.
    if let Some(mut state) = unsafe { LauncherGuard::enter(hwnd) } {
        state.set_entries(entries, building);
    }
}

/// The path behind result `slot`, for the application that is about to launch it.
pub fn item(hwnd: HWND, slot: usize) -> Option<PathBuf> {
    // SAFETY: `hwnd` is a live launcher window of this process, created on this thread.
    let state = unsafe { LauncherGuard::enter(hwnd) }?;
    let index = *state.results.get(slot)?;

    state.entries.get(index).map(|entry| entry.path.clone())
}

/// The name behind result `slot`, for the log line a launch writes.
pub fn name(hwnd: HWND, slot: usize) -> Option<String> {
    // SAFETY: `hwnd` is a live launcher window of this process, created on this thread.
    let state = unsafe { LauncherGuard::enter(hwnd) }?;
    let index = *state.results.get(slot)?;

    state.entries.get(index).map(|entry| entry.name.clone())
}

/// How many results the panel is showing.
pub fn result_count(hwnd: HWND) -> usize {
    // SAFETY: `hwnd` is a live launcher window of this process, created on this thread.
    unsafe { LauncherGuard::enter(hwnd) }.map_or(0, |state| state.results.len())
}

/// The query the panel currently filters with.
pub fn query_of(hwnd: HWND) -> Option<String> {
    // SAFETY: `hwnd` is a live launcher window of this process, created on this thread.
    unsafe { LauncherGuard::enter(hwnd) }.map(|state| state.query.clone())
}

/// How many frames this panel has actually drawn; zero means the paint path never ran.
pub fn painted_frames(hwnd: HWND) -> u64 {
    // SAFETY: `hwnd` is a live launcher window of this process, created on this thread.
    unsafe { LauncherGuard::enter(hwnd) }.map_or(0, |state| state.frames)
}

/// Paints `hwnd` synchronously, without waiting for the queue.
///
/// `WM_PAINT` is the only thing that draws, so this is the whole paint path; the allocation harness
/// in `tests/paint_no_alloc.rs` is what needs the synchronous entry point.
pub fn repaint_now(hwnd: HWND) {
    // SAFETY: `hwnd` is a live launcher window of this process, created on this thread.
    if let Some(mut state) = unsafe { LauncherGuard::enter(hwnd) } {
        state.paint();
    }
}

/// The client size of `hwnd` in pixels, which a harness compares with the panel it asked for.
pub fn client_size(hwnd: HWND) -> Option<(u32, u32)> {
    let mut rect = RECT::default();
    // SAFETY: `hwnd` is a live window and `rect` is a live out-parameter.
    unsafe { GetClientRect(hwnd, &mut rect) }.ok()?;

    Some((
        u32::try_from(rect.right).unwrap_or(1),
        u32::try_from(rect.bottom).unwrap_or(1),
    ))
}

/// Registers the launcher's hotkey, falling back from `Alt+Space` to `Ctrl+Alt+Space`.
///
/// The gesture is registered once, on the controller window. A refusal is not a failure: the fallback
/// is tried, and the log says which gesture won, which is the "toast" this build has.
pub fn register_hotkey(control: HWND) -> Option<&'static str> {
    let candidates = [
        (MOD_ALT | MOD_NOREPEAT, "Alt+Space"),
        (MOD_CONTROL | MOD_ALT | MOD_NOREPEAT, "Ctrl+Alt+Space"),
    ];

    for (modifiers, gesture) in candidates {
        // SAFETY: `control` is this process's own message window, and the modifiers and key are
        // plain values. Windows fulfils the request for that window.
        if unsafe { RegisterHotKey(Some(control), HOTKEY_ID, modifiers, u32::from(VK_SPACE.0)) }
            .is_ok()
        {
            crate::log_info!("launcher", "hotkey armed on {gesture}");

            if gesture != "Alt+Space" {
                let message =
                    get(Language::Ru, Text::LauncherHotkeyFallback).replace("{gesture}", gesture);
                crate::log_warn!("launcher", "{message}");

                let english =
                    get(Language::En, Text::LauncherHotkeyFallback).replace("{gesture}", gesture);
                crate::log_warn!("launcher", "{english}");
            }

            return Some(gesture);
        }
    }

    crate::log_warn!(
        "launcher",
        "every launcher gesture is taken by another program"
    );
    None
}

/// Releases the hotkey; harmless when none was registered.
pub fn unregister_hotkey(control: HWND) {
    // SAFETY: `control` is this process's own message window.
    unsafe {
        let _ = UnregisterHotKey(Some(control), HOTKEY_ID);
    }
}

/// Puts `hwnd` in the foreground even when this process did not just receive input.
///
/// `SetForegroundWindow` alone is refused for a process that is not in the foreground — which is
/// exactly the case when the harness posts `WM_HOTKEY` — so the foreground thread's input queue is
/// attached for the length of the call, the documented workaround.
pub fn force_foreground(hwnd: HWND) {
    // SAFETY: both handles are window handles from Windows, and the thread ids are plain values. The
    // attachment is undone on every path.
    unsafe {
        let foreground = GetForegroundWindow();
        if foreground == hwnd {
            return;
        }

        let foreground_thread = GetWindowThreadProcessId(foreground, None);
        let this_thread = GetCurrentThreadId();
        let attached = foreground_thread != 0
            && foreground_thread != this_thread
            && AttachThreadInput(this_thread, foreground_thread, true).as_bool();

        let _ = SetForegroundWindow(hwnd);

        if attached {
            let _ = AttachThreadInput(this_thread, foreground_thread, false);
        }
    }
}

/// Puts `previous` back in the foreground when the panel closes.
pub fn restore_foreground(previous: isize) {
    if previous == 0 {
        return;
    }

    force_foreground(HWND(previous as *mut std::ffi::c_void));
}

/// The handle of the window that owns the foreground, as a plain integer.
pub fn foreground() -> isize {
    // SAFETY: the call takes no arguments and returns a handle this process does not own.
    unsafe { GetForegroundWindow().0 as isize }
}

/// `SetFocus` for a live window; a free function so the `unsafe` block stays in one place.
unsafe fn set_focus(hwnd: HWND) -> windows::core::Result<HWND> {
    // SAFETY: the caller guarantees `hwnd` is a live window of this thread.
    unsafe { SetFocus(Some(hwnd)) }
}

/// Whether an entry's target is an application rather than a document.
fn is_application(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            ["exe", "com", "bat", "cmd", "msc"]
                .iter()
                .any(|wanted| extension.eq_ignore_ascii_case(wanted))
        })
}

/// The system DPI of `hwnd`, or the base scale when Windows cannot say.
fn dpi_of(hwnd: HWND) -> u32 {
    // SAFETY: `hwnd` is a live window of this process.
    unsafe { GetDpiForWindow(hwnd) }.max(96)
}

/// Builds a renderer, logging and giving up when Direct2D refuses.
fn make_renderer(
    hwnd: HWND,
    pixel: (u32, u32),
    dpi: u32,
    theme: &Theme,
    language: Language,
) -> Option<LauncherRenderer> {
    match LauncherRenderer::new(hwnd, pixel, dpi, theme, language) {
        Ok(renderer) => Some(renderer),
        Err(error) => {
            crate::log_warn!("launcher", "cannot create the renderer: {error}");
            None
        }
    }
}

/// A solid brush for the query control's background.
fn solid_brush(colour: &windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> HBRUSH {
    // SAFETY: the call takes a colour value and returns a brush this state owns.
    unsafe { CreateSolidBrush(colour_of(colour)) }
}

/// A Direct2D colour as a `COLORREF`, which is `0x00BBGGRR`.
fn colour_of(colour: &windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F) -> COLORREF {
    let channel = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u32;

    COLORREF(channel(colour.r) | (channel(colour.g) << 8) | (channel(colour.b) << 16))
}

/// Asks Windows to page out the process's unused pages.
///
/// This is not a measurement trick: the launcher has just been destroyed and its target and caches
/// are freed, but a process that stays resident would otherwise keep those freed pages in its working
/// set until the system runs short of memory. Handing them back is what keeps a resident shell inside
/// its budget after a transient window has been used.
pub fn trim_working_set() {
    // SAFETY: `GetCurrentProcess` returns a pseudo-handle for this process, which needs no closing,
    // and the documented `(usize::MAX, usize::MAX)` pair asks Windows to trim the working set.
    unsafe {
        let _ = windows::Win32::System::Threading::SetProcessWorkingSetSize(
            windows::Win32::System::Threading::GetCurrentProcess(),
            usize::MAX,
            usize::MAX,
        );
    }
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(dpi: u32) -> Monitor {
        Monitor {
            handle: 1,
            device: r"\\.\DISPLAY1".to_owned(),
            rect: Rect::new(0, 0, 1920, 1080),
            work: Rect::new(0, 0, 1920, 1032),
            dpi,
        }
    }

    #[test]
    fn the_panel_is_centred_on_its_monitor_at_ninety_six_dpi() {
        let rect = panel_rect(&monitor(96));

        assert_eq!(rect, Rect::new(600, 300, 1320, 780));
        assert_eq!((rect.width(), rect.height()), (720, 480));
    }

    #[test]
    fn the_panel_scales_with_the_monitor_and_never_outgrows_it() {
        let scaled = panel_rect(&monitor(144));
        assert_eq!((scaled.width(), scaled.height()), (1080, 720));

        let small = Monitor {
            rect: Rect::new(0, 0, 640, 400),
            work: Rect::new(0, 0, 640, 360),
            dpi: 96,
            ..monitor(96)
        };
        let clamped = panel_rect(&small);

        assert_eq!((clamped.width(), clamped.height()), (640, 400));
        assert_eq!(clamped, Rect::new(0, 0, 640, 400));
    }

    #[test]
    fn the_hotkey_id_and_the_messages_are_stable() {
        assert_eq!(HOTKEY_ID, 0x4E42);
        assert_eq!(WM_LAUNCHER_KEY, WM_APP + 8);
        assert_eq!(WM_LAUNCHER_QUERY, WM_APP + 9);
        assert_eq!(super::super::WM_LAUNCHER_OPEN, WM_APP + 6);
        assert_eq!(super::super::WM_LAUNCHER_INDEX, WM_APP + 7);
    }

    #[test]
    fn a_selection_stays_inside_the_result_list() {
        // The arithmetic `move_selection` uses, kept pure so it is tested without a window.
        let clamp = |current: isize, len: usize, delta: isize| -> usize {
            (current + delta).clamp(0, len as isize - 1) as usize
        };

        assert_eq!(clamp(0, 5, -1), 0);
        assert_eq!(clamp(0, 5, 1), 1);
        assert_eq!(clamp(4, 5, 1), 4);
        assert_eq!(clamp(2, 5, 5), 4);
    }

    #[test]
    fn a_colour_is_converted_to_the_colourref_bit_order() {
        let black = colour_of(&windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
            r: 0.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        });
        let red = colour_of(&windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F {
            r: 1.0,
            g: 0.0,
            b: 0.0,
            a: 1.0,
        });

        assert_eq!(black, COLORREF(0));
        assert_eq!(red, COLORREF(0x0000_00FF));
    }

    #[test]
    fn the_row_band_constants_line_up_with_the_renderer() {
        use crate::launcher::render::{ACCENT_BAR, FOOTER_HEIGHT, ROW_HEIGHT};

        assert_eq!(ROW_HEIGHT, 48.0);
        assert_eq!(ACCENT_BAR, 3.0);
        assert_eq!(FOOTER_HEIGHT, 34.0);
        assert_eq!(GLASS_ALPHA, 245);
    }
}
