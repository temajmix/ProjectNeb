//! Where the wallpaper window goes: the desktop's live-wallpaper slot.
//!
//! The slot is not a named window, and where it lives depends on the Windows build:
//!
//! * **Windows 11 24H2 and later** — `Progman` has no redirection bitmap
//!   (`WS_EX_NOREDIRECTIONBITMAP`) and owns both the icon view and a full-screen `WorkerW` child.
//!   That child is the wallpaper layer, and it only composites its children once it clips them and
//!   the icon view has been refreshed.
//! * **Up to Windows 11 23H2** — `Progman` spawns a top-level `WorkerW` behind the icon view after
//!   the undocumented `0x052C` message, and that sibling is the slot.
//!
//! The search itself is written against [`WindowTree`] so each layout is a unit test rather than a
//! machine to own; only [`LiveTree`] talks to Win32. When neither layout is found, the caller falls
//! back to a bottom-most top-level window and says why in the log.

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, FindWindowExW, FindWindowW, SMTO_ABORTIFHUNG, SMTO_NORMAL, SendMessageTimeoutW,
};

use super::window::{self, RectI};

/// The undocumented message that makes `Progman` spawn the extra `WorkerW` older builds hide the
/// wallpaper in. Explorer drops that window when it restarts, so it is sent before every search.
pub const SPAWN_SLOT_MESSAGE: u32 = 0x052C;

/// How long the spawn message waits for the desktop to answer.
const SPAWN_TIMEOUT_MILLISECONDS: u32 = 1000;

/// `Progman`, the desktop window since Windows 3.1.
pub const PROGMAN_CLASS: &str = "Progman";
/// The layer the wallpaper lives in, on every build that has one.
pub const WORKER_CLASS: &str = "WorkerW";
/// The icon view.
pub const DESKTOP_VIEW_CLASS: &str = "SHELLDLL_DefView";

/// Which layout the slot was found in.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SlotKind {
    /// No slot: the desktop is not Explorer's, or it has not finished starting.
    #[default]
    None,
    /// A `WorkerW` child of `Progman` - Windows 11 24H2 and later.
    ProgmanChild,
    /// A top-level `WorkerW` behind the icon host - Windows 11 23H2 and earlier.
    Sibling,
}

impl SlotKind {
    /// A name for the log and the report.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::ProgmanChild => "progman-child",
            Self::Sibling => "sibling",
        }
    }

    /// The kind as the small integer the IPC snapshot stores.
    pub fn code(self) -> u32 {
        match self {
            Self::None => 0,
            Self::ProgmanChild => 1,
            Self::Sibling => 2,
        }
    }

    /// The kind a stored code means.
    pub fn from_code(code: u32) -> Self {
        match code {
            1 => Self::ProgmanChild,
            2 => Self::Sibling,
            _ => Self::None,
        }
    }
}

/// What a search found: the slot, what it hangs from, and the icon view that must stay above it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Slot {
    /// The window the wallpaper becomes a child of, or zero when nothing was found.
    pub slot: HWND,
    /// The desktop window.
    pub progman: HWND,
    /// The window that owns the icon view - `Progman` on 24H2.
    pub icon_host: HWND,
    /// `SHELLDLL_DefView`, the icons themselves.
    pub desktop_view: HWND,
    /// Which layout this is.
    pub kind: SlotKind,
}

impl Slot {
    /// Whether a slot was found.
    pub fn is_found(&self) -> bool {
        self.kind != SlotKind::None && !window::is_empty_handle(self.slot)
    }

    /// The handles, in the order a log line reads best.
    pub fn describe(&self) -> String {
        format!(
            "kind={} slot=0x{:x} progman=0x{:x} icons=0x{:x} view=0x{:x}",
            self.kind.as_str(),
            handle(self.slot),
            handle(self.progman),
            handle(self.icon_host),
            handle(self.desktop_view)
        )
    }
}

/// Why the window could not be attached.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AttachError {
    /// There is no `Progman` window at all: this is not an Explorer desktop.
    #[error("the desktop has no Progman window")]
    NoProgman,

    /// `Progman` exists but offers no `WorkerW` slot.
    #[error("the desktop offers no WorkerW slot")]
    NoSlot,

    /// The window could not be made a child of the slot, with the Win32 reason.
    #[error("SetParent refused: {0}")]
    SetParent(String),

    /// The window could not be positioned inside the slot.
    #[error("the window could not be placed in the slot")]
    PlacementFailed,
}

impl AttachError {
    /// A phrase for the log line that explains the fallback.
    pub fn reason(&self) -> String {
        match self {
            Self::NoProgman => "no Progman".to_owned(),
            Self::NoSlot => "no WorkerW slot".to_owned(),
            Self::SetParent(why) => format!("SetParent refused ({why})"),
            Self::PlacementFailed => "placement refused".to_owned(),
        }
    }
}

/// The part of the desktop window tree the search needs, and nothing else.
pub trait WindowTree {
    /// The `Progman` window.
    fn progman(&self) -> HWND;

    /// The top-level windows, in Z order from the front to the back.
    fn top_level(&self) -> Vec<HWND>;

    /// Whether `window` hosts the desktop icons, i.e. owns a `SHELLDLL_DefView` child.
    fn owns_desktop_icons(&self, window: HWND) -> bool;

    /// The `SHELLDLL_DefView` child of `window`, or zero.
    fn desktop_view(&self, window: HWND) -> HWND;

    /// The `WorkerW` child of `window`, or zero - the 24H2 wallpaper layer.
    fn worker_w_child(&self, window: HWND) -> HWND;

    /// The `WorkerW` that follows `window` in Z order, or zero.
    fn worker_w_after(&self, window: HWND) -> HWND;
}

/// The search, in the order that matters.
///
/// The `Progman`-owned `WorkerW` comes first because on 24H2 it is the only wallpaper layer there is;
/// then the top-level `WorkerW` that follows the icon host, which is where every older build puts
/// it. A `WorkerW` that is neither belongs to something else - a shell extension, the search window -
/// and a wallpaper placed there would be in the wrong place or nowhere at all.
pub fn find(tree: &impl WindowTree) -> Slot {
    let progman = tree.progman();
    let mut icon_host = HWND::default();
    let mut desktop_view = HWND::default();

    for window in tree.top_level() {
        if !tree.owns_desktop_icons(window) {
            continue;
        }

        icon_host = window;
        desktop_view = tree.desktop_view(window);
        break;
    }

    let progman_worker = tree.worker_w_child(progman);
    if !window::is_empty_handle(progman_worker) {
        return Slot {
            slot: progman_worker,
            progman,
            icon_host,
            desktop_view,
            kind: SlotKind::ProgmanChild,
        };
    }

    if !window::is_empty_handle(icon_host) {
        let sibling = tree.worker_w_after(icon_host);
        if !window::is_empty_handle(sibling) {
            return Slot {
                slot: sibling,
                progman,
                icon_host,
                desktop_view,
                kind: SlotKind::Sibling,
            };
        }
    }

    Slot {
        slot: HWND::default(),
        progman,
        icon_host,
        desktop_view,
        kind: SlotKind::None,
    }
}

/// The real desktop, read fresh on every call.
///
/// Deliberately stateless: a display change or an Explorer restart must be answered by a fresh walk
/// of the tree, never by a cached handle.
#[derive(Debug, Clone, Copy, Default)]
pub struct LiveTree;

impl WindowTree for LiveTree {
    fn progman(&self) -> HWND {
        class_window(PROGMAN_CLASS).unwrap_or_default()
    }

    fn top_level(&self) -> Vec<HWND> {
        let mut windows = Vec::with_capacity(64);
        let pointer = &mut windows as *mut Vec<HWND> as isize;

        // SAFETY: the callback only pushes into `windows`, which outlives the enumeration, and
        // never unwinds.
        unsafe {
            let _ = EnumWindows(Some(collect_window), LPARAM(pointer));
        }

        windows
    }

    fn owns_desktop_icons(&self, window: HWND) -> bool {
        !window::is_empty_handle(self.desktop_view(window))
    }

    fn desktop_view(&self, window: HWND) -> HWND {
        child_of(window, DESKTOP_VIEW_CLASS)
    }

    fn worker_w_child(&self, window: HWND) -> HWND {
        child_of(window, WORKER_CLASS)
    }

    fn worker_w_after(&self, window: HWND) -> HWND {
        if window::is_empty_handle(window) {
            return HWND::default();
        }

        let class = window::wide(WORKER_CLASS);
        // SAFETY: `Some(window)` asks for the next top-level window class match after `window`;
        // the class string outlives the call.
        unsafe {
            FindWindowExW(
                None,
                Some(window),
                windows::core::PCWSTR(class.as_ptr()),
                windows::core::PCWSTR::null(),
            )
        }
        .unwrap_or_default()
    }
}

/// Asks `Progman` to spawn the slot older builds keep out of sight, then finds it.
///
/// On 24H2 the message is answered and creates nothing; the slot is the child `Progman` already
/// owns. Sending it costs one timeout and saves a whole build's worth of special cases.
pub fn probe() -> Result<Slot, AttachError> {
    let tree = LiveTree;
    let progman = tree.progman();
    if window::is_empty_handle(progman) {
        return Err(AttachError::NoProgman);
    }

    // SAFETY: `Progman` is a live window and the message takes no parameters; the result is
    // ignored, and `SMTO_ABORTIFHUNG` keeps a hung Explorer from blocking the caller.
    unsafe {
        let _ = SendMessageTimeoutW(
            progman,
            SPAWN_SLOT_MESSAGE,
            WPARAM(0),
            LPARAM(0),
            SMTO_NORMAL | SMTO_ABORTIFHUNG,
            SPAWN_TIMEOUT_MILLISECONDS,
            None,
        );
    }

    Ok(find(&tree))
}

/// Makes `child` a full-monitor child of the live-wallpaper slot.
///
/// The sequence is the one the C# build proved on this machine, in this order:
/// clip the slot's children, make the window a real child, attach it, place it over the monitor,
/// then refresh the icon view and put the layer behind the icons.
///
/// # Errors
///
/// [`AttachError`] naming which step refused, so the caller can fall back and say why in the log.
pub fn attach(child: HWND, rect: RectI) -> Result<Slot, AttachError> {
    if window::is_empty_handle(child) {
        return Err(AttachError::SetParent(
            "the window handle is null".to_owned(),
        ));
    }

    let slot = probe()?;
    if !slot.is_found() {
        return Err(AttachError::NoSlot);
    }

    // 24H2: a Progman child draws nothing until the WorkerW clips its children, so the composited
    // child area is shown instead of the parent's snapshot of the desktop wallpaper.
    if slot.kind == SlotKind::ProgmanChild {
        window::clip_children(slot.slot);
    }

    window::adopt(child, slot.slot).map_err(AttachError::SetParent)?;

    if !window::place_in_parent(child, slot.slot, rect) {
        return Err(AttachError::PlacementFailed);
    }

    if slot.kind == SlotKind::ProgmanChild {
        // Hiding and showing the icon view makes DWM take a fresh snapshot of the layer below it,
        // which is the composition our window joined.
        window::refresh_desktop_view(slot.desktop_view);

        // The wallpaper layer is a sibling of the icon view and nothing guarantees its order after
        // a refresh; above the view it hides every shortcut on the desktop.
        window::place_behind(slot.slot, slot.desktop_view);
    }

    Ok(slot)
}

/// The fallback: a bottom-most, never-activating top-level window, used when there is no slot.
pub fn attach_as_bottom(window: HWND, rect: RectI) -> bool {
    if window::is_empty_handle(window) {
        return false;
    }

    window::place_at_bottom(window, rect)
}

/// A handle as the log prints it.
pub fn handle(window: HWND) -> usize {
    window.0 as usize
}

/// `FindWindowW` for one class, with nothing cached.
fn class_window(class: &str) -> Option<HWND> {
    let class = window::wide(class);
    // SAFETY: the class string outlives the call and no window name is wanted.
    unsafe {
        FindWindowW(
            windows::core::PCWSTR(class.as_ptr()),
            windows::core::PCWSTR::null(),
        )
    }
    .ok()
}

/// `FindWindowExW` for the first child of `parent` with `class`.
fn child_of(parent: HWND, class: &str) -> HWND {
    if window::is_empty_handle(parent) {
        return HWND::default();
    }

    let class = window::wide(class);
    // SAFETY: both the parent and the class string are valid for the call; no child name is wanted.
    unsafe {
        FindWindowExW(
            Some(parent),
            None,
            windows::core::PCWSTR(class.as_ptr()),
            windows::core::PCWSTR::null(),
        )
    }
    .unwrap_or_default()
}

/// One top-level window, pushed into the vector the enumeration was started with.
unsafe extern "system" fn collect_window(window: HWND, data: LPARAM) -> windows::core::BOOL {
    // SAFETY: `data` is the `&mut Vec<HWND>` the enumeration is filling, and it outlives the
    // callback.
    unsafe {
        let found = &mut *(data.0 as *mut Vec<HWND>);
        found.push(window);
    }

    true.into()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A desktop stand-in: windows are small integers, and each relationship is a list.
    #[derive(Debug, Default)]
    struct FakeTree {
        progman: usize,
        top_level: Vec<usize>,
        views: Vec<(usize, usize)>,
        workers_of: Vec<(usize, usize)>,
        worker_after: Vec<(usize, usize)>,
    }

    fn window(id: usize) -> HWND {
        HWND(id as *mut core::ffi::c_void)
    }

    fn id(handle: HWND) -> usize {
        handle.0 as usize
    }

    fn lookup(pairs: &[(usize, usize)], key: usize) -> HWND {
        pairs
            .iter()
            .find(|(from, _)| *from == key)
            .map_or_else(HWND::default, |(_, to)| window(*to))
    }

    impl WindowTree for FakeTree {
        fn progman(&self) -> HWND {
            window(self.progman)
        }

        fn top_level(&self) -> Vec<HWND> {
            self.top_level.iter().copied().map(window).collect()
        }

        fn owns_desktop_icons(&self, handle: HWND) -> bool {
            !window::is_empty_handle(self.desktop_view(handle))
        }

        fn desktop_view(&self, handle: HWND) -> HWND {
            lookup(&self.views, id(handle))
        }

        fn worker_w_child(&self, handle: HWND) -> HWND {
            lookup(&self.workers_of, id(handle))
        }

        fn worker_w_after(&self, handle: HWND) -> HWND {
            lookup(&self.worker_after, id(handle))
        }
    }

    /// Windows 11 24H2: Progman owns both the view and the wallpaper layer.
    fn layout_24h2() -> FakeTree {
        FakeTree {
            progman: 1,
            top_level: vec![1],
            views: vec![(1, 30)],
            workers_of: vec![(1, 40)],
            worker_after: vec![],
        }
    }

    /// Windows 11 23H2: a top-level WorkerW after the icon host.
    fn layout_23h2() -> FakeTree {
        FakeTree {
            progman: 1,
            top_level: vec![1, 2],
            views: vec![(2, 30)],
            workers_of: vec![],
            worker_after: vec![(2, 41)],
        }
    }

    #[test]
    fn the_progman_child_wins_on_24h2() {
        let slot = find(&layout_24h2());

        assert_eq!(slot.kind, SlotKind::ProgmanChild);
        assert_eq!(id(slot.slot), 40);
        assert_eq!(id(slot.progman), 1);
        assert_eq!(id(slot.icon_host), 1, "Progman hosts the view on 24H2");
        assert_eq!(id(slot.desktop_view), 30);
        assert!(slot.is_found());
    }

    #[test]
    fn the_sibling_worker_is_the_fallback_on_older_builds() {
        let slot = find(&layout_23h2());

        assert_eq!(slot.kind, SlotKind::Sibling);
        assert_eq!(id(slot.slot), 41);
        assert_eq!(id(slot.icon_host), 2);
        assert_eq!(id(slot.desktop_view), 30);
    }

    #[test]
    fn a_desktop_with_both_layouts_attaches_to_the_progman_child() {
        let mut tree = layout_24h2();
        tree.top_level = vec![1, 2];
        tree.views.push((2, 31));
        tree.worker_after.push((2, 41));

        let slot = find(&tree);

        assert_eq!(slot.kind, SlotKind::ProgmanChild);
        assert_eq!(id(slot.slot), 40);
    }

    #[test]
    fn a_desktop_without_a_slot_is_reported_as_such() {
        let tree = FakeTree {
            progman: 1,
            top_level: vec![1],
            views: vec![(1, 30)],
            workers_of: vec![],
            worker_after: vec![],
        };

        let slot = find(&tree);

        assert_eq!(slot.kind, SlotKind::None);
        assert!(!slot.is_found());
        assert_eq!(id(slot.progman), 1, "the report still names what exists");
        assert_eq!(id(slot.desktop_view), 30);
    }

    #[test]
    fn a_missing_progman_is_not_mistaken_for_a_sibling_layout() {
        let tree = FakeTree {
            progman: 0,
            top_level: vec![2],
            views: vec![(2, 30)],
            workers_of: vec![],
            worker_after: vec![(2, 41)],
        };

        let slot = find(&tree);

        // The sibling still exists, but it is only trusted when an icon host was found with it.
        assert_eq!(slot.kind, SlotKind::Sibling);
        assert_eq!(id(slot.slot), 41);
        assert_eq!(id(slot.progman), 0);
    }

    #[test]
    fn a_workerw_that_belongs_to_something_else_is_ignored() {
        // A WorkerW child of a window that is not Progman: the search never looks there.
        let tree = FakeTree {
            progman: 1,
            top_level: vec![1, 2],
            views: vec![(2, 30)],
            workers_of: vec![(2, 40)],
            worker_after: vec![],
        };

        let slot = find(&tree);

        assert_eq!(slot.kind, SlotKind::None);
        assert!(!slot.is_found());
    }

    #[test]
    fn the_first_icon_host_in_z_order_is_the_one_used() {
        let tree = FakeTree {
            progman: 1,
            top_level: vec![1, 2, 3],
            views: vec![(2, 30), (3, 31)],
            workers_of: vec![],
            worker_after: vec![(2, 41), (3, 42)],
        };

        let slot = find(&tree);

        assert_eq!(id(slot.icon_host), 2);
        assert_eq!(id(slot.slot), 41);
    }

    #[test]
    fn the_error_reasons_are_distinct_and_readable() {
        let reasons = [
            AttachError::NoProgman.reason(),
            AttachError::NoSlot.reason(),
            AttachError::SetParent("win32 error 87".to_owned()).reason(),
            AttachError::PlacementFailed.reason(),
        ];

        for (index, reason) in reasons.iter().enumerate() {
            assert!(!reason.is_empty());
            assert_eq!(
                reasons.iter().filter(|other| *other == reason).count(),
                1,
                "reason {index} is not unique: {reason}"
            );
        }

        assert!(
            AttachError::SetParent("win32 error 87".to_owned())
                .reason()
                .contains("87"),
            "the refusal must keep the code that caused it"
        );
    }

    #[test]
    fn the_slot_description_names_every_handle() {
        let slot = find(&layout_24h2());

        assert!(slot.describe().contains("progman-child"));
        assert!(slot.describe().contains("0x28"), "slot 40 in hex");
        assert_eq!(handle(slot.slot), 40);
    }

    #[test]
    fn the_spawn_message_is_the_one_the_c_sharp_build_uses() {
        assert_eq!(SPAWN_SLOT_MESSAGE, 0x052C);
    }
}
