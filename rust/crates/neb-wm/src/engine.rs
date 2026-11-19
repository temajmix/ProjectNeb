//! The manager's core: model plus layout plus the calls that move windows.
//!
//! Win32 is behind [`WindowOps`]. The engine can therefore be tested against a fake desktop that
//! records rectangles, refuses a particular handle or reports a window as fullscreen — including
//! the two behaviours that are hard to reproduce with real windows: a refusal half-way through an
//! animation, and a restore that itself fails because the application is gone.

use std::time::Duration;

use neb_core::config::WindowManagerSettings;
use neb_core::theme::Rgb;
use serde_json::Value;

use crate::animation::{self, AnimationPlan, Move};
use crate::decorations::{self, DecorationPlan, DecorationReadback};
use crate::layout::{self, LayoutKind, LayoutParams};
use crate::model::{Window, WindowId, WorkspaceModel};
use crate::rect::{Direction, Rect};

/// Everything the engine needs from the operating system.
///
/// The defaults make the fake in the tests tiny: a trait object that only implements the four
/// methods the tiling needs still compiles and still exercises every branch.
pub trait WindowOps {
    /// The window's current screen rectangle.
    fn rect(&self, id: WindowId) -> Result<Rect, String>;

    /// Moves and resizes the window.
    fn set_rect(&self, id: WindowId, rect: Rect) -> Result<(), String>;

    /// Shows or hides the window.
    fn show(&self, id: WindowId, visible: bool) -> Result<(), String>;

    /// Writes the decoration attributes.
    fn set_decorations(&self, id: WindowId, plan: &DecorationPlan) -> Result<(), String>;

    /// Reads the decoration attributes back, when Windows allows it.
    fn read_decorations(&self, id: WindowId) -> DecorationReadback {
        let _ = id;
        DecorationReadback::default()
    }

    /// Brings the window to the foreground.
    fn focus(&self, id: WindowId) -> Result<(), String> {
        let _ = id;
        Ok(())
    }

    /// Whether the window owns its whole monitor and must not be touched.
    fn is_fullscreen(&self, id: WindowId) -> bool {
        let _ = id;
        false
    }

    /// Turns off DWM's own transitions for the duration of an animation.
    fn begin_animation(&self, ids: &[WindowId]) {
        let _ = ids;
    }

    /// Restores DWM's transitions.
    fn end_animation(&self, ids: &[WindowId]) {
        let _ = ids;
    }
}

/// One window that could not be placed and has therefore been let go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Refusal {
    /// The window that refused.
    pub id: WindowId,
    /// The error the operating system or the fake reported.
    pub message: String,
}

/// What a bulk operation did, for logging and for the tests.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outcome {
    /// How many windows the layout wanted to move.
    pub positioned: usize,
    /// Windows that refused a move and were unmanaged.
    pub refused: Vec<Refusal>,
    /// Decoration writes that failed; these do not unmanage the window.
    pub decoration_errors: Vec<Refusal>,
    /// The animation that ran, when one did.
    pub animation: Option<AnimationReport>,
}

impl Outcome {
    /// Whether anything went wrong.
    pub fn has_errors(&self) -> bool {
        !self.refused.is_empty() || !self.decoration_errors.is_empty()
    }
}

/// A measurement of the animation that just ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationReport {
    /// How many windows moved.
    pub windows: usize,
    /// How many frames were written.
    pub frames: usize,
    /// Planned duration in milliseconds, equal to `frames * 8`.
    pub duration_ms: u32,
    /// Wall time the stepping loop actually took, in milliseconds.
    pub elapsed_ms: u32,
}

impl AnimationReport {
    /// The report as IPC JSON.
    pub fn to_json(self) -> Value {
        serde_json::json!({
            "windows": self.windows,
            "frames": self.frames,
            "durationMs": self.duration_ms,
            "elapsedMs": self.elapsed_ms,
            "stepMs": animation::STEP_MS,
        })
    }
}

/// The tiling manager's model and policy, with no Win32 state of its own.
#[derive(Debug, Clone)]
pub struct Engine {
    /// Managed windows and workspaces.
    pub model: WorkspaceModel,
    /// Current layout.
    pub layout: LayoutKind,
    /// Spacing for the layouts.
    pub params: LayoutParams,
    /// Whether layout changes animate.
    pub animations: bool,
    /// Whether management is on.
    pub enabled: bool,
    /// Accent colour for the border decoration.
    pub accent: Rgb,
    /// The last animation, for `wm.getState` and the report.
    pub last_animation: Option<AnimationReport>,
}

impl Engine {
    /// Builds an engine from the persisted settings and the theme accent.
    pub fn new(settings: &WindowManagerSettings, accent: Rgb) -> Self {
        let layout = LayoutKind::parse(settings.layout()).unwrap_or_default();
        let params = LayoutParams {
            gaps: settings.gaps(),
            outer_gaps: settings.outer_gaps(),
            border: settings.border(),
            master_ratio: layout::DEFAULT_MASTER_RATIO,
        };

        Self {
            model: WorkspaceModel::new(settings.workspaces()),
            layout,
            params,
            animations: settings.animations,
            enabled: settings.enabled,
            accent,
            last_animation: None,
        }
    }

    /// The current parameters with the layout defaults.
    pub fn layout_params(&self) -> LayoutParams {
        self.params
    }

    /// Sets the layout and returns whether it changed.
    pub fn set_layout(&mut self, kind: LayoutKind) -> bool {
        if self.layout == kind {
            return false;
        }

        self.layout = kind;
        true
    }

    /// Cycles to the next layout.
    pub fn cycle_layout(&mut self) -> LayoutKind {
        self.layout = self.layout.next();
        self.layout
    }

    /// The target rectangle of every tile on the active workspace, in tiling order.
    ///
    /// Pure with respect to the desktop: the engine does not move anything here, so this is also
    /// what the tests and the report use to draw a layout.
    pub fn targets(&self, area: Rect) -> Vec<(WindowId, Rect)> {
        let ids = self.model.tiles();
        let rects = layout::plan(self.layout, area, ids.len(), self.params);

        ids.into_iter().zip(rects).collect()
    }

    /// Adds a window and gives it its decorations and visibility.
    ///
    /// A failed show is reported rather than swallowed: the window becomes unmanaged again, so the
    /// caller can log it and the model never claims a window it cannot control.
    pub fn manage<O: WindowOps>(&mut self, ops: &O, window: Window) -> Result<(), String> {
        if !self.enabled {
            return Err("the manager is disabled".to_owned());
        }

        let id = window.id;
        let on_active = window.workspace == self.model.active();
        let no_border = window.no_border;

        if !self.model.manage(window) {
            return Ok(());
        }

        if let Some(managed) = self.model.get_mut(id) {
            let plan = decorations::apply(self.accent, no_border);
            match ops.set_decorations(id, &plan) {
                Ok(()) => managed.decorated = true,
                Err(error) => return Err(format!("cannot decorate {id}: {error}")),
            }
        }

        if let Err(error) = ops.show(id, on_active) {
            self.model.forget(id);
            return Err(format!("cannot show {id}: {error}"));
        }

        Ok(())
    }

    /// Removes a window and puts its rectangle and decorations back.
    ///
    /// Returns the errors that happened while restoring. Every one is worth logging, but none of
    /// them may leave the window in the model.
    pub fn unmanage<O: WindowOps>(&mut self, ops: &O, id: WindowId) -> Vec<String> {
        let Some(window) = self.model.get(id).cloned() else {
            return Vec::new();
        };
        self.model.unmanage(id);

        let mut errors = Vec::new();
        if window.decorated {
            if let Err(error) = ops.set_decorations(id, &decorations::restore()) {
                errors.push(format!("cannot restore decorations of {id}: {error}"));
            }
        }
        if let Err(error) = ops.set_rect(id, window.original) {
            errors.push(format!("cannot restore rectangle of {id}: {error}"));
        }
        if let Err(error) = ops.show(id, true) {
            errors.push(format!("cannot show {id} after unmanaging: {error}"));
        }

        errors
    }

    /// Shows the windows of the active workspace and hides the rest.
    ///
    /// The order matters for what the user sees: windows are shown before the layout pass, so the
    /// animation starts from a real rectangle rather than from the hidden state.
    pub fn sync_visibility<O: WindowOps>(&mut self, ops: &O) -> Outcome {
        let mut outcome = Outcome::default();
        if !self.enabled {
            return outcome;
        }

        let active = self.model.active();
        let desired: Vec<(WindowId, bool, bool)> = self
            .model
            .windows()
            .iter()
            .map(|window| {
                (
                    window.id,
                    window.workspace == active,
                    ops.is_fullscreen(window.id),
                )
            })
            .collect();

        for (id, visible, fullscreen) in desired {
            if fullscreen {
                continue;
            }
            if let Err(error) = ops.show(id, visible) {
                outcome.refused.push(Refusal {
                    id,
                    message: format!("cannot change visibility of {id}: {error}"),
                });
                self.model.forget(id);
            }
        }

        outcome
    }

    /// Applies the layout to the active workspace.
    ///
    /// Windows that refuse a move are removed from the model and their original rectangle and
    /// decorations are written back (best effort). One broken application must not stop the rest
    /// from being tiled, and must not keep the manager trying forever.
    pub fn apply<O: WindowOps>(&mut self, ops: &O, area: Rect) -> Outcome {
        let mut outcome = Outcome::default();
        if !self.enabled {
            return outcome;
        }

        let ids: Vec<WindowId> = self
            .model
            .tiles()
            .into_iter()
            .filter(|id| !ops.is_fullscreen(*id))
            .collect();
        let rects = layout::plan(self.layout, area, ids.len(), self.params);

        let mut finals: Vec<(WindowId, Rect)> = Vec::new();
        let mut moves: Vec<Move> = Vec::new();
        let mut failures: Vec<Refusal> = Vec::new();

        for (id, target) in ids.into_iter().zip(rects) {
            match ops.rect(id) {
                Ok(current) => {
                    if current != target {
                        finals.push((id, target));
                        moves.push(Move::new(current, target));
                    }
                }
                Err(message) => failures.push(Refusal { id, message }),
            }
        }

        outcome.positioned = finals.len();

        if self.animations && animation::should_animate(&moves) {
            let moving: Vec<WindowId> = finals.iter().map(|(id, _)| *id).collect();
            let plan = animation::plan_layout(&moves);

            ops.begin_animation(&moving);
            let elapsed = self.run_frames(ops, &finals, &plan, &mut failures);
            let elapsed_ms = u32::try_from(elapsed.as_millis()).unwrap_or(u32::MAX);
            ops.end_animation(&moving);

            self.last_animation = Some(AnimationReport {
                windows: moving.len(),
                frames: plan.frame_count(),
                duration_ms: plan.duration_ms,
                elapsed_ms,
            });
            outcome.animation = self.last_animation;
        } else {
            for (id, target) in &finals {
                if let Err(message) = ops.set_rect(*id, *target) {
                    failures.push(Refusal { id: *id, message });
                }
            }
        }

        // A failure repeated on every frame is still one broken window.
        dedupe(&mut failures);
        for refusal in failures {
            let window = self.model.get(refusal.id).cloned();
            self.model.unmanage(refusal.id);

            if let Some(window) = window {
                if window.decorated {
                    let _ = ops.set_decorations(refusal.id, &decorations::restore());
                }
                let _ = ops.set_rect(refusal.id, window.original);
                let _ = ops.show(refusal.id, true);
            }

            outcome.refused.push(refusal);
        }

        outcome
    }

    /// Writes the animation frames on an absolute 8 ms schedule.
    ///
    /// The deadline for frame `i` is measured from the start of the loop, not from the end of the
    /// previous frame. Without that, every `SetWindowPos` and every scheduling delay is added to
    /// the step and a nominal 176 ms animation drifts past the 180 ms cap on a busy desktop. With
    /// it, the cost of writing a frame is absorbed into the step as long as it is under 8 ms.
    fn run_frames<O: WindowOps>(
        &self,
        ops: &O,
        finals: &[(WindowId, Rect)],
        plan: &AnimationPlan,
        failures: &mut Vec<Refusal>,
    ) -> Duration {
        let started = std::time::Instant::now();
        let step = Duration::from_millis(u64::from(animation::STEP_MS));

        for (index, frame) in plan.frames.iter().enumerate() {
            let deadline = started + step * (index as u32 + 1);
            let now = std::time::Instant::now();
            if deadline > now {
                std::thread::sleep(deadline - now);
            }

            for ((id, _), target) in finals.iter().zip(frame) {
                if let Err(message) = ops.set_rect(*id, *target) {
                    failures.push(Refusal { id: *id, message });
                }
            }
        }

        started.elapsed()
    }

    /// Puts every managed window back and stops managing.
    ///
    /// This is the "disable" contract: original rectangle, original decorations, visible again,
    /// whichever workspace it happened to be on. It runs even for windows whose workspace was
    /// hidden, because the user asked for the desktop back.
    pub fn disable<O: WindowOps>(&mut self, ops: &O) -> Outcome {
        let mut outcome = Outcome::default();
        self.enabled = false;

        for window in self.model.clear() {
            if window.decorated {
                if let Err(message) = ops.set_decorations(window.id, &decorations::restore()) {
                    outcome.decoration_errors.push(Refusal {
                        id: window.id,
                        message,
                    });
                }
            }
            if let Err(message) = ops.set_rect(window.id, window.original) {
                outcome.refused.push(Refusal {
                    id: window.id,
                    message,
                });
            }
            if let Err(message) = ops.show(window.id, true) {
                outcome.refused.push(Refusal {
                    id: window.id,
                    message,
                });
            }
        }

        self.last_animation = None;
        outcome
    }

    /// Toggles the float state of the focused window; returns whether it changed.
    pub fn toggle_float(&mut self) -> bool {
        let Some(id) = self.model.focused() else {
            return false;
        };

        let Some(window) = self.model.get(id) else {
            return false;
        };
        let floating = !window.floating;

        self.model.set_floating(id, floating)
    }

    /// Focuses the neighbour of the focused tile in `direction`.
    ///
    /// Without a focused window the first tile gets focus, so the hotkey is never a no-op on a
    /// workspace that has windows.
    pub fn focus_direction<O: WindowOps>(
        &mut self,
        ops: &O,
        area: Rect,
        direction: Direction,
    ) -> Option<WindowId> {
        let targets = self.targets(area);
        if targets.is_empty() {
            return None;
        }

        let focused = self.model.focused();
        let Some(current) = focused.and_then(|id| targets.iter().position(|(tile, _)| *tile == id))
        else {
            let id = targets[0].0;
            self.model.focus(id);
            let _ = ops.focus(id);
            return Some(id);
        };

        let rects: Vec<Rect> = targets.iter().map(|(_, rect)| *rect).collect();
        let next = layout::neighbour(&rects, current, direction)?;
        let id = targets[next].0;

        self.model.focus(id);
        if let Err(error) = ops.focus(id) {
            // Focus is a convenience; a refusal is worth reporting by the caller, but it must not
            // unmanage a window that is otherwise tiled correctly.
            let _ = error;
        }

        Some(id)
    }

    /// Swaps the focused tile with its neighbour in `direction`; returns whether the order changed.
    pub fn move_direction(&mut self, area: Rect, direction: Direction) -> bool {
        let targets = self.targets(area);
        let Some(focused) = self.model.focused() else {
            return false;
        };
        let Some(current) = targets.iter().position(|(tile, _)| *tile == focused) else {
            return false;
        };

        let rects: Vec<Rect> = targets.iter().map(|(_, rect)| *rect).collect();
        let Some(neighbour) = layout::neighbour(&rects, current, direction) else {
            return false;
        };

        self.model
            .swap_in_order(focused, neighbour as i32 - current as i32)
    }

    /// Focuses the next (`delta = 1`) or previous window and returns it.
    pub fn focus_next<O: WindowOps>(&mut self, ops: &O, delta: i32) -> Option<WindowId> {
        let id = self.model.focus_next(delta)?;
        let _ = ops.focus(id);
        Some(id)
    }

    /// A snapshot of the manager for `wm.getState`.
    pub fn to_json<O: WindowOps>(&self, ops: &O) -> Value {
        let windows: Vec<Value> = self
            .model
            .windows()
            .iter()
            .map(|window| {
                let current = ops.rect(window.id).ok();
                serde_json::json!({
                    "id": window.id.0,
                    "title": window.title,
                    "class": window.class,
                    "exe": window.exe,
                    "workspace": window.workspace,
                    "floating": window.floating,
                    "noBorder": window.no_border,
                    "rule": window.rule,
                    "current": current.map(Rect::to_json),
                    "original": window.original.to_json(),
                    "decorations": ops.read_decorations(window.id).to_json(),
                })
            })
            .collect();

        serde_json::json!({
            "enabled": self.enabled,
            "layout": self.layout.as_str(),
            "activeWorkspace": self.model.active(),
            "workspaces": self.model.workspaces(),
            "focused": self.model.focused().map(|id| id.0),
            "animations": self.animations,
            "gaps": self.params.gaps,
            "outerGaps": self.params.outer_gaps,
            "border": self.params.border,
            "managed": self.model.len(),
            "windows": windows,
            "lastAnimation": self.last_animation.map(AnimationReport::to_json),
        })
    }
}

/// Removes repeated refusals for the same window, keeping the first message.
fn dedupe(refusals: &mut Vec<Refusal>) {
    let mut unique: Vec<Refusal> = Vec::with_capacity(refusals.len());

    for refusal in refusals.drain(..) {
        if !unique.iter().any(|kept| kept.id == refusal.id) {
            unique.push(refusal);
        }
    }

    *refusals = unique;
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::{HashMap, HashSet};

    use super::*;
    use crate::rules::WindowFacts;

    const SCREEN: Rect = Rect::new(0, 0, 1920, 1080);

    #[derive(Debug, Default)]
    struct FakeOps {
        rects: RefCell<HashMap<WindowId, Rect>>,
        refuses: RefCell<HashSet<WindowId>>,
        fullscreen: RefCell<HashSet<WindowId>>,
        visible: RefCell<HashMap<WindowId, bool>>,
        decorations: RefCell<HashMap<WindowId, DecorationPlan>>,
        readbacks: RefCell<HashMap<WindowId, DecorationReadback>>,
        focused: RefCell<Option<WindowId>>,
        writes: RefCell<usize>,
        transitions: RefCell<Vec<bool>>,
    }

    impl FakeOps {
        fn refusing(ids: &[WindowId]) -> Self {
            let ops = Self::default();
            ops.refuses.borrow_mut().extend(ids.iter().copied());
            ops
        }

        fn rect_at(&self, id: WindowId, rect: Rect) {
            self.rects.borrow_mut().insert(id, rect);
        }

        fn rect_of(&self, id: WindowId) -> Rect {
            *self
                .rects
                .borrow()
                .get(&id)
                .expect("the window has a rectangle")
        }
    }

    impl WindowOps for FakeOps {
        fn rect(&self, id: WindowId) -> Result<Rect, String> {
            self.rects
                .borrow()
                .get(&id)
                .copied()
                .ok_or_else(|| format!("{id} is gone"))
        }

        fn set_rect(&self, id: WindowId, rect: Rect) -> Result<(), String> {
            *self.writes.borrow_mut() += 1;
            if self.refuses.borrow().contains(&id) {
                return Err(format!("{id} refuses to be moved"));
            }
            self.rects.borrow_mut().insert(id, rect);
            Ok(())
        }

        fn show(&self, id: WindowId, visible: bool) -> Result<(), String> {
            self.visible.borrow_mut().insert(id, visible);
            Ok(())
        }

        fn set_decorations(&self, id: WindowId, plan: &DecorationPlan) -> Result<(), String> {
            self.decorations.borrow_mut().insert(id, *plan);
            Ok(())
        }

        fn read_decorations(&self, id: WindowId) -> DecorationReadback {
            self.readbacks
                .borrow()
                .get(&id)
                .copied()
                .unwrap_or_default()
        }

        fn focus(&self, id: WindowId) -> Result<(), String> {
            *self.focused.borrow_mut() = Some(id);
            Ok(())
        }

        fn is_fullscreen(&self, id: WindowId) -> bool {
            self.fullscreen.borrow().contains(&id)
        }

        fn begin_animation(&self, _ids: &[WindowId]) {
            self.transitions.borrow_mut().push(true);
        }

        fn end_animation(&self, _ids: &[WindowId]) {
            self.transitions.borrow_mut().push(false);
        }
    }

    fn settings() -> WindowManagerSettings {
        WindowManagerSettings {
            enabled: true,
            layout: "masterstack".to_owned(),
            gaps: 0,
            outer_gaps: 0,
            border: 0,
            workspaces: 4,
            animations: false,
            rules: Vec::new(),
        }
    }

    fn engine() -> Engine {
        Engine::new(
            &settings(),
            Rgb {
                red: 0xA7,
                green: 0x8B,
                blue: 0xFA,
            },
        )
    }

    fn window(id: isize, workspace: usize) -> Window {
        let mut window = Window::new(
            WindowId(id),
            WindowFacts::new("app.exe", "AppClass", format!("Window {id}")),
            Rect::new(40, 40, 300, 200),
        );
        window.workspace = workspace;
        window
    }

    fn managed(engine: &mut Engine, ops: &FakeOps, count: isize) {
        for id in 1..=count {
            let id = WindowId(id);
            ops.rect_at(id, Rect::new(40, 40, 300, 200));
            engine
                .manage(ops, window(id.0, 0))
                .expect("the fake shows and decorates everything");
        }
    }

    #[test]
    fn the_engine_reads_its_policy_from_the_settings() {
        let engine = engine();

        assert!(engine.enabled);
        assert_eq!(engine.layout, LayoutKind::MasterStack);
        assert_eq!(engine.params.gaps, 0);
        assert_eq!(engine.model.workspaces(), 4);
        assert!(!engine.animations);

        let fallback = Engine::new(
            &WindowManagerSettings {
                layout: "does-not-exist".to_owned(),
                ..WindowManagerSettings::default()
            },
            Rgb {
                red: 0,
                green: 0,
                blue: 0,
            },
        );
        assert_eq!(fallback.layout, LayoutKind::Dwindle);
    }

    #[test]
    fn managing_decorates_and_shows_a_window_on_the_active_workspace() {
        let mut engine = engine();
        let ops = FakeOps::default();
        let id = WindowId(1);
        ops.rect_at(id, Rect::new(10, 10, 400, 300));

        engine.manage(&ops, window(1, 0)).expect("manage");

        assert!(engine.model.contains(id));
        assert_eq!(
            ops.visible.borrow().get(&id),
            Some(&true),
            "a window on the active workspace is shown"
        );
        let plan = ops
            .decorations
            .borrow()
            .get(&id)
            .copied()
            .expect("decorated");
        assert_eq!(plan.corner, Some(decorations::CORNER_ROUND));
        assert_eq!(plan.dark, Some(1));
    }

    #[test]
    fn a_window_managed_on_another_workspace_is_hidden_at_once() {
        let mut engine = engine();
        let ops = FakeOps::default();
        let id = WindowId(2);
        ops.rect_at(id, Rect::new(10, 10, 400, 300));

        engine.manage(&ops, window(2, 2)).expect("manage");

        assert_eq!(ops.visible.borrow().get(&id), Some(&false));
        assert!(engine.model.tiles().is_empty());
    }

    #[test]
    fn a_refusing_window_is_unmanaged_and_does_not_stop_the_others() {
        let mut engine = engine();
        let ops = FakeOps::refusing(&[WindowId(2)]);
        managed(&mut engine, &ops, 3);

        let outcome = engine.apply(&ops, SCREEN);

        assert_eq!(outcome.refused.len(), 1);
        assert_eq!(outcome.refused[0].id, WindowId(2));
        assert!(
            outcome.refused[0].message.contains("refuses"),
            "{:?}",
            outcome.refused
        );
        assert!(!engine.model.contains(WindowId(2)), "the window was let go");
        assert_eq!(engine.model.len(), 2);
        assert_eq!(
            engine.targets(SCREEN).len(),
            2,
            "the others retile without it"
        );
        assert_eq!(
            outcome.positioned, 3,
            "the refused window was still attempted"
        );
    }

    #[test]
    fn a_window_that_refuses_during_an_animation_is_unmanaged_too() {
        let mut engine = engine();
        engine.animations = true;
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 3);

        // The first pass succeeds, so the window has a real rectangle to animate from.
        engine.apply(&ops, SCREEN);
        ops.refuses.borrow_mut().insert(WindowId(2));
        ops.transitions.borrow_mut().clear();

        engine.set_layout(LayoutKind::Grid);
        let outcome = engine.apply(&ops, SCREEN);

        assert_eq!(outcome.refused.len(), 1);
        assert!(!engine.model.contains(WindowId(2)));
        assert!(
            ops.transitions.borrow().as_slice() == [true, false],
            "DWM transitions must be restored after the animation"
        );
    }

    #[test]
    fn disabling_restores_every_rectangle_decoration_and_visibility() {
        let mut engine = engine();
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 3);
        engine.apply(&ops, SCREEN);
        engine.model.set_workspace(WindowId(3), 2);
        engine.sync_visibility(&ops);

        let outcome = engine.disable(&ops);

        assert!(!outcome.has_errors(), "{outcome:?}");
        assert!(!engine.enabled);
        assert!(engine.model.is_empty());
        for id in [1, 2, 3] {
            let id = WindowId(id);
            assert_eq!(
                ops.rect_of(id),
                Rect::new(40, 40, 300, 200),
                "the original rectangle is back"
            );
            assert_eq!(
                ops.visible.borrow().get(&id),
                Some(&true),
                "and it is visible"
            );
            let plan = ops
                .decorations
                .borrow()
                .get(&id)
                .copied()
                .expect("restored");
            assert_eq!(plan.corner, Some(decorations::CORNER_DONOTROUND));
            assert_eq!(plan.dark, Some(0));
            assert_eq!(plan.border, Some(decorations::COLOR_DEFAULT));
        }
    }

    #[test]
    fn switching_workspaces_hides_the_old_set_and_shows_the_new_one() {
        let mut engine = engine();
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 2);
        engine.model.set_workspace(WindowId(1), 1);

        engine.model.set_active(1);
        let outcome = engine.sync_visibility(&ops);
        assert!(!outcome.has_errors());

        assert_eq!(ops.visible.borrow().get(&WindowId(1)), Some(&true));
        assert_eq!(
            ops.visible.borrow().get(&WindowId(2)),
            Some(&false),
            "the window left behind is hidden"
        );
    }

    #[test]
    fn an_animation_reports_twenty_two_frames_and_one_hundred_and_seventy_six_milliseconds() {
        let mut engine = engine();
        engine.animations = true;
        engine.set_layout(LayoutKind::Dwindle);
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 3);

        let first = engine.apply(&ops, SCREEN);
        assert!(
            first.animation.is_some(),
            "the first layout moves every window"
        );
        let report = first.animation.expect("animation");
        assert_eq!(report.frames, 22);
        assert_eq!(report.duration_ms, 176);
        assert_eq!(report.windows, 3);
        assert!(
            report.duration_ms <= animation::MAX_DURATION_MS,
            "the cap holds"
        );
    }

    #[test]
    fn a_move_smaller_than_two_pixels_is_not_animated() {
        let mut engine = engine();
        engine.animations = true;
        let ops = FakeOps::default();
        let id = WindowId(1);
        ops.rect_at(id, Rect::new(0, 0, 1920, 1080));
        engine.manage(&ops, window(1, 0)).expect("manage");

        // The monocle tile is one pixel inside the fake's rectangle, so the move is invisible.
        engine.set_layout(LayoutKind::Monocle);
        let writes_before = *ops.writes.borrow();
        let outcome = engine.apply(&ops, Rect::new(1, 1, 1919, 1079));

        assert!(outcome.animation.is_none());
        assert_eq!(
            *ops.writes.borrow() - writes_before,
            1,
            "one final write, no frames"
        );
    }

    #[test]
    fn a_fullscreen_window_is_never_moved_and_the_others_retile_around_it() {
        let mut engine = engine();
        engine.set_layout(LayoutKind::Dwindle);
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 2);
        let fullscreen_rect = Rect::new(0, 0, 1920, 1080);
        ops.rect_at(WindowId(2), fullscreen_rect);
        ops.fullscreen.borrow_mut().insert(WindowId(2));

        let outcome = engine.apply(&ops, SCREEN);

        assert!(!outcome.has_errors());
        assert_eq!(
            ops.rect_of(WindowId(2)),
            fullscreen_rect,
            "a game is never touched"
        );
        assert_eq!(
            engine.targets(SCREEN).len(),
            2,
            "the model still holds both windows"
        );
        assert_eq!(
            ops.rect_of(WindowId(1)),
            Rect::new(0, 0, 1920, 1080),
            "the other window gets the whole area"
        );
    }

    #[test]
    fn floating_windows_are_not_in_the_target_list() {
        let mut engine = engine();
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 3);
        assert!(engine.model.set_floating(WindowId(2), true));

        let targets = engine.targets(SCREEN);

        assert_eq!(targets.len(), 2);
        assert!(targets.iter().all(|(id, _)| *id != WindowId(2)));
    }

    #[test]
    fn toggling_float_affects_the_focused_window() {
        let mut engine = engine();
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 2);

        assert!(engine.toggle_float());
        assert_eq!(
            engine.model.get(WindowId(1)).map(|window| window.floating),
            Some(true)
        );
        assert!(engine.toggle_float());
        assert_eq!(
            engine.model.get(WindowId(1)).map(|window| window.floating),
            Some(false)
        );
    }

    #[test]
    fn focus_moves_between_tiles_and_never_leaves_the_workspace() {
        let mut engine = engine();
        engine.set_layout(LayoutKind::Grid);
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 4);
        engine.apply(&ops, SCREEN);

        let right = engine.focus_direction(&ops, SCREEN, Direction::Right);
        assert_eq!(right, Some(WindowId(2)));
        assert_eq!(*ops.focused.borrow(), Some(WindowId(2)));

        let down = engine.focus_direction(&ops, SCREEN, Direction::Down);
        assert_eq!(down, Some(WindowId(4)));

        assert_eq!(engine.focus_direction(&ops, SCREEN, Direction::Down), None);
    }

    #[test]
    fn moving_a_window_swaps_it_with_its_neighbour() {
        let mut engine = engine();
        engine.set_layout(LayoutKind::Grid);
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 4);
        engine.apply(&ops, SCREEN);

        assert!(engine.move_direction(SCREEN, Direction::Right));
        assert_eq!(
            engine.model.tiles(),
            [WindowId(2), WindowId(1), WindowId(3), WindowId(4)],
            "the focused window moved one place right"
        );
        assert!(engine.model.focused().is_some());
    }

    #[test]
    fn focus_next_wraps_and_updates_the_fake_desktop() {
        let mut engine = engine();
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 2);

        assert_eq!(engine.focus_next(&ops, 1), Some(WindowId(2)));
        assert_eq!(*ops.focused.borrow(), Some(WindowId(2)));
        assert_eq!(engine.focus_next(&ops, 1), Some(WindowId(1)));
    }

    #[test]
    fn the_snapshot_carries_the_model_the_rectangles_and_the_decorations() {
        let mut engine = engine();
        let ops = FakeOps::default();
        managed(&mut engine, &ops, 1);
        ops.readbacks.borrow_mut().insert(
            WindowId(1),
            DecorationReadback {
                corner: Some(2),
                dark: Some(1),
                border: None,
            },
        );

        let state = engine.to_json(&ops);

        assert_eq!(state["enabled"], serde_json::json!(true));
        assert_eq!(state["layout"], serde_json::json!("masterstack"));
        assert_eq!(state["managed"], serde_json::json!(1));
        assert_eq!(state["windows"][0]["id"], serde_json::json!(1));
        assert_eq!(
            state["windows"][0]["decorations"]["corner"],
            serde_json::json!(2)
        );
        assert_eq!(
            state["windows"][0]["decorations"]["dark"],
            serde_json::json!(1)
        );
        assert_eq!(
            state["windows"][0]["original"]["width"],
            serde_json::json!(260)
        );
    }

    #[test]
    fn unmanaging_restores_the_window_and_tolerates_a_failing_restore() {
        let mut engine = engine();
        let ops = FakeOps::refusing(&[WindowId(1)]);
        managed(&mut engine, &ops, 1);

        let errors = engine.unmanage(&ops, WindowId(1));

        assert!(!engine.model.contains(WindowId(1)));
        assert!(
            errors
                .iter()
                .any(|error| error.contains("restore rectangle")),
            "{errors:?}"
        );
    }
}
