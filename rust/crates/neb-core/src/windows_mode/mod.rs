//! "Замена Windows": reversible hiding of the default Windows shell chrome.
//!
//! Seven user-facing toggles take the parts of Explorer's own shell out of the way so the ProjectNeb
//! bar, dock and wallpaper can stand alone. Every one of them is a plain `HKCU` write, needs no
//! administrator and is recorded before it happens, so the machine can always be put back:
//!
//! | Toggle | Registry | Hidden | Shown |
//! | --- | --- | --- | --- |
//! | `taskbar` | `Explorer\StuckRects3\Settings` | auto-hide bit set | bit as before |
//! | `desktopIcons` | `Explorer\Advanced\HideIcons` | `1` | `0` |
//! | `searchBox` | `CurrentVersion\Search\SearchboxTaskbarMode` | `0` | `2` |
//! | `taskView` | `Explorer\Advanced\ShowTaskViewButton` | `0` | `1` |
//! | `widgets` | `Explorer\Advanced\TaskbarDa` | `0` | `1` |
//! | `copilot` | `Explorer\Advanced\ShowCopilotButton` | `0` | `1` |
//! | `news` | `Explorer\Advanced\ShellFeedsTaskbarViewMode` | `2` | `0` |
//!
//! The taskbar is the one value that is bytes rather than a number: see [`taskbar`] for the layout
//! transform, which the crash repair in `neb-supervisor` shares.
//!
//! # The contract
//!
//! * Before the first change, [`Session`] writes `windows-mode.json` with the previous raw value of
//!   every toggle it is about to hide (or "absent"), so a crash cannot lose the way back.
//! * [`Session::restore`] writes those values back and removes a value that did not exist before,
//!   then drops the entry; a fully restored file is deleted.
//! * Applying and restoring the same toggle twice is a no-op: [`Session::sync`] only touches the
//!   toggles whose desired state differs from the recorded one.
//! * A state file that cannot be decoded reads as empty and says why, instead of inventing previous
//!   values.
//!
//! The pure plans ([`plan_write`], [`plan_restore`]) and the state file are tested without Windows;
//! [`RegistryAccess`] is the seam that keeps the tests away from `HKCU`.

use std::fmt;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::time::UtcDateTime;

mod registry;
mod state;
pub mod taskbar;

pub use registry::{MemoryRegistry, RealRegistry, RegistryAccess, SETTINGS_AREA};
pub use state::{Record, State};

/// File under the ProjectNeb data directory that records what was changed.
pub const STATE_FILE: &str = "windows-mode.json";

/// `Explorer\Advanced`, where Windows keeps most taskbar switches.
pub const ADVANCED_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Explorer\Advanced";

/// `CurrentVersion\Search`, where Windows 11 keeps the taskbar search box mode.
///
/// Older builds read the same value from `...\Explorer\Search`; Windows 11 24H2 (26100) ignores that
/// path, which was confirmed live while building this mode, so the live key is the one it owns.
pub const SEARCH_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Search";

/// One part of the Windows shell the mode can hide.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Toggle {
    /// The taskbar itself, through its auto-hide bit.
    Taskbar,
    /// The desktop icons.
    #[serde(alias = "desktop_icons")]
    DesktopIcons,
    /// The search box inside the taskbar.
    #[serde(alias = "search_box")]
    SearchBox,
    /// The task view button.
    #[serde(alias = "task_view")]
    TaskView,
    /// The widgets button.
    Widgets,
    /// The Copilot button.
    Copilot,
    /// The news and interests button.
    News,
}

impl Toggle {
    /// Every toggle, in the order the settings window lists them.
    pub const ALL: [Self; 7] = [
        Self::Taskbar,
        Self::DesktopIcons,
        Self::SearchBox,
        Self::TaskView,
        Self::Widgets,
        Self::Copilot,
        Self::News,
    ];

    /// The camelCase id used by the config and the IPC document.
    pub const fn id(self) -> &'static str {
        match self {
            Self::Taskbar => "taskbar",
            Self::DesktopIcons => "desktopIcons",
            Self::SearchBox => "searchBox",
            Self::TaskView => "taskView",
            Self::Widgets => "widgets",
            Self::Copilot => "copilot",
            Self::News => "news",
        }
    }

    /// The registry key, value, value shape and the two DWORD values of this toggle.
    pub const fn descriptor(self) -> Descriptor {
        match self {
            Self::Taskbar => Descriptor {
                key: taskbar::KEY_PATH,
                value: taskbar::VALUE_NAME,
                kind: ValueKind::Layout,
                hidden: None,
                shown: None,
            },
            Self::DesktopIcons => Descriptor {
                key: ADVANCED_KEY,
                value: "HideIcons",
                kind: ValueKind::Dword,
                hidden: Some(1),
                shown: Some(0),
            },
            Self::SearchBox => Descriptor {
                key: SEARCH_KEY,
                value: "SearchboxTaskbarMode",
                kind: ValueKind::Dword,
                hidden: Some(0),
                shown: Some(2),
            },
            Self::TaskView => Descriptor {
                key: ADVANCED_KEY,
                value: "ShowTaskViewButton",
                kind: ValueKind::Dword,
                hidden: Some(0),
                shown: Some(1),
            },
            Self::Widgets => Descriptor {
                key: ADVANCED_KEY,
                value: "TaskbarDa",
                kind: ValueKind::Dword,
                hidden: Some(0),
                shown: Some(1),
            },
            Self::Copilot => Descriptor {
                key: ADVANCED_KEY,
                value: "ShowCopilotButton",
                kind: ValueKind::Dword,
                hidden: Some(0),
                shown: Some(1),
            },
            Self::News => Descriptor {
                key: ADVANCED_KEY,
                value: "ShellFeedsTaskbarViewMode",
                kind: ValueKind::Dword,
                hidden: Some(2),
                shown: Some(0),
            },
        }
    }

    /// Reads an id: `desktopIcons` and `desktop_icons` both name the same toggle.
    pub fn parse(id: &str) -> Option<Self> {
        let normalized: String = id
            .trim()
            .chars()
            .filter(|character| *character != '_')
            .flat_map(char::to_lowercase)
            .collect();

        Self::ALL
            .into_iter()
            .find(|toggle| toggle.id().to_ascii_lowercase() == normalized)
    }
}

/// Which value shape a toggle writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueKind {
    /// A `REG_DWORD`.
    Dword,
    /// The taskbar layout bytes.
    Layout,
}

/// Everything the registry edge needs to know about one toggle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Descriptor {
    /// Key under `HKCU`.
    pub key: &'static str,
    /// Value inside the key.
    pub value: &'static str,
    /// Shape of the value.
    pub kind: ValueKind,
    /// The DWORD that hides the chrome, when the toggle is a DWORD.
    pub hidden: Option<u32>,
    /// The DWORD that shows it again.
    pub shown: Option<u32>,
}

impl Descriptor {
    /// The DWORD for the requested state, or `None` for a layout toggle.
    pub fn target(&self, hidden: bool) -> Option<u32> {
        if hidden { self.hidden } else { self.shown }
    }
}

/// A registry value exactly as it was read or as it must be written.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum RawValue {
    /// The key or value did not exist, so restoring means deleting whatever the mode wrote.
    #[default]
    Absent,
    /// A `REG_DWORD`.
    Dword {
        /// The number.
        value: u32,
    },
    /// A `REG_BINARY` value, which only the taskbar layout uses.
    Binary {
        /// The bytes.
        value: Vec<u8>,
    },
}

impl RawValue {
    /// A one-word description for log lines.
    pub fn describe(&self) -> String {
        match self {
            Self::Absent => "absent".to_owned(),
            Self::Dword { value } => value.to_string(),
            Self::Binary { value } => format!("{} bytes", value.len()),
        }
    }
}

/// The selection of toggles, one boolean per [`Toggle`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ToggleSet {
    /// Hide the taskbar.
    pub taskbar: bool,
    /// Hide the desktop icons.
    #[serde(alias = "desktop_icons")]
    pub desktop_icons: bool,
    /// Hide the taskbar search box.
    #[serde(alias = "search_box")]
    pub search_box: bool,
    /// Hide the task view button.
    #[serde(alias = "task_view")]
    pub task_view: bool,
    /// Hide the widgets button.
    pub widgets: bool,
    /// Hide the Copilot button.
    pub copilot: bool,
    /// Hide the news and interests button.
    pub news: bool,
}

impl ToggleSet {
    /// Whether `toggle` is selected.
    pub const fn get(self, toggle: Toggle) -> bool {
        match toggle {
            Toggle::Taskbar => self.taskbar,
            Toggle::DesktopIcons => self.desktop_icons,
            Toggle::SearchBox => self.search_box,
            Toggle::TaskView => self.task_view,
            Toggle::Widgets => self.widgets,
            Toggle::Copilot => self.copilot,
            Toggle::News => self.news,
        }
    }

    /// Selects or deselects `toggle`.
    pub fn set(&mut self, toggle: Toggle, selected: bool) {
        match toggle {
            Toggle::Taskbar => self.taskbar = selected,
            Toggle::DesktopIcons => self.desktop_icons = selected,
            Toggle::SearchBox => self.search_box = selected,
            Toggle::TaskView => self.task_view = selected,
            Toggle::Widgets => self.widgets = selected,
            Toggle::Copilot => self.copilot = selected,
            Toggle::News => self.news = selected,
        }
    }

    /// Whether anything is selected.
    pub fn any(self) -> bool {
        Toggle::ALL.into_iter().any(|toggle| self.get(toggle))
    }

    /// How many toggles are selected.
    pub fn count(self) -> usize {
        Toggle::ALL
            .into_iter()
            .filter(|toggle| self.get(*toggle))
            .count()
    }

    /// The selected toggles, in [`Toggle::ALL`] order.
    pub fn selected(self) -> Vec<Toggle> {
        Toggle::ALL
            .into_iter()
            .filter(|toggle| self.get(*toggle))
            .collect()
    }
}

/// The `windowsMode` object of `config.json`.
///
/// `enabled` is the master switch: when it is off, nothing is applied and everything recorded is
/// restored; the individual flags remember what the user wants for the next time it is on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct WindowsModeSettings {
    /// Whether the mode is on.
    pub enabled: bool,
    /// Which parts of the Windows shell the mode hides while it is on.
    #[serde(flatten)]
    pub toggles: ToggleSet,
}

impl WindowsModeSettings {
    /// The toggles that should be hidden right now: the selection, or none while the mode is off.
    pub fn desired(&self) -> ToggleSet {
        if self.enabled {
            self.toggles
        } else {
            ToggleSet::default()
        }
    }
}

/// Why a plan, a read or a write could not be carried out.
#[derive(Debug, thiserror::Error)]
pub enum ModeError {
    /// The registry refused the operation.
    #[error("cannot {action} '{key}\\{value}': {reason}")]
    Registry {
        /// Verbed operation, e.g. `read` or `write`.
        action: &'static str,
        /// Key under `HKCU`.
        key: String,
        /// Value inside the key.
        value: String,
        /// What Windows said.
        reason: String,
    },

    /// The taskbar layout has not been written by Explorer yet.
    #[error(
        "'{key}\\{value}' is not in the registry yet: there is no taskbar layout to transform, so the taskbar was left alone"
    )]
    TaskbarLayoutAbsent {
        /// Key under `HKCU`.
        key: String,
        /// Value inside the key.
        value: String,
    },

    /// The taskbar layout is too short to carry the auto-hide bit.
    #[error(
        "'{key}\\{value}' is {len} bytes long, too short to carry the auto-hide flag; the taskbar was left alone"
    )]
    TaskbarLayoutTooShort {
        /// Key under `HKCU`.
        key: String,
        /// Value inside the key.
        value: String,
        /// How many bytes the value holds.
        len: usize,
    },

    /// The value is not the shape this toggle writes.
    #[error("'{key}\\{value}' is not the shape this toggle expects: {reason}")]
    UnexpectedValue {
        /// Key under `HKCU`.
        key: String,
        /// Value inside the key.
        value: String,
        /// What was found instead.
        reason: String,
    },

    /// The state file could not be read, written or removed.
    #[error("cannot {action} '{}': {source}", path.display())]
    StateFile {
        /// Verbed operation, e.g. `write` or `replace`.
        action: &'static str,
        /// File the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },

    /// The state could not be rendered as JSON.
    #[error("windows-mode state for '{}' is not valid json: {reason}", path.display())]
    StateJson {
        /// File that could not be rendered.
        path: PathBuf,
        /// What the serialiser said.
        reason: String,
    },
}

impl ModeError {
    /// A registry failure for `descriptor`.
    fn registry(action: &'static str, descriptor: Descriptor, reason: impl Into<String>) -> Self {
        Self::Registry {
            action,
            key: descriptor.key.to_owned(),
            value: descriptor.value.to_owned(),
            reason: reason.into(),
        }
    }
}

/// What `windows.set` asked for, parsed from its `params`.
///
/// Both shapes are accepted:
///
/// ```json
/// { "enabled": true, "toggles": { "desktopIcons": true, "searchBox": true } }
/// ```
///
/// and the flat form where the toggles sit next to `enabled`. A key that names no toggle and is not
/// `enabled`/`toggles` is an error, not something to ignore: a typo in a settings UI must not look
/// like a successful no-op.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Update {
    enabled: Option<bool>,
    toggles: [Option<bool>; Toggle::ALL.len()],
}

impl Update {
    /// Parses `params`; `None` (a request without parameters) changes nothing.
    pub fn parse(params: Option<&Value>) -> Result<Self, UpdateError> {
        let Some(value) = params else {
            return Ok(Self::default());
        };
        let Value::Object(object) = value else {
            return Err(UpdateError::NotAnObject);
        };

        let mut update = Self::default();

        for (key, value) in object {
            match key.as_str() {
                "enabled" => {
                    update.enabled = Some(
                        value
                            .as_bool()
                            .ok_or_else(|| UpdateError::NotABoolean { key: key.clone() })?,
                    );
                }
                "toggles" => {
                    let Value::Object(toggles) = value else {
                        return Err(UpdateError::TogglesNotAnObject);
                    };
                    for (id, flag) in toggles {
                        let toggle = Toggle::parse(id)
                            .ok_or_else(|| UpdateError::UnknownToggle(id.clone()))?;
                        let flag = flag
                            .as_bool()
                            .ok_or_else(|| UpdateError::NotABoolean { key: id.clone() })?;
                        update.toggles[toggle as usize] = Some(flag);
                    }
                }
                other => {
                    let toggle = Toggle::parse(other)
                        .ok_or_else(|| UpdateError::UnknownKey(other.to_owned()))?;
                    let flag = value.as_bool().ok_or_else(|| UpdateError::NotABoolean {
                        key: other.to_owned(),
                    })?;
                    update.toggles[toggle as usize] = Some(flag);
                }
            }
        }

        Ok(update)
    }

    /// The requested master switch, when the caller named one.
    pub fn enabled(self) -> Option<bool> {
        self.enabled
    }

    /// The requested state of `toggle`, when the caller named one.
    pub fn toggle(self, toggle: Toggle) -> Option<bool> {
        self.toggles[toggle as usize]
    }

    /// Whether any toggle was named at all.
    pub fn touches_toggles(self) -> bool {
        self.toggles.iter().any(Option::is_some)
    }
}

/// Why `windows.set` parameters were refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UpdateError {
    /// The parameters were not a JSON object.
    #[error("'windows.set' expects a json object of toggles")]
    NotAnObject,

    /// `toggles` was present but not an object.
    #[error("'windows.set' expects 'toggles' to be an object")]
    TogglesNotAnObject,

    /// A key that names neither `enabled`, `toggles` nor a toggle.
    #[error("unknown key '{0}' in 'windows.set'")]
    UnknownKey(String),

    /// A toggle object key that names no toggle.
    #[error("unknown toggle '{0}' in 'windows.set'")]
    UnknownToggle(String),

    /// A flag that was not a boolean.
    #[error("'{key}' in 'windows.set' must be true or false")]
    NotABoolean {
        /// Which key was wrong.
        key: String,
    },
}

/// The raw value a toggle must write for `hidden`.
///
/// A DWORD toggle ignores `current`; the layout toggle needs the bytes Explorer stored. A missing or
/// short layout is an error rather than a guess, because inventing a taskbar layout would move the
/// taskbar to a monitor or an edge the user never chose.
pub fn plan_write(toggle: Toggle, current: &RawValue, hidden: bool) -> Result<RawValue, ModeError> {
    let descriptor = toggle.descriptor();

    if let Some(value) = descriptor.target(hidden) {
        return Ok(RawValue::Dword { value });
    }

    match current {
        RawValue::Binary { value } if value.len() > taskbar::FLAG_INDEX => Ok(RawValue::Binary {
            value: taskbar::with_hidden(value, hidden),
        }),
        RawValue::Binary { value } => Err(ModeError::TaskbarLayoutTooShort {
            key: descriptor.key.to_owned(),
            value: descriptor.value.to_owned(),
            len: value.len(),
        }),
        RawValue::Absent => Err(ModeError::TaskbarLayoutAbsent {
            key: descriptor.key.to_owned(),
            value: descriptor.value.to_owned(),
        }),
        RawValue::Dword { .. } => Err(ModeError::UnexpectedValue {
            key: descriptor.key.to_owned(),
            value: descriptor.value.to_owned(),
            reason: "a number where the layout bytes belong".to_owned(),
        }),
    }
}

/// What restoring `previous` means now.
///
/// An absent previous value becomes [`RawValue::Absent`], which the caller turns into a delete. A
/// layout keeps the *current* bytes and only moves the auto-hide bit back, so a monitor change made
/// while the mode was on survives the restore.
pub fn plan_restore(previous: &RawValue, current: &RawValue) -> RawValue {
    match previous {
        RawValue::Absent => RawValue::Absent,
        RawValue::Dword { .. } => previous.clone(),
        RawValue::Binary { value } => {
            let restored = match current {
                RawValue::Binary { value: current } => taskbar::plan_restore(Some(current), value),
                _ => value.clone(),
            };

            RawValue::Binary { value: restored }
        }
    }
}

/// One step a [`Session::sync`] took, for the supervisor log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// A toggle was hidden; `previous` is what the state file now holds.
    Applied {
        /// Which toggle.
        toggle: Toggle,
        /// The value from before the change.
        previous: RawValue,
    },
    /// A toggle was put back.
    Restored {
        /// Which toggle.
        toggle: Toggle,
        /// The value that was restored.
        previous: RawValue,
    },
    /// One toggle failed; the rest of the plan still ran.
    Failed {
        /// Which toggle.
        toggle: Toggle,
        /// What was attempted, e.g. `apply`.
        action: &'static str,
        /// Why it failed.
        reason: String,
    },
}

impl Change {
    /// The toggle this step is about.
    pub fn toggle(&self) -> Toggle {
        match self {
            Self::Applied { toggle, .. }
            | Self::Restored { toggle, .. }
            | Self::Failed { toggle, .. } => *toggle,
        }
    }

    /// One English line, the way the supervisor log writes it.
    pub fn describe(&self) -> String {
        match self {
            Self::Applied { toggle, previous } => {
                format!("{} hidden (was {})", toggle.id(), previous.describe())
            }
            Self::Restored { toggle, previous } => {
                format!("{} restored (was {})", toggle.id(), previous.describe())
            }
            Self::Failed {
                toggle,
                action,
                reason,
            } => format!("cannot {action} {}: {reason}", toggle.id()),
        }
    }
}

/// Applies and restores toggles, keeping the state file in step.
///
/// The session owns the state once it is opened: every successful apply records the previous value
/// on disk before the registry write, and every successful restore removes its entry.
pub struct Session {
    registry: Box<dyn RegistryAccess + Send>,
    state_path: PathBuf,
    state: State,
    warning: Option<String>,
}

impl fmt::Debug for Session {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Session")
            .field("registry", &self.registry)
            .field("state_path", &self.state_path)
            .field("applied", &self.state.applied())
            .finish()
    }
}

impl Session {
    /// Opens `state_path` and remembers anything that went wrong while reading it.
    pub fn new(
        registry: impl RegistryAccess + Send + 'static,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        let state_path = state_path.into();
        let (state, warning) = state::load(&state_path);

        Self {
            registry: Box::new(registry),
            state_path,
            state,
            warning,
        }
    }

    /// A session over the real `HKCU` and the real desktop.
    pub fn real(state_path: impl Into<PathBuf>) -> Self {
        Self::new(RealRegistry, state_path)
    }

    /// The file this session records into.
    pub fn state_path(&self) -> &std::path::Path {
        &self.state_path
    }

    /// The state as it is in memory.
    pub fn state(&self) -> &State {
        &self.state
    }

    /// A warning from opening the state file, taken once so the caller logs it exactly once.
    pub fn take_warning(&mut self) -> Option<String> {
        self.warning.take()
    }

    /// The toggles recorded as applied.
    pub fn applied(&self) -> ToggleSet {
        self.state.applied()
    }

    /// How many toggles are recorded as applied.
    pub fn applied_count(&self) -> usize {
        self.state.applied_count()
    }

    /// Hides `toggle` and records the previous value first.
    ///
    /// Applying a toggle that is already applied returns the existing record and touches nothing.
    pub fn apply(&mut self, toggle: Toggle) -> Result<Record, ModeError> {
        if let Some(record) = self
            .state
            .toggles
            .get(&toggle)
            .filter(|record| record.applied)
        {
            return Ok(record.clone());
        }

        let descriptor = toggle.descriptor();
        let current = self.read(toggle)?;
        let hidden = plan_write(toggle, &current, true)?;
        let record = Record {
            applied: true,
            previous: current,
            saved_at: UtcDateTime::now_utc().iso8601(),
        };

        // The way back is on disk before the registry changes, which is the whole point of the file.
        self.state.toggles.insert(toggle, record.clone());
        self.save_state()?;

        if let Err(reason) = self
            .registry
            .write(descriptor.key, descriptor.value, &hidden)
        {
            self.state.toggles.remove(&toggle);
            let _ = self.save_state();
            return Err(ModeError::registry("write", descriptor, reason));
        }

        self.registry.notify(toggle, true);

        Ok(record)
    }

    /// Puts `toggle` back the way it was and drops its entry.
    ///
    /// Restoring a toggle that was never applied is a no-op.
    pub fn restore(&mut self, toggle: Toggle) -> Result<(), ModeError> {
        let Some(record) = self
            .state
            .toggles
            .get(&toggle)
            .filter(|record| record.applied)
            .cloned()
        else {
            return Ok(());
        };

        let descriptor = toggle.descriptor();
        let current = self.read(toggle)?;
        let target = plan_restore(&record.previous, &current);

        match &target {
            RawValue::Absent => self
                .registry
                .delete(descriptor.key, descriptor.value)
                .map_err(|reason| ModeError::registry("delete", descriptor, reason))?,
            value => self
                .registry
                .write(descriptor.key, descriptor.value, value)
                .map_err(|reason| ModeError::registry("write", descriptor, reason))?,
        }

        self.registry.notify(toggle, false);
        self.state.toggles.remove(&toggle);
        self.save_state()
    }

    /// Restores every toggle recorded as applied.
    pub fn restore_applied(&mut self) -> Vec<Change> {
        let mut changes = Vec::new();

        for toggle in Toggle::ALL {
            if !self.state.is_applied(toggle) {
                continue;
            }

            let previous = self
                .state
                .previous(toggle)
                .cloned()
                .unwrap_or(RawValue::Absent);

            match self.restore(toggle) {
                Ok(()) => changes.push(Change::Restored { toggle, previous }),
                Err(error) => changes.push(Change::Failed {
                    toggle,
                    action: "restore",
                    reason: error.to_string(),
                }),
            }
        }

        changes
    }

    /// Brings the registry in line with `desired`, applying and restoring only what differs.
    ///
    /// A failure of one toggle does not stop the others: the returned steps say exactly what
    /// happened, and a repeated call retries the failures while leaving the successes alone.
    pub fn sync(&mut self, desired: ToggleSet) -> Vec<Change> {
        let mut changes = Vec::new();

        for toggle in Toggle::ALL {
            let wanted = desired.get(toggle);
            let applied = self.state.is_applied(toggle);

            if wanted && !applied {
                match self.apply(toggle) {
                    Ok(record) => changes.push(Change::Applied {
                        toggle,
                        previous: record.previous,
                    }),
                    Err(error) => changes.push(Change::Failed {
                        toggle,
                        action: "apply",
                        reason: error.to_string(),
                    }),
                }
            } else if !wanted && applied {
                let previous = self
                    .state
                    .previous(toggle)
                    .cloned()
                    .unwrap_or(RawValue::Absent);

                match self.restore(toggle) {
                    Ok(()) => changes.push(Change::Restored { toggle, previous }),
                    Err(error) => changes.push(Change::Failed {
                        toggle,
                        action: "restore",
                        reason: error.to_string(),
                    }),
                }
            }
        }

        changes
    }

    /// Reads a toggle's current value, naming it in the error.
    fn read(&self, toggle: Toggle) -> Result<RawValue, ModeError> {
        let descriptor = toggle.descriptor();
        self.registry
            .read(descriptor.key, descriptor.value)
            .map_err(|reason| ModeError::registry("read", descriptor, reason))
    }

    /// Writes the state file, or removes it when nothing is left.
    fn save_state(&self) -> Result<(), ModeError> {
        state::save(&self.state_path, &self.state)
    }
}

// A shared registry is the seam the supervisor and the tests both need: the supervisor keeps an
// `Arc<MemoryRegistry>` only in tests, while production hands over the unit `RealRegistry`.
impl<R: RegistryAccess + ?Sized> RegistryAccess for std::sync::Arc<R> {
    fn read(&self, key: &str, value: &str) -> Result<RawValue, String> {
        (**self).read(key, value)
    }

    fn write(&self, key: &str, value: &str, raw: &RawValue) -> Result<(), String> {
        (**self).write(key, value, raw)
    }

    fn delete(&self, key: &str, value: &str) -> Result<(), String> {
        (**self).delete(key, value)
    }

    fn notify(&self, toggle: Toggle, hidden: bool) {
        (**self).notify(toggle, hidden);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn selection(toggles: &[Toggle]) -> ToggleSet {
        let mut set = ToggleSet::default();
        for toggle in toggles {
            set.set(*toggle, true);
        }

        set
    }

    fn layout(flags: u8) -> Vec<u8> {
        let mut layout: Vec<u8> = (0..44).collect();
        layout[taskbar::FLAG_INDEX] = flags;
        layout
    }

    fn session() -> (tempfile::TempDir, Arc<MemoryRegistry>, Session) {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry = Arc::new(MemoryRegistry::default());
        let session = Session::new(Arc::clone(&registry), directory.path().join(STATE_FILE));

        (directory, registry, session)
    }

    #[derive(Debug)]
    struct BrokenRegistry;

    impl RegistryAccess for BrokenRegistry {
        fn read(&self, _key: &str, _value: &str) -> Result<RawValue, String> {
            Ok(RawValue::Absent)
        }

        fn write(&self, _key: &str, _value: &str, _raw: &RawValue) -> Result<(), String> {
            Err("the registry is closed".to_owned())
        }

        fn delete(&self, _key: &str, _value: &str) -> Result<(), String> {
            Ok(())
        }

        fn notify(&self, _toggle: Toggle, _hidden: bool) {}
    }

    #[test]
    fn every_toggle_names_the_registry_value_the_contract_promises() {
        let expected = [
            (
                Toggle::Taskbar,
                taskbar::KEY_PATH,
                taskbar::VALUE_NAME,
                ValueKind::Layout,
                None,
                None,
            ),
            (
                Toggle::DesktopIcons,
                ADVANCED_KEY,
                "HideIcons",
                ValueKind::Dword,
                Some(1),
                Some(0),
            ),
            (
                Toggle::SearchBox,
                SEARCH_KEY,
                "SearchboxTaskbarMode",
                ValueKind::Dword,
                Some(0),
                Some(2),
            ),
            (
                Toggle::TaskView,
                ADVANCED_KEY,
                "ShowTaskViewButton",
                ValueKind::Dword,
                Some(0),
                Some(1),
            ),
            (
                Toggle::Widgets,
                ADVANCED_KEY,
                "TaskbarDa",
                ValueKind::Dword,
                Some(0),
                Some(1),
            ),
            (
                Toggle::Copilot,
                ADVANCED_KEY,
                "ShowCopilotButton",
                ValueKind::Dword,
                Some(0),
                Some(1),
            ),
            (
                Toggle::News,
                ADVANCED_KEY,
                "ShellFeedsTaskbarViewMode",
                ValueKind::Dword,
                Some(2),
                Some(0),
            ),
        ];

        assert_eq!(expected.len(), Toggle::ALL.len());
        for (toggle, key, value, kind, hidden, shown) in expected {
            let descriptor = toggle.descriptor();
            assert_eq!(descriptor.key, key, "{}", toggle.id());
            assert_eq!(descriptor.value, value, "{}", toggle.id());
            assert_eq!(descriptor.kind, kind, "{}", toggle.id());
            assert_eq!(descriptor.hidden, hidden, "{}", toggle.id());
            assert_eq!(descriptor.shown, shown, "{}", toggle.id());
        }
    }

    #[test]
    fn the_ids_are_camel_case_and_both_spellings_parse() {
        assert_eq!(
            Toggle::ALL.map(Toggle::id),
            [
                "taskbar",
                "desktopIcons",
                "searchBox",
                "taskView",
                "widgets",
                "copilot",
                "news"
            ]
        );

        for toggle in Toggle::ALL {
            assert_eq!(Toggle::parse(toggle.id()), Some(toggle));
            let snake: String = toggle
                .id()
                .chars()
                .flat_map(|character| {
                    if character.is_ascii_uppercase() {
                        vec!['_', character.to_ascii_lowercase()].into_iter()
                    } else {
                        vec![character].into_iter()
                    }
                })
                .collect();
            assert_eq!(Toggle::parse(&snake), Some(toggle), "{snake}");
        }

        assert_eq!(Toggle::parse("telemetry"), None);
    }

    #[test]
    fn a_dword_toggle_writes_its_constant_and_ignores_what_was_there() {
        for toggle in Toggle::ALL {
            let descriptor = toggle.descriptor();
            if descriptor.kind != ValueKind::Dword {
                continue;
            }

            for current in [RawValue::Absent, RawValue::Dword { value: 99 }] {
                assert_eq!(
                    plan_write(toggle, &current, true).expect("hidden plan"),
                    RawValue::Dword {
                        value: descriptor.hidden.expect("a hidden value")
                    },
                    "{}",
                    toggle.id()
                );
                assert_eq!(
                    plan_write(toggle, &current, false).expect("shown plan"),
                    RawValue::Dword {
                        value: descriptor.shown.expect("a shown value")
                    },
                    "{}",
                    toggle.id()
                );
            }
        }
    }

    #[test]
    fn the_taskbar_plan_keeps_every_byte_but_the_flag() {
        let current = RawValue::Binary {
            value: layout(0b0000_0110),
        };

        let hidden = plan_write(Toggle::Taskbar, &current, true).expect("hide");
        let shown = plan_write(Toggle::Taskbar, &current, false).expect("show");

        let RawValue::Binary { value: hidden } = hidden else {
            panic!("the taskbar stays binary");
        };
        let RawValue::Binary { value: shown } = shown else {
            panic!("the taskbar stays binary");
        };
        let original = layout(0b0000_0110);

        assert_eq!(hidden[taskbar::FLAG_INDEX], 0b0000_0111);
        assert_eq!(
            hidden[..taskbar::FLAG_INDEX],
            original[..taskbar::FLAG_INDEX]
        );
        assert_eq!(
            hidden[taskbar::FLAG_INDEX + 1..],
            original[taskbar::FLAG_INDEX + 1..]
        );
        assert_eq!(shown[taskbar::FLAG_INDEX], 0b0000_0110);
        assert!(!taskbar::is_hidden(&shown));
    }

    #[test]
    fn the_taskbar_plan_refuses_a_layout_it_cannot_transform() {
        for current in [RawValue::Absent, RawValue::Binary { value: vec![0; 4] }] {
            assert!(matches!(
                plan_write(Toggle::Taskbar, &current, true),
                Err(ModeError::TaskbarLayoutAbsent { .. })
                    | Err(ModeError::TaskbarLayoutTooShort { .. })
            ));
        }

        assert!(matches!(
            plan_write(Toggle::Taskbar, &RawValue::Dword { value: 1 }, true),
            Err(ModeError::UnexpectedValue { .. })
        ));
    }

    #[test]
    fn restoring_an_absent_value_means_delete() {
        assert_eq!(
            plan_restore(&RawValue::Absent, &RawValue::Dword { value: 1 }),
            RawValue::Absent
        );
        assert_eq!(
            plan_restore(&RawValue::Absent, &RawValue::Absent),
            RawValue::Absent
        );
    }

    #[test]
    fn restoring_a_dword_writes_the_original_number() {
        assert_eq!(
            plan_restore(&RawValue::Dword { value: 2 }, &RawValue::Dword { value: 0 }),
            RawValue::Dword { value: 2 }
        );
    }

    #[test]
    fn restoring_the_taskbar_keeps_the_current_layout() {
        let previous = RawValue::Binary {
            value: layout(0b0000_0010),
        };
        let current = RawValue::Binary {
            value: layout(0b0000_0011),
        };

        let RawValue::Binary { value: restored } = plan_restore(&previous, &current) else {
            panic!("the taskbar stays binary");
        };

        assert_eq!(restored[taskbar::FLAG_INDEX], 0b0000_0010);
        assert_eq!(
            restored[1], 1,
            "a byte outside the flag survives the restore"
        );
    }

    #[test]
    fn the_settings_json_is_flat_and_defaults_to_everything_off() {
        let settings = WindowsModeSettings::default();
        let value: Value = serde_json::from_str(&serde_json::to_string(&settings).expect("render"))
            .expect("parse");

        assert_eq!(value["enabled"], json!(false));
        for toggle in Toggle::ALL {
            assert_eq!(value[toggle.id()], json!(false), "{}", toggle.id());
        }
        assert_eq!(value.as_object().expect("an object").len(), 8);

        let parsed: WindowsModeSettings = serde_json::from_str(
            r#"{"enabled":true,"desktopIcons":true,"searchBox":true,"news":true}"#,
        )
        .expect("a partial object must load");

        assert!(parsed.enabled);
        assert!(parsed.toggles.desktop_icons);
        assert!(parsed.toggles.search_box);
        assert!(parsed.toggles.news);
        assert!(!parsed.toggles.taskbar);
        assert_eq!(parsed.desired(), parsed.toggles);
    }

    #[test]
    fn an_off_mode_desires_nothing_however_the_toggles_are_set() {
        let settings = WindowsModeSettings {
            enabled: false,
            toggles: selection(&[Toggle::DesktopIcons, Toggle::SearchBox]),
        };

        assert!(!settings.desired().any());

        let settings = WindowsModeSettings {
            enabled: true,
            ..settings
        };
        assert_eq!(settings.desired().count(), 2);
    }

    #[test]
    fn update_parsing_accepts_the_nested_and_the_flat_shape() {
        let nested = Update::parse(Some(&json!({
            "enabled": true,
            "toggles": { "desktopIcons": true, "searchBox": false }
        })))
        .expect("nested");

        assert_eq!(nested.enabled(), Some(true));
        assert_eq!(nested.toggle(Toggle::DesktopIcons), Some(true));
        assert_eq!(nested.toggle(Toggle::SearchBox), Some(false));
        assert_eq!(nested.toggle(Toggle::News), None);
        assert!(nested.touches_toggles());

        let flat = Update::parse(Some(&json!({
            "taskbar": true,
            "task_view": false
        })))
        .expect("flat");

        assert_eq!(flat.enabled(), None);
        assert_eq!(flat.toggle(Toggle::Taskbar), Some(true));
        assert_eq!(flat.toggle(Toggle::TaskView), Some(false));
    }

    #[test]
    fn an_empty_update_changes_nothing() {
        let update = Update::parse(None).expect("no params");

        assert_eq!(update, Update::default());
        assert!(!update.touches_toggles());
        assert_eq!(update.enabled(), None);
    }

    #[test]
    fn unknown_keys_and_toggles_are_refused_by_name() {
        assert_eq!(
            Update::parse(Some(&json!({ "telemetry": true }))),
            Err(UpdateError::UnknownKey("telemetry".to_owned()))
        );
        assert_eq!(
            Update::parse(Some(&json!({ "toggles": { "telemetry": true } }))),
            Err(UpdateError::UnknownToggle("telemetry".to_owned()))
        );
        assert_eq!(
            Update::parse(Some(&json!({ "enabled": "yes" }))),
            Err(UpdateError::NotABoolean {
                key: "enabled".to_owned()
            })
        );
        assert_eq!(
            Update::parse(Some(&json!({ "toggles": 7 }))),
            Err(UpdateError::TogglesNotAnObject)
        );
        assert_eq!(
            Update::parse(Some(&json!([true]))),
            Err(UpdateError::NotAnObject)
        );
    }

    #[test]
    fn applying_a_subset_writes_only_that_subset_and_records_the_previous_values() {
        let (directory, registry, mut session) = session();
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });

        let changes = session.sync(selection(&[Toggle::DesktopIcons, Toggle::SearchBox]));

        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|change| matches!(change, Change::Applied { .. }))
        );
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 1 }
        );
        assert_eq!(
            registry.get(Toggle::SearchBox),
            RawValue::Dword { value: 0 }
        );
        assert_eq!(
            registry.get(Toggle::TaskView),
            RawValue::Absent,
            "a toggle outside the selection is never touched"
        );
        assert_eq!(registry.writes(), 2);
        assert_eq!(
            registry.notifications(),
            vec![(Toggle::DesktopIcons, true), (Toggle::SearchBox, true)]
        );

        let state = session.state();
        assert_eq!(state.applied_count(), 2);
        assert_eq!(
            state.previous(Toggle::DesktopIcons),
            Some(&RawValue::Dword { value: 0 })
        );
        assert_eq!(
            state.previous(Toggle::SearchBox),
            Some(&RawValue::Absent),
            "an absent value is remembered as absent, not as zero"
        );
        assert!(
            directory.path().join(STATE_FILE).exists(),
            "the way back is on disk"
        );
    }

    #[test]
    fn applying_the_same_selection_twice_writes_nothing_the_second_time() {
        let (_directory, registry, mut session) = session();
        let desired = selection(&[Toggle::DesktopIcons, Toggle::SearchBox]);

        session.sync(desired);
        let writes = registry.writes();
        let notifications = registry.notifications().len();

        let changes = session.sync(desired);

        assert!(changes.is_empty(), "{changes:?}");
        assert_eq!(registry.writes(), writes, "no value was written twice");
        assert_eq!(registry.notifications().len(), notifications);
    }

    #[test]
    fn disabling_restores_only_the_toggles_that_were_applied() {
        let (_directory, registry, mut session) = session();
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });
        registry.set(Toggle::TaskView, RawValue::Dword { value: 1 });

        session.sync(selection(&[Toggle::DesktopIcons, Toggle::SearchBox]));
        let value_before = registry.get(Toggle::TaskView);

        let changes = session.sync(ToggleSet::default());

        assert_eq!(changes.len(), 2);
        assert!(
            changes
                .iter()
                .all(|change| matches!(change, Change::Restored { .. }))
        );
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 0 }
        );
        assert_eq!(
            registry.get(Toggle::SearchBox),
            RawValue::Absent,
            "a value that did not exist is deleted again"
        );
        assert_eq!(registry.get(Toggle::TaskView), value_before);
        assert_eq!(registry.deletes(), 1);
        assert_eq!(session.applied_count(), 0);
    }

    #[test]
    fn changing_the_selection_restores_what_left_and_applies_what_arrived() {
        let (_directory, registry, mut session) = session();
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });

        session.sync(selection(&[Toggle::DesktopIcons]));
        session.sync(selection(&[Toggle::SearchBox]));

        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 0 }
        );
        assert_eq!(
            registry.get(Toggle::SearchBox),
            RawValue::Dword { value: 0 }
        );
        assert_eq!(session.applied(), selection(&[Toggle::SearchBox]));
    }

    #[test]
    fn restoring_everything_removes_the_state_file() {
        let (directory, _registry, mut session) = session();
        let path = directory.path().join(STATE_FILE);

        session.sync(selection(&[Toggle::Widgets, Toggle::Copilot]));
        assert!(path.exists());

        session.restore_applied();

        assert!(
            !path.exists(),
            "a fully restored mode leaves nothing behind"
        );
        assert_eq!(session.applied_count(), 0);
    }

    #[test]
    fn the_taskbar_toggle_moves_only_its_bit_and_puts_the_layout_back() {
        let (_directory, registry, mut session) = session();
        let original = layout(0b0000_0010);
        registry.set(
            Toggle::Taskbar,
            RawValue::Binary {
                value: original.clone(),
            },
        );

        session.sync(selection(&[Toggle::Taskbar]));

        let RawValue::Binary { value: hidden } = registry.get(Toggle::Taskbar) else {
            panic!("the taskbar stays binary");
        };
        assert!(taskbar::is_hidden(&hidden));
        assert_eq!(hidden[taskbar::FLAG_INDEX], 0b0000_0011);
        assert_eq!(
            hidden[1..taskbar::FLAG_INDEX],
            original[1..taskbar::FLAG_INDEX]
        );

        session.sync(ToggleSet::default());

        assert_eq!(
            registry.get(Toggle::Taskbar),
            RawValue::Binary { value: original }
        );
    }

    #[test]
    fn a_corrupted_state_file_opens_empty_and_warns_instead_of_guessing() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(STATE_FILE);
        std::fs::write(&path, "{ not json").expect("write");
        let registry = Arc::new(MemoryRegistry::default());
        let mut session = Session::new(Arc::clone(&registry), &path);

        assert_eq!(session.applied_count(), 0);
        let warning = session.take_warning().expect("the corruption is reported");
        assert!(
            warning.contains("not valid windows-mode state"),
            "{warning}"
        );
        assert_eq!(session.take_warning(), None, "the warning is said once");

        session.sync(selection(&[Toggle::News]));

        assert_eq!(registry.get(Toggle::News), RawValue::Dword { value: 2 });
        assert_eq!(session.applied_count(), 1);
    }

    #[test]
    fn a_failed_write_removes_the_record_it_had_just_saved() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(STATE_FILE);
        let mut session = Session::new(BrokenRegistry, &path);

        let error = session
            .apply(Toggle::DesktopIcons)
            .expect_err("the write fails");

        assert!(
            error.to_string().contains("the registry is closed"),
            "{error}"
        );
        assert_eq!(session.applied_count(), 0);
        assert!(!path.exists(), "no phantom entry is left behind");
    }

    #[test]
    fn a_change_describes_itself_for_the_log() {
        assert_eq!(
            Change::Applied {
                toggle: Toggle::DesktopIcons,
                previous: RawValue::Dword { value: 0 }
            }
            .describe(),
            "desktopIcons hidden (was 0)"
        );
        assert_eq!(
            Change::Restored {
                toggle: Toggle::SearchBox,
                previous: RawValue::Absent
            }
            .describe(),
            "searchBox restored (was absent)"
        );
        assert_eq!(
            Change::Failed {
                toggle: Toggle::Taskbar,
                action: "apply",
                reason: "no layout".to_owned()
            }
            .describe(),
            "cannot apply taskbar: no layout"
        );
        assert_eq!(
            Change::Applied {
                toggle: Toggle::News,
                previous: RawValue::Binary { value: vec![0; 3] }
            }
            .toggle(),
            Toggle::News
        );
    }
}
