//! The elevated half: one plan file in, one result file out.
//!
//! Writing `ModsSource` and `HKLM\SOFTWARE\Windhawk\Engine\Mods` needs administrator rights, so an
//! ordinary manager asks the shell for `runas` and starts this same executable with
//! `--helper --plan <file>`. `ShellExecuteW` gives the parent no stdout, hence the result file.
//!
//! The helper never guesses the enabled state. It writes `Disabled=0`, reads the value back and
//! reports `enabled` only when that read proves it. Any failure along that path leaves
//! `enabled: "unknown"` and `needsWindhawkUi: true`, which is what makes the UI offer Windhawk
//! instead of pretending the mod is on.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::installed;
use crate::paths;
use crate::plan::{Action, ApplyPlan, ApplyResult, PlanError};
use crate::registry::ModRegistry;

/// Why the helper could not finish a plan.
#[derive(Debug, thiserror::Error)]
pub enum HelperError {
    /// The plan or its result could not be read or written.
    #[error("cannot {action} '{}': {source}", path.display())]
    Io {
        /// Verbed operation.
        action: &'static str,
        /// File the operation targeted.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The plan is malformed or unsafe.
    #[error(transparent)]
    Plan(#[from] PlanError),

    /// The plan file was not the JSON the manager writes.
    #[error("cannot parse the plan '{}': {source}", path.display())]
    Json {
        /// Plan file.
        path: PathBuf,
        /// The parser's message.
        #[source]
        source: serde_json::Error,
    },

    /// The result file could not be serialised, which is a bug rather than a machine failure.
    #[error("cannot render the result: {0}")]
    Render(String),
}

/// Reads `plan_path`, applies it and writes the result to `plan.result`.
///
/// # Errors
///
/// [`HelperError`] when the plan cannot be read or the result cannot be written. A failure to
/// *apply* the plan is written into the result document instead, so the manager always gets an
/// answer it can show.
pub fn run_plan_file(
    plan_path: &Path,
    registry: &dyn ModRegistry,
) -> Result<ApplyResult, HelperError> {
    let bytes = fs::read(plan_path).map_err(|source| HelperError::Io {
        action: "read",
        path: plan_path.to_path_buf(),
        source,
    })?;
    let plan: ApplyPlan = serde_json::from_slice(&bytes).map_err(|source| HelperError::Json {
        path: plan_path.to_path_buf(),
        source,
    })?;

    let result = match plan.validate() {
        Ok(()) => apply(&plan, registry),
        Err(error) => ApplyResult::failure(&plan, error.to_string()),
    };

    write_result(Path::new(&plan.result), &result)?;
    Ok(result)
}

/// Applies one validated plan.
///
/// The returned document is always written by [`run_plan_file`]. `ok` is false only when the
/// requested end state was not reached; an unproven enabled state is a success with
/// `enabled: "unknown"`.
pub fn apply(plan: &ApplyPlan, registry: &dyn ModRegistry) -> ApplyResult {
    match plan.action {
        Action::Install => install(plan, registry),
        Action::Remove => remove(plan, registry),
    }
}

fn install(plan: &ApplyPlan, registry: &dyn ModRegistry) -> ApplyResult {
    let Ok(target) = plan.target() else {
        return ApplyResult::failure(plan, "the plan has no usable target");
    };
    let Some(source) = plan.source.as_deref() else {
        return ApplyResult::failure(plan, "the plan has no source");
    };

    let bytes = match fs::read(source) {
        Ok(bytes) => bytes,
        Err(error) => {
            return ApplyResult::failure(plan, format!("cannot read '{source}': {error}"));
        }
    };

    if let Some(parent) = target.parent() {
        if let Err(error) = fs::create_dir_all(parent) {
            return ApplyResult::failure(
                plan,
                format!("cannot create '{}': {error}", parent.display()),
            );
        }
    }

    let temporary = target.with_extension("tmp");
    if let Err(error) = fs::write(&temporary, &bytes) {
        return ApplyResult::failure(
            plan,
            format!("cannot write '{}': {error}", temporary.display()),
        );
    }

    // Windows refuses to rename over an existing file, and replacing an installed mod is the
    // normal update path.
    let _ = fs::remove_file(&target);
    if let Err(error) = fs::rename(&temporary, &target) {
        let _ = fs::remove_file(&temporary);
        return ApplyResult::failure(
            plan,
            format!(
                "cannot move the source into '{}': {error}",
                target.display()
            ),
        );
    }

    let installed = target.is_file()
        && fs::metadata(&target).is_ok_and(|meta| meta.len() == bytes.len() as u64);
    if !installed {
        return ApplyResult::failure(
            plan,
            format!("'{}' was not written completely", target.display()),
        );
    }

    let mut result = ApplyResult {
        ok: true,
        action: Action::Install,
        id: plan.id.clone(),
        installed: true,
        enabled: "unknown".to_owned(),
        needs_windhawk_ui: true,
        source_path: Some(target.display().to_string()),
        config_key: None,
        error: None,
        note: None,
    };

    if let Some(disabled) = plan.disabled {
        match registry.set_disabled(&plan.id, disabled) {
            Ok(()) => match registry.read_disabled(&plan.id) {
                Ok(Some(stored)) if stored == disabled => {
                    result.enabled = if disabled { "disabled" } else { "enabled" }.to_owned();
                    result.needs_windhawk_ui = false;
                    result.config_key = Some(registry.key_path(&plan.id));
                }
                Ok(other) => {
                    result.note = Some(format!(
                        "the enabled state was written but read back as {other:?}; Windhawk must confirm it"
                    ));
                }
                Err(error) => {
                    result.note = Some(format!(
                        "the enabled state was written but could not be verified: {error}"
                    ));
                }
            },
            Err(error) => {
                result.note = Some(format!(
                    "the source is installed but the enabled state could not be written: {error}"
                ));
            }
        }
    } else {
        result.note = Some("the plan asked not to change the enabled state".to_owned());
    }

    result
}

fn remove(plan: &ApplyPlan, registry: &dyn ModRegistry) -> ApplyResult {
    let Ok(target) = plan.target() else {
        return ApplyResult::failure(plan, "the plan has no usable target");
    };

    if let Err(error) = fs::remove_file(&target) {
        if error.kind() != io::ErrorKind::NotFound {
            return ApplyResult::failure(
                plan,
                format!("cannot delete '{}': {error}", target.display()),
            );
        }
    }

    let mut notes = Vec::new();

    if let Err(error) = registry.remove(&plan.id) {
        notes.push(format!("the configuration key was not removed: {error}"));
    }

    if let Err(error) = installed::remove_compiled(Path::new(&plan.data_dir), &plan.id) {
        notes.push(format!("the compiled files were not all removed: {error}"));
    }

    ApplyResult {
        ok: true,
        action: Action::Remove,
        id: plan.id.clone(),
        installed: target.is_file(),
        enabled: "unknown".to_owned(),
        needs_windhawk_ui: false,
        source_path: Some(target.display().to_string()),
        config_key: None,
        error: None,
        note: (!notes.is_empty()).then(|| notes.join("; ")),
    }
}

fn write_result(path: &Path, result: &ApplyResult) -> Result<(), HelperError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| HelperError::Io {
            action: "create",
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let bytes = serde_json::to_vec_pretty(result)
        .map_err(|error| HelperError::Render(error.to_string()))?;
    let temporary = path.with_extension("json.tmp");

    fs::write(&temporary, bytes).map_err(|source| HelperError::Io {
        action: "write",
        path: temporary.clone(),
        source,
    })?;
    let _ = fs::remove_file(path);
    fs::rename(&temporary, path).map_err(|source| HelperError::Io {
        action: "move",
        path: path.to_path_buf(),
        source,
    })
}

/// Reads a result the helper wrote.
///
/// # Errors
///
/// [`HelperError::Io`] when it is missing or unreadable, [`HelperError::Json`] when it is not the
/// documented shape.
pub fn read_result(path: &Path) -> Result<ApplyResult, HelperError> {
    let bytes = fs::read(path).map_err(|source| HelperError::Io {
        action: "read",
        path: path.to_path_buf(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| HelperError::Json {
        path: path.to_path_buf(),
        source,
    })
}

/// The data directory a plan names, or `%ProgramData%\Windhawk` when it names none.
pub fn plan_data_dir(plan: &ApplyPlan) -> PathBuf {
    paths::windhawk_data_dir(Some(&plan.data_dir), None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registry::MemoryRegistry;
    use tempfile::tempdir;

    fn plan(directory: &Path, enable: bool) -> ApplyPlan {
        let data = directory.join("Windhawk");
        let source = directory.join("download.wh.cpp");
        fs::write(&source, b"//! ```c\n//! id: accent-color-sync\n//! ```\n").expect("download");
        ApplyPlan::install(
            "accent-color-sync",
            &data,
            &source,
            &directory.join("result.json"),
            enable,
        )
    }

    #[test]
    fn an_install_writes_the_source_and_proves_the_enabled_state() {
        let directory = tempdir().expect("temp dir");
        let plan = plan(directory.path(), true);
        let registry = MemoryRegistry::new();

        let result = apply(&plan, &registry);

        assert!(result.ok, "{result:?}");
        assert!(result.installed);
        assert_eq!(result.enabled, "enabled");
        assert!(!result.needs_windhawk_ui);
        assert_eq!(registry.get("accent-color-sync"), Some(false));
        assert!(Path::new(result.source_path.as_deref().expect("path")).is_file());
    }

    #[test]
    fn an_install_without_a_proven_enable_is_reported_as_unknown() {
        let directory = tempdir().expect("temp dir");
        let plan = plan(directory.path(), true);
        let registry = MemoryRegistry::new();
        registry.deny_writes();

        let result = apply(&plan, &registry);

        assert!(result.ok, "the source is still installed");
        assert!(result.installed);
        assert_eq!(result.enabled, "unknown");
        assert!(result.needs_windhawk_ui);
        assert!(result.config_key.is_none());
        assert!(result.note.is_some());
    }

    #[test]
    fn a_remove_deletes_the_source_configuration_and_compiled_files() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let source = data.join("ModsSource").join("accent-color-sync.wh.cpp");
        fs::create_dir_all(source.parent().expect("parent")).expect("sources");
        fs::write(&source, b"// mod").expect("source");
        let engine = data.join("Engine").join("Mods").join("64");
        fs::create_dir_all(&engine).expect("engine");
        fs::write(engine.join("accent-color-sync_1.0_1.dll"), b"dll").expect("dll");

        let registry = MemoryRegistry::with(&[("accent-color-sync", false)]);
        let plan = ApplyPlan::remove(
            "accent-color-sync",
            &data,
            &directory.path().join("result.json"),
        );

        let result = apply(&plan, &registry);

        assert!(result.ok, "{result:?}");
        assert!(!result.installed);
        assert!(!source.exists());
        assert!(!engine.join("accent-color-sync_1.0_1.dll").exists());
        assert_eq!(registry.get("accent-color-sync"), None);
    }

    #[test]
    fn a_plan_file_round_trips_through_the_result_file() {
        let directory = tempdir().expect("temp dir");
        let plan = plan(directory.path(), true);
        let plan_path = directory.path().join("plan.json");
        fs::write(&plan_path, serde_json::to_vec(&plan).expect("plan json")).expect("plan");

        let result = run_plan_file(&plan_path, &MemoryRegistry::new()).expect("helper");
        let read = read_result(Path::new(&plan.result)).expect("result");

        assert_eq!(read, result);
        assert!(result.ok);
    }

    #[test]
    fn an_unsafe_plan_writes_a_failure_instead_of_touching_the_disk() {
        let directory = tempdir().expect("temp dir");
        let plan = ApplyPlan {
            action: Action::Install,
            id: "../../evil".to_owned(),
            data_dir: directory.path().join("Windhawk").display().to_string(),
            source: Some(directory.path().display().to_string()),
            result: directory.path().join("result.json").display().to_string(),
            disabled: Some(false),
        };
        let plan_path = directory.path().join("plan.json");
        fs::write(&plan_path, serde_json::to_vec(&plan).expect("plan json")).expect("plan");

        let result = run_plan_file(&plan_path, &MemoryRegistry::new()).expect("helper");

        assert!(!result.ok);
        assert!(!result.installed);
        assert_eq!(result.enabled, "unknown");
        assert!(result.needs_windhawk_ui);
        assert!(result.error.is_some());
    }

    #[test]
    fn replacing_an_installed_source_is_an_update_not_an_error() {
        let directory = tempdir().expect("temp dir");
        let first = plan(directory.path(), true);
        apply(&first, &MemoryRegistry::new());

        fs::write(
            directory.path().join("download.wh.cpp"),
            b"//! a newer source with a different length\n",
        )
        .expect("download");
        let second = apply(&first, &MemoryRegistry::new());

        assert!(second.ok, "{second:?}");
        let target = second.source_path.as_deref().expect("path");
        assert_eq!(
            fs::read(target).expect("read"),
            b"//! a newer source with a different length\n"
        );
    }

    #[test]
    fn a_remove_of_something_absent_succeeds() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let plan = ApplyPlan::remove("nothing-here", &data, &directory.path().join("result.json"));

        let result = apply(&plan, &MemoryRegistry::new());

        assert!(result.ok);
        assert!(!result.installed);
    }

    #[test]
    fn the_plan_data_directory_helper_keeps_a_non_empty_override() {
        let plan = ApplyPlan::remove("x", Path::new(r"D:\Data"), Path::new(r"C:\r.json"));

        assert_eq!(plan_data_dir(&plan), PathBuf::from(r"D:\Data"));
    }
}
