//! The manager's own model of the desktop: which windows it manages, where they live and which one
//! has focus.
//!
//! The model owns virtual workspaces, not Windows': a workspace is a set of windows this process
//! agrees to show together, implemented by hiding and showing the windows themselves. That keeps
//! the model authoritative — the manager never has to ask the shell which desktop it is on — and it
//! keeps the whole thing testable: no `HWND`, no message loop, just a list.
//!
//! Order matters. Windows are kept in the order they were managed, which is the order the layouts
//! tile them in; moving a window through the tiling swaps its position in that list, so the next
//! layout pass reflects the change without a second source of truth.

use serde::{Deserialize, Serialize};

use crate::rect::Rect;

/// A window handle, carried as a plain integer so the model can travel between threads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowId(pub isize);

impl WindowId {
    /// Whether this is the null handle.
    pub fn is_null(self) -> bool {
        self.0 == 0
    }
}

impl std::fmt::Display for WindowId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "0x{:x}", self.0)
    }
}

/// One managed window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// The handle the manager positions.
    pub id: WindowId,
    /// Title at the moment the window was managed; refreshed on each event.
    pub title: String,
    /// Window class.
    pub class: String,
    /// Executable path, or the process name when it could not be read.
    pub exe: String,
    /// Zero-based workspace the window belongs to.
    pub workspace: usize,
    /// Floating windows keep their rectangle and are excluded from the tiling.
    pub floating: bool,
    /// Whether this window keeps the system decorations instead of the accent border.
    pub no_border: bool,
    /// The rectangle captured on manage, restored on unmanage and on disable.
    pub original: Rect,
    /// Whether the manager applied its decorations to this window.
    pub decorated: bool,
    /// The rule index that decided this window, for the report, or `None` for a default.
    pub rule: Option<usize>,
}

impl Window {
    /// A window with the defaults the manager uses when no rule applies.
    pub fn new(id: WindowId, facts: crate::rules::WindowFacts, original: Rect) -> Self {
        Self {
            id,
            title: facts.title,
            class: facts.class,
            exe: facts.exe,
            workspace: 0,
            floating: false,
            no_border: false,
            original,
            decorated: false,
            rule: None,
        }
    }
}

/// What changed when a window was unmanaged, so the caller can restore the right things.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Removed {
    /// The window that left the model.
    pub window_id: WindowId,
    /// Whether it had decorations applied by the manager.
    pub decorated: bool,
}

/// The set of windows and workspaces.
#[derive(Debug, Clone)]
pub struct WorkspaceModel {
    workspaces: usize,
    active: usize,
    focused: Option<WindowId>,
    windows: Vec<Window>,
}

impl WorkspaceModel {
    /// A model with `workspaces` workspaces, clamped into `1..=9`, starting on the first.
    pub fn new(workspaces: usize) -> Self {
        Self {
            workspaces: workspaces.clamp(1, 9),
            active: 0,
            focused: None,
            windows: Vec::new(),
        }
    }

    /// How many workspaces the model keeps.
    pub fn workspaces(&self) -> usize {
        self.workspaces
    }

    /// The zero-based active workspace.
    pub fn active(&self) -> usize {
        self.active
    }

    /// Every managed window, in tiling order.
    pub fn windows(&self) -> &[Window] {
        &self.windows
    }

    /// How many windows are managed across all workspaces.
    pub fn len(&self) -> usize {
        self.windows.len()
    }

    /// Whether nothing is managed.
    pub fn is_empty(&self) -> bool {
        self.windows.is_empty()
    }

    /// The focused window, if any.
    pub fn focused(&self) -> Option<WindowId> {
        self.focused
    }

    /// Looks a window up by handle.
    pub fn get(&self, id: WindowId) -> Option<&Window> {
        self.windows.iter().find(|window| window.id == id)
    }

    /// Mutable lookup, used by the engine when a title changes.
    pub fn get_mut(&mut self, id: WindowId) -> Option<&mut Window> {
        self.windows.iter_mut().find(|window| window.id == id)
    }

    /// Whether `id` is managed.
    pub fn contains(&self, id: WindowId) -> bool {
        self.get(id).is_some()
    }

    /// Adds a window, assigning its workspace and focusing it when it lands on the active one.
    ///
    /// Returns `false` for a duplicate handle: a second `EVENT_OBJECT_SHOW` for a window that is
    /// already managed must not reset its original rectangle.
    pub fn manage(&mut self, mut window: Window) -> bool {
        if self.contains(window.id) {
            return false;
        }

        window.workspace = window.workspace.min(self.workspaces - 1);
        let on_active = window.workspace == self.active;
        self.windows.push(window);

        if self.focused.is_none() && on_active {
            self.focused = Some(self.windows[self.windows.len() - 1].id);
        }

        true
    }

    /// Removes a window and returns what the caller must restore, if anything.
    pub fn unmanage(&mut self, id: WindowId) -> Option<Removed> {
        let index = self.windows.iter().position(|window| window.id == id)?;
        let window = self.windows.remove(index);
        self.repair_focus(id);

        Some(Removed {
            window_id: id,
            decorated: window.decorated,
        })
    }

    /// Forgets a window without asking for a restore; used when the application hid or destroyed
    /// it, where writing a rectangle back would fight the application.
    pub fn forget(&mut self, id: WindowId) {
        if let Some(index) = self.windows.iter().position(|window| window.id == id) {
            self.windows.remove(index);
            self.repair_focus(id);
        }
    }

    /// Makes sure focus does not stay on a window that is no longer there or no longer visible.
    fn repair_focus(&mut self, gone: WindowId) {
        if self.focused != Some(gone) {
            return;
        }

        self.focused = self
            .windows
            .iter()
            .find(|window| window.workspace == self.active)
            .map(|window| window.id);
    }

    /// Switches to `index`, clamped into the existing workspaces.
    pub fn set_active(&mut self, index: usize) {
        let index = index.min(self.workspaces - 1);
        if index == self.active {
            return;
        }

        self.active = index;
        self.focused = self
            .windows
            .iter()
            .find(|window| window.workspace == index)
            .map(|window| window.id);
    }

    /// Moves the active workspace by `delta`, wrapping around.
    pub fn cycle_active(&mut self, delta: i32) {
        let count = self.workspaces as i32;
        let next = (self.active as i32 + delta).rem_euclid(count);

        self.set_active(next as usize);
    }

    /// The windows on the active workspace, in tiling order; floating windows are included here
    /// because they still have to be shown and hidden with the workspace.
    pub fn active_windows(&self) -> impl Iterator<Item = &Window> {
        self.windows
            .iter()
            .filter(move |window| window.workspace == self.active)
    }

    /// The active workspace's tiling order: the windows the layouts receive, floating excluded.
    pub fn tiles(&self) -> Vec<WindowId> {
        self.active_windows()
            .filter(|window| !window.floating)
            .map(|window| window.id)
            .collect()
    }

    /// Moves `id` to `workspace`, clamping both the window and the focus.
    ///
    /// Returns `false` when the window is not managed. Moving the focused window off the active
    /// workspace hands focus to the next window that stays behind.
    pub fn set_workspace(&mut self, id: WindowId, workspace: usize) -> bool {
        let workspace = workspace.min(self.workspaces - 1);
        let active = self.active;
        let Some(window) = self.get_mut(id) else {
            return false;
        };

        window.workspace = workspace;
        if self.focused == Some(id) && workspace != active {
            self.focused = self
                .windows
                .iter()
                .find(|window| window.workspace == active)
                .map(|window| window.id);
        }

        true
    }

    /// Sets the float state of a window.
    pub fn set_floating(&mut self, id: WindowId, floating: bool) -> bool {
        let Some(window) = self.get_mut(id) else {
            return false;
        };

        window.floating = floating;
        true
    }

    /// Focuses `id` when it is on the active workspace; returns whether focus changed.
    pub fn focus(&mut self, id: WindowId) -> bool {
        let Some(window) = self.get(id) else {
            return false;
        };

        if window.workspace != self.active || self.focused == Some(id) {
            return false;
        }

        self.focused = Some(id);
        true
    }

    /// Focuses the next (`delta = 1`) or previous window on the active workspace, wrapping.
    pub fn focus_next(&mut self, delta: i32) -> Option<WindowId> {
        let ids: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|window| window.workspace == self.active)
            .map(|window| window.id)
            .collect();

        if ids.is_empty() {
            self.focused = None;
            return None;
        }

        let current = self
            .focused
            .and_then(|id| ids.iter().position(|candidate| *candidate == id));
        let next = match current {
            Some(position) => (position as i32 + delta).rem_euclid(ids.len() as i32) as usize,
            None if delta < 0 => ids.len() - 1,
            None => 0,
        };

        self.focused = Some(ids[next]);
        self.focused
    }

    /// Swaps `id` with the neighbouring tile `delta` places away in the active tiling order.
    ///
    /// Returns whether the order changed. A floating window is not part of the tiling order and
    /// therefore cannot be swapped.
    pub fn swap_in_order(&mut self, id: WindowId, delta: i32) -> bool {
        if delta == 0 {
            return false;
        }

        let active = self.active;
        let indices: Vec<usize> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.workspace == active && !window.floating)
            .map(|(index, _)| index)
            .collect();

        let Some(position) = indices
            .iter()
            .position(|index| self.windows[*index].id == id)
        else {
            return false;
        };

        let target = position as i32 + delta;
        if target < 0 || target as usize >= indices.len() {
            return false;
        }

        let left = indices[position];
        let right = indices[target as usize];
        self.windows.swap(left, right);

        true
    }

    /// Moves a window to an absolute position in the active tiling order, clamped.
    pub fn move_in_order(&mut self, id: WindowId, position: usize) -> bool {
        let active = self.active;
        let indices: Vec<usize> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, window)| window.workspace == active && !window.floating)
            .map(|(index, _)| index)
            .collect();

        let Some(from) = indices
            .iter()
            .position(|index| self.windows[*index].id == id)
        else {
            return false;
        };

        let to = position.min(indices.len().saturating_sub(1));
        if from == to {
            return false;
        }

        let window = self.windows.remove(indices[from]);
        // Recompute after the removal: the target index shifts when it was after the source.
        let insert_at = indices
            .get(to)
            .copied()
            .unwrap_or(self.windows.len())
            .min(self.windows.len());
        self.windows.insert(insert_at, window);

        true
    }

    /// Removes every window without restoring anything, returning what was removed.
    pub fn clear(&mut self) -> Vec<Window> {
        self.focused = None;
        std::mem::take(&mut self.windows)
    }

    /// The window on the active workspace that currently has focus, if any.
    pub fn focused_on_active(&self) -> Option<&Window> {
        let focused = self.focused?;
        self.get(focused)
            .filter(|window| window.workspace == self.active)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::WindowFacts;

    fn window(id: isize, workspace: usize) -> Window {
        let mut window = Window::new(
            WindowId(id),
            WindowFacts::new("app.exe", "AppClass", format!("Window {id}")),
            Rect::new(0, 0, 100, 100),
        );
        window.workspace = workspace;
        window
    }

    #[test]
    fn a_fresh_model_has_the_requested_workspaces_and_nothing_else() {
        let model = WorkspaceModel::new(4);

        assert_eq!(model.workspaces(), 4);
        assert_eq!(model.active(), 0);
        assert!(model.is_empty());
        assert_eq!(model.focused(), None);
        assert_eq!(model.tiles(), Vec::new());

        assert_eq!(
            WorkspaceModel::new(0).workspaces(),
            1,
            "at least one workspace"
        );
        assert_eq!(
            WorkspaceModel::new(99).workspaces(),
            9,
            "hotkeys cover nine"
        );
    }

    #[test]
    fn managing_keeps_order_and_ignores_a_duplicate() {
        let mut model = WorkspaceModel::new(4);

        assert!(model.manage(window(1, 0)));
        assert!(model.manage(window(2, 0)));
        assert!(
            !model.manage(window(1, 0)),
            "a duplicate must not reset the original"
        );
        assert_eq!(model.len(), 2);
        assert_eq!(model.tiles(), vec![WindowId(1), WindowId(2)]);
        assert_eq!(model.focused(), Some(WindowId(1)));
    }

    #[test]
    fn a_window_managed_on_another_workspace_does_not_take_focus() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 2));

        assert_eq!(model.focused(), None);
        assert!(model.tiles().is_empty());
        assert_eq!(model.len(), 1, "it is managed, just not visible");
    }

    #[test]
    fn unmanaging_the_focused_window_hands_focus_to_a_neighbour() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));

        assert!(model.unmanage(WindowId(1)).is_some());
        assert_eq!(model.focused(), Some(WindowId(2)));
        assert!(
            model.unmanage(WindowId(1)).is_none(),
            "a second removal is a no-op"
        );
    }

    #[test]
    fn forgetting_a_window_does_not_touch_its_neighbours() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));

        model.forget(WindowId(2));
        assert_eq!(model.tiles(), vec![WindowId(1)]);
        assert_eq!(model.focused(), Some(WindowId(1)));
    }

    #[test]
    fn switching_workspaces_changes_the_visible_set_and_the_focus() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));
        model.manage(window(3, 2));

        model.set_active(2);
        assert_eq!(model.active(), 2);
        assert_eq!(model.tiles(), vec![WindowId(3)]);
        assert_eq!(model.focused(), Some(WindowId(3)));

        model.set_active(1);
        assert_eq!(model.active(), 1);
        assert!(model.tiles().is_empty());
        assert_eq!(model.focused(), None);
    }

    #[test]
    fn the_workspace_cycle_wraps_in_both_directions() {
        let mut model = WorkspaceModel::new(4);

        model.cycle_active(-1);
        assert_eq!(model.active(), 3);
        model.cycle_active(1);
        assert_eq!(model.active(), 0);
        model.cycle_active(5);
        assert_eq!(model.active(), 1, "five steps past the first is the second");
    }

    #[test]
    fn moving_a_window_to_workspace_two_hides_it_from_the_active_tiles() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));

        assert!(model.set_workspace(WindowId(1), 1));
        assert_eq!(model.tiles(), vec![WindowId(2)]);
        assert_eq!(
            model.focused(),
            Some(WindowId(2)),
            "focus moved off the hidden window"
        );
        assert_eq!(
            model.get(WindowId(1)).map(|window| window.workspace),
            Some(1)
        );

        model.set_active(1);
        assert_eq!(
            model.tiles(),
            vec![WindowId(1)],
            "it comes back on workspace two"
        );
    }

    #[test]
    fn a_workspace_outside_the_model_is_clamped() {
        let mut model = WorkspaceModel::new(2);
        model.manage(window(1, 9));

        assert_eq!(
            model.get(WindowId(1)).map(|window| window.workspace),
            Some(1)
        );
        assert!(model.set_workspace(WindowId(1), 99));
        assert_eq!(
            model.get(WindowId(1)).map(|window| window.workspace),
            Some(1)
        );
        assert!(
            !model.set_workspace(WindowId(7), 0),
            "unknown windows are refused"
        );
    }

    #[test]
    fn floating_windows_are_shown_with_the_workspace_but_never_tiled() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));

        assert!(model.set_floating(WindowId(2), true));
        assert_eq!(model.tiles(), vec![WindowId(1)]);
        assert_eq!(
            model.active_windows().count(),
            2,
            "a floating window is still part of the workspace"
        );
        assert_eq!(model.focused(), Some(WindowId(1)));
    }

    #[test]
    fn focus_cycles_through_the_active_workspace_and_wraps() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));
        model.manage(window(3, 1));

        assert_eq!(model.focus_next(1), Some(WindowId(2)));
        assert_eq!(model.focus_next(1), Some(WindowId(1)), "wrap to the first");
        assert_eq!(model.focus_next(-1), Some(WindowId(2)), "and back");

        model.set_active(1);
        assert_eq!(model.focus_next(1), Some(WindowId(3)));
    }

    #[test]
    fn focusing_a_window_on_another_workspace_does_nothing() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));

        assert!(!model.focus(WindowId(1)), "already focused");
        assert!(!model.focus(WindowId(9)), "unknown");
        assert_eq!(model.focused(), Some(WindowId(1)));
    }

    #[test]
    fn moving_a_window_swaps_it_with_its_neighbour() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));
        model.manage(window(3, 0));

        assert!(model.swap_in_order(WindowId(1), 1));
        assert_eq!(model.tiles(), vec![WindowId(2), WindowId(1), WindowId(3)]);
        assert!(!model.swap_in_order(WindowId(1), 5), "the edge refuses");
        assert!(
            !model.swap_in_order(WindowId(9), 1),
            "an unknown window refuses"
        );
    }

    #[test]
    fn moving_to_an_absolute_position_keeps_every_window() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));
        model.manage(window(3, 0));

        assert!(model.move_in_order(WindowId(3), 0));
        assert_eq!(model.tiles(), vec![WindowId(3), WindowId(1), WindowId(2)]);
        assert!(!model.move_in_order(WindowId(3), 0), "already first");
        assert_eq!(model.len(), 3, "a move never drops a window");
    }

    #[test]
    fn clearing_returns_every_window_and_resets_focus() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 1));

        let removed = model.clear();

        assert_eq!(removed.len(), 2);
        assert!(model.is_empty());
        assert_eq!(model.focused(), None);
    }

    #[test]
    fn the_focused_window_must_be_on_the_active_workspace() {
        let mut model = WorkspaceModel::new(4);
        model.manage(window(1, 0));
        model.manage(window(2, 0));

        model.set_workspace(WindowId(1), 3);
        assert!(
            model
                .focused_on_active()
                .is_none_or(|window| window.workspace == 0)
        );
        assert_eq!(model.focused(), Some(WindowId(2)));
    }
}
