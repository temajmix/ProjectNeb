//! The supervisor half of "Замена Windows": the config object, the state file and the IPC answers.
//!
//! The registry work lives in [`neb_core::windows_mode`]; this module joins it to the two pieces of
//! process state the supervisor owns: `config.json` (what the user selected) and the supervisor log.
//! Both are written before the registry is touched, so a crash at any point leaves a file that says
//! what still has to be undone.

use std::path::{Path, PathBuf};

use neb_core::config::ConfigStore;
use neb_core::windows_mode::{Change, Session, Toggle, Update, WindowsModeSettings};
use serde_json::{Value, json};

use crate::supervisor::Reporter;

/// What the supervisor knows about the mode: the selection, the state file and the log.
pub struct Mode {
    config: Option<PathBuf>,
    session: Session,
    settings: WindowsModeSettings,
    reporter: Option<Reporter>,
}

impl std::fmt::Debug for Mode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Mode")
            .field("config", &self.config)
            .field("session", &self.session)
            .field("settings", &self.settings)
            .finish()
    }
}

impl Mode {
    /// Opens the mode for the real data directory.
    ///
    /// The config is read once; a file that cannot be read falls back to "nothing selected", which
    /// is the safe direction: it never hides chrome the user did not ask for.
    pub fn open(root: &Path, reporter: Reporter) -> Self {
        let config = root.join("config.json");
        let settings = ConfigStore::new(&config).load().windows_mode;
        let session = Session::real(root.join(neb_core::windows_mode::STATE_FILE));

        Self {
            config: Some(config),
            session,
            settings,
            reporter: Some(reporter),
        }
    }

    /// A mode with no config and an in-memory registry, used by tests and by a run without a data
    /// directory. It never touches the real machine.
    pub fn detached() -> Self {
        Self {
            config: None,
            session: Session::new(
                neb_core::windows_mode::MemoryRegistry::default(),
                std::env::temp_dir().join("ProjectNeb-never-written-windows-mode.json"),
            ),
            settings: WindowsModeSettings::default(),
            reporter: None,
        }
    }

    /// A mode assembled from its parts, for tests that want a fake registry and a temp config.
    #[cfg(test)]
    pub fn with_parts(
        session: Session,
        settings: WindowsModeSettings,
        config: Option<PathBuf>,
    ) -> Self {
        Self {
            config,
            session,
            settings,
            reporter: None,
        }
    }

    /// The selection the config holds.
    #[cfg(test)]
    pub fn settings(&self) -> WindowsModeSettings {
        self.settings
    }

    /// How many toggles are recorded as applied right now.
    #[cfg(test)]
    pub fn applied_count(&self) -> usize {
        self.session.applied_count()
    }

    /// Whether anything is applied, which is what the tray action tests.
    pub fn has_applied(&self) -> bool {
        self.session.applied_count() > 0
    }

    /// The document `windows.getState` returns.
    pub fn state_doc(&self) -> Value {
        json!({
            "enabled": self.settings.enabled,
            "toggles": self.settings.toggles,
            "applied": self.session.applied(),
        })
    }

    /// Merges `update` into the selection, saves the config and brings the registry in line.
    ///
    /// The order matters: the config is written first, so a crash after the registry change cannot
    /// leave a mode that is on with a file that says it is off. When the caller does not name
    /// `enabled`, selecting anything turns the mode on and deselecting everything turns it off,
    /// which is what a settings screen with one master switch and seven boxes means.
    pub fn apply(&mut self, update: &Update) -> Result<Vec<Change>, String> {
        let mut settings = self.settings;

        if let Some(enabled) = update.enabled() {
            settings.enabled = enabled;
        }
        for toggle in Toggle::ALL {
            if let Some(selected) = update.toggle(toggle) {
                settings.toggles.set(toggle, selected);
            }
        }
        if update.enabled().is_none() && update.touches_toggles() {
            settings.enabled = settings.toggles.any();
        }

        self.persist(&settings)?;
        self.settings = settings;

        let changes = self.session.sync(settings.desired());
        self.log_changes(&changes);

        Ok(changes)
    }

    /// Sets every toggle back to "shown", saves the config and restores what was applied.
    pub fn restore_all(&mut self) -> Result<Vec<Change>, String> {
        let settings = WindowsModeSettings::default();

        self.persist(&settings)?;
        self.settings = settings;

        let changes = self.session.restore_applied();
        self.log_changes(&changes);

        Ok(changes)
    }

    /// Runs on supervisor start.
    ///
    /// The only correctness rule needed here is the one the state file exists for: when the config
    /// says the mode is off, anything still recorded as hidden is put back. A mode that is on is left
    /// alone, and a corrupt state file is reported rather than guessed at.
    ///
    /// Before either branch the records are reconciled with the live registry. After a crash the OS
    /// may have put the taskbar back while the file still says `applied`, and restoring that stale
    /// record would overwrite a value the user (or Windows) has already chosen. Reconciling drops
    /// such a record without writing anything; only what is still hidden stays recorded.
    pub fn repair(&mut self) -> Vec<String> {
        let mut notes = Vec::new();

        if let Some(warning) = self.session.take_warning() {
            notes.push(warning);
        }

        for toggle in self.session.reconcile() {
            notes.push(format!(
                "{} was changed outside the supervisor, its record was dropped and the value left as it is",
                toggle.id()
            ));
        }

        if self.settings.enabled {
            // Converge the registry on the selection: this re-applies a toggle a crash interrupted
            // and is a no-op for everything that is already recorded as hidden.
            for change in self.session.sync(self.settings.desired()) {
                notes.push(change.describe());
            }
        } else if self.session.applied_count() > 0 {
            for change in self.session.restore_applied() {
                notes.push(change.describe());
            }
        }

        notes
    }

    /// Writes the selection into `config.json`, keeping everything else the file holds.
    fn persist(&self, settings: &WindowsModeSettings) -> Result<(), String> {
        let Some(path) = &self.config else {
            return Ok(());
        };

        let store = ConfigStore::new(path);
        let mut config = store
            .load_checked()
            .map_err(|error| format!("cannot read '{}': {error}", path.display()))?;
        config.windows_mode = *settings;
        store
            .save(&config)
            .map_err(|error| format!("cannot write '{}': {error}", path.display()))
    }

    /// Writes every step to the supervisor log.
    fn log_changes(&self, changes: &[Change]) {
        for change in changes {
            self.log(&format!("windows-mode: {}", change.describe()));
        }
    }

    /// One line, when this mode has a log at all.
    fn log(&self, message: &str) {
        if let Some(reporter) = &self.reporter {
            reporter.info(message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use neb_core::windows_mode::{MemoryRegistry, RawValue};
    use std::sync::Arc;

    fn as_file(settings: &WindowsModeSettings) -> Value {
        serde_json::to_value(settings).expect("render")
    }

    fn mode() -> (tempfile::TempDir, Arc<MemoryRegistry>, Mode) {
        let directory = tempfile::tempdir().expect("temp dir");
        let registry = Arc::new(MemoryRegistry::default());
        let session = Session::new(
            Arc::clone(&registry),
            directory.path().join(neb_core::windows_mode::STATE_FILE),
        );
        let mode = Mode::with_parts(
            session,
            WindowsModeSettings::default(),
            Some(directory.path().join("config.json")),
        );

        (directory, registry, mode)
    }

    fn update(value: Value) -> Update {
        Update::parse(Some(&value)).expect("valid update")
    }

    #[test]
    fn a_selection_applies_and_is_written_to_the_config() {
        let (directory, registry, mut mode) = mode();

        mode.apply(&update(json!({
            "toggles": { "desktopIcons": true, "searchBox": true }
        })))
        .expect("apply");

        assert!(
            mode.settings().enabled,
            "selecting something turns the mode on"
        );
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 1 }
        );
        assert_eq!(
            registry.get(Toggle::SearchBox),
            RawValue::Dword { value: 0 }
        );
        assert_eq!(mode.applied_count(), 2);

        let saved: Value = serde_json::from_slice(
            &std::fs::read(directory.path().join("config.json")).expect("config written"),
        )
        .expect("valid json");
        assert_eq!(
            saved["windowsMode"],
            as_file(&mode.settings()),
            "the config carries the selection"
        );
    }

    #[test]
    fn deselecting_everything_turns_the_mode_off_and_restores_it() {
        let (_directory, registry, mut mode) = mode();
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });

        mode.apply(&update(json!({ "toggles": { "desktopIcons": true } })))
            .expect("apply");
        mode.apply(&update(json!({ "toggles": { "desktopIcons": false } })))
            .expect("deselect");

        assert!(!mode.settings().enabled);
        assert_eq!(mode.applied_count(), 0);
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 0 }
        );
    }

    #[test]
    fn the_master_switch_alone_leaves_the_selection_remembered() {
        let (_directory, _registry, mut mode) = mode();

        mode.apply(&update(json!({
            "enabled": true,
            "toggles": { "widgets": true }
        })))
        .expect("enable");
        mode.apply(&update(json!({ "enabled": false })))
            .expect("disable");

        assert!(!mode.settings().enabled);
        assert!(mode.settings().toggles.widgets, "the box stays ticked");
        assert_eq!(mode.applied_count(), 0);
    }

    #[test]
    fn restore_all_clears_the_selection_and_the_registry() {
        let (_directory, registry, mut mode) = mode();
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });
        mode.apply(&update(json!({
            "toggles": { "desktopIcons": true, "taskView": true }
        })))
        .expect("apply");

        mode.restore_all().expect("restore");

        assert_eq!(mode.settings(), WindowsModeSettings::default());
        assert!(!mode.settings().enabled);
        assert_eq!(mode.applied_count(), 0);
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 0 }
        );
        assert_eq!(registry.get(Toggle::TaskView), RawValue::Absent);
    }

    #[test]
    fn the_repair_restores_what_an_off_config_left_behind() {
        let (directory, registry, mut mode) = mode();
        registry.set(Toggle::News, RawValue::Dword { value: 0 });

        mode.apply(&update(json!({ "toggles": { "news": true } })))
            .expect("apply");

        // The user turned the mode off in an editor, or a crash beat the save.
        let mut off = mode.settings();
        off.enabled = false;
        let mut repaired = Mode::with_parts(
            Session::new(
                Arc::clone(&registry),
                directory.path().join(neb_core::windows_mode::STATE_FILE),
            ),
            off,
            None,
        );

        let notes = repaired.repair();

        assert!(
            notes.iter().any(|note| note.contains("news restored")),
            "{notes:?}"
        );
        assert_eq!(repaired.applied_count(), 0);
        assert_eq!(registry.get(Toggle::News), RawValue::Dword { value: 0 });
    }

    #[test]
    fn the_repair_leaves_an_enabled_mode_alone() {
        let (directory, registry, mut mode) = mode();
        mode.apply(&update(json!({ "toggles": { "copilot": true } })))
            .expect("apply");

        let mut enabled = Mode::with_parts(
            Session::new(
                Arc::clone(&registry),
                directory.path().join(neb_core::windows_mode::STATE_FILE),
            ),
            mode.settings(),
            None,
        );

        assert!(enabled.repair().is_empty());
        assert_eq!(enabled.applied_count(), 1);
        assert_eq!(registry.get(Toggle::Copilot), RawValue::Dword { value: 0 });
    }

    #[test]
    fn the_repair_applies_a_selection_a_crash_left_unrecorded() {
        let (directory, registry, _mode) = mode();
        let settings = WindowsModeSettings {
            enabled: true,
            toggles: neb_core::windows_mode::ToggleSet {
                widgets: true,
                ..neb_core::windows_mode::ToggleSet::default()
            },
        };
        let mut repaired = Mode::with_parts(
            Session::new(
                Arc::clone(&registry),
                directory.path().join(neb_core::windows_mode::STATE_FILE),
            ),
            settings,
            None,
        );

        let notes = repaired.repair();

        assert!(
            notes.iter().any(|note| note.contains("widgets hidden")),
            "{notes:?}"
        );
        assert_eq!(repaired.applied_count(), 1);
        assert_eq!(registry.get(Toggle::Widgets), RawValue::Dword { value: 0 });
    }

    #[test]
    fn the_repair_corrects_a_state_file_the_registry_has_already_restored() {
        let (directory, registry, mut mode) = mode();
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });
        mode.apply(&update(json!({ "toggles": { "desktopIcons": true } })))
            .expect("apply");
        let writes = registry.writes();

        // The user or Explorer showed the icons again after a crash; the file still says applied.
        registry.set(Toggle::DesktopIcons, RawValue::Dword { value: 0 });

        let mut off = mode.settings();
        off.enabled = false;
        let mut repaired = Mode::with_parts(
            Session::new(
                Arc::clone(&registry),
                directory.path().join(neb_core::windows_mode::STATE_FILE),
            ),
            off,
            None,
        );

        let notes = repaired.repair();

        assert!(
            notes.iter().any(
                |note| note.contains("desktopIcons") && note.contains("outside the supervisor")
            ),
            "{notes:?}"
        );
        assert_eq!(repaired.applied_count(), 0, "the record is gone");
        assert_eq!(
            registry.writes(),
            writes,
            "a value the user already restored is never written again"
        );
        assert_eq!(
            registry.get(Toggle::DesktopIcons),
            RawValue::Dword { value: 0 }
        );
        assert!(
            !directory
                .path()
                .join(neb_core::windows_mode::STATE_FILE)
                .exists(),
            "the state file now matches reality"
        );
    }

    #[test]
    fn the_repair_leaves_a_state_file_that_still_matches_the_registry_alone() {
        let (directory, registry, mut mode) = mode();
        mode.apply(&update(json!({ "toggles": { "taskView": true } })))
            .expect("apply");
        let writes = registry.writes();

        let mut repaired = Mode::with_parts(
            Session::new(
                Arc::clone(&registry),
                directory.path().join(neb_core::windows_mode::STATE_FILE),
            ),
            mode.settings(),
            None,
        );

        let notes = repaired.repair();

        assert!(notes.is_empty(), "{notes:?}");
        assert_eq!(repaired.applied_count(), 1);
        assert_eq!(registry.writes(), writes, "nothing was written");
        assert_eq!(registry.get(Toggle::TaskView), RawValue::Dword { value: 0 });
        assert!(
            directory
                .path()
                .join(neb_core::windows_mode::STATE_FILE)
                .exists(),
            "a matching state file is untouched"
        );
    }

    #[test]
    fn the_state_document_is_the_shape_the_ipc_contract_promises() {
        let (_directory, _registry, mut mode) = mode();
        mode.apply(&update(json!({ "toggles": { "taskView": true } })))
            .expect("apply");

        let document = mode.state_doc();

        assert_eq!(document["enabled"], json!(true));
        assert_eq!(document["toggles"]["taskView"], json!(true));
        assert_eq!(document["toggles"]["desktopIcons"], json!(false));
        assert_eq!(document["applied"]["taskView"], json!(true));
        assert_eq!(document["applied"]["taskbar"], json!(false));
        assert_eq!(document.as_object().expect("an object").len(), 3);
    }

    #[test]
    fn a_detached_mode_never_touches_the_machine() {
        let mode = Mode::detached();

        assert!(!mode.settings().enabled);
        assert_eq!(mode.state_doc()["applied"]["desktopIcons"], json!(false));
        assert!(mode.applied_count() == 0);
    }
}
