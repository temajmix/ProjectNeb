//! The install/remove plan and the values the elevated helper answers with.
//!
//! A plan is JSON on disk because that is the only thing a `runas` child shares with its parent:
//! the manager cannot read the helper's stdout through `ShellExecuteW`. The manager writes
//! [`ApplyPlan`] to a cache file, starts `neb-windhawk.exe --helper --plan <file>`, and reads
//! [`ApplyResult`] from `plan.result` when the helper is done.
//!
//! [`sanitize_id`] is the security boundary. A mod id becomes a file name and a registry subkey, so
//! it is accepted only when it is a bare Windhawk id: `0-9`, `a-z` and `-`. That refuses every
//! traversal shape (`..`, `/`, `\`, drive letters, NUL) before a path is built.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::paths;

/// What the helper has to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    /// Copy the downloaded source into `ModsSource` and optionally enable it.
    Install,
    /// Delete the source, the compiled DLLs and the mod's configuration key.
    Remove,
}

impl Action {
    /// The word the result carries.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Install => "install",
            Self::Remove => "remove",
        }
    }
}

/// One helper invocation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyPlan {
    /// Install or remove.
    pub action: Action,
    /// Sanitised mod id.
    pub id: String,
    /// Windhawk data directory (`...\Windhawk`, the parent of `ModsSource`).
    pub data_dir: String,
    /// Downloaded `.wh.cpp` for an install.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Where the helper writes the [`ApplyResult`].
    pub result: String,
    /// `Some(false)` asks for `Disabled=0`, `Some(true)` for `Disabled=1`; `None` leaves the
    /// registry alone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disabled: Option<bool>,
}

impl ApplyPlan {
    /// An install of `source` for `id`, optionally setting the enabled state.
    pub fn install(id: &str, data_dir: &Path, source: &Path, result: &Path, enable: bool) -> Self {
        Self {
            action: Action::Install,
            id: id.to_owned(),
            data_dir: data_dir.display().to_string(),
            source: Some(source.display().to_string()),
            result: result.display().to_string(),
            disabled: enable.then_some(false),
        }
    }

    /// A removal of `id` under `data_dir`.
    pub fn remove(id: &str, data_dir: &Path, result: &Path) -> Self {
        Self {
            action: Action::Remove,
            id: id.to_owned(),
            data_dir: data_dir.display().to_string(),
            source: None,
            result: result.display().to_string(),
            disabled: None,
        }
    }

    /// Checks everything the helper is about to touch.
    ///
    /// # Errors
    ///
    /// [`PlanError`] naming the first field that cannot be used.
    pub fn validate(&self) -> Result<(), PlanError> {
        if sanitize_id(&self.id)? != self.id {
            return Err(PlanError::Id(format!(
                "'{}' is not the canonical form '{}'",
                self.id,
                sanitize_id(&self.id)?
            )));
        }

        let data_dir = absolute(&self.data_dir, "dataDir")?;
        target_path(&data_dir, &self.id)?;
        absolute(&self.result, "result")?;

        if self.action == Action::Install {
            let source = self.source.as_deref().ok_or(PlanError::Source)?;
            let source = absolute(source, "source")?;
            if !source.is_file() {
                return Err(PlanError::SourceMissing(source));
            }
        }

        Ok(())
    }

    /// `ModsSource\<id>.wh.cpp` under the plan's data directory.
    ///
    /// # Errors
    ///
    /// [`PlanError`] for an unusable data directory or id.
    pub fn target(&self) -> Result<PathBuf, PlanError> {
        target_path(Path::new(&self.data_dir), &self.id)
    }

    /// The engine folder holding compiled DLLs.
    pub fn engine_mods(&self) -> PathBuf {
        paths::engine_mods(Path::new(&self.data_dir))
    }
}

/// The helper's answer, also serialised by the manager as the IPC result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApplyResult {
    /// Whether the requested end state was reached.
    pub ok: bool,
    /// The action that ran.
    pub action: Action,
    /// The sanitised id.
    pub id: String,
    /// Whether a source file exists after the operation.
    pub installed: bool,
    /// `enabled`, `disabled` or `unknown`; `unknown` is never a guess.
    pub enabled: String,
    /// Whether the UI should offer "Открыть Windhawk" because the state is not proven.
    pub needs_windhawk_ui: bool,
    /// Source file the helper wrote or removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Registry key the helper configured, when it could.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_key: Option<String>,
    /// Why the operation failed, when it did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A non-fatal explanation, e.g. why the enabled state is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl ApplyResult {
    /// A failed result for `plan`.
    pub fn failure(plan: &ApplyPlan, error: impl Into<String>) -> Self {
        Self {
            ok: false,
            action: plan.action,
            id: plan.id.clone(),
            installed: false,
            enabled: "unknown".to_owned(),
            needs_windhawk_ui: true,
            source_path: None,
            config_key: None,
            error: Some(error.into()),
            note: None,
        }
    }
}

/// Why a plan cannot be used.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum PlanError {
    /// The id is empty or carries a character that could escape its folder or key.
    #[error("'{0}' is not a valid mod id; ids use only a-z, 0-9 and '-'")]
    Id(String),

    /// A path field is empty or relative.
    #[error("{field} must be an absolute path, got '{value}'")]
    Path {
        /// Field name from the plan.
        field: &'static str,
        /// The rejected value.
        value: String,
    },

    /// The install plan names no source.
    #[error("an install plan needs a source file")]
    Source,

    /// The install plan's source does not exist.
    #[error("the downloaded source '{}' does not exist", .0.display())]
    SourceMissing(PathBuf),

    /// The computed target escaped `ModsSource`.
    #[error("'{0}' does not resolve inside ModsSource")]
    Escape(String),
}

/// Normalises a user-supplied id and refuses anything Windhawk itself would not accept.
///
/// The charset is Windhawk's own bare-id rule: lowercase ASCII letters, digits and hyphens. A
/// `.wh.cpp` suffix is accepted and trimmed because that is how catalog files are named.
///
/// # Errors
///
/// [`PlanError::Id`] for an empty, too long or otherwise unsafe id.
pub fn sanitize_id(raw: &str) -> Result<String, PlanError> {
    let trimmed = raw.trim();
    let trimmed = trimmed.strip_suffix(".wh.cpp").unwrap_or(trimmed);
    let lowered = trimmed.to_ascii_lowercase();

    let valid = !lowered.is_empty()
        && lowered.len() <= 128
        && !lowered.starts_with('-')
        && !lowered.ends_with('-')
        && lowered
            .chars()
            .all(|character| matches!(character, 'a'..='z' | '0'..='9' | '-'));

    if valid {
        Ok(lowered)
    } else {
        Err(PlanError::Id(raw.to_owned()))
    }
}

/// `ModsSource\<id>.wh.cpp`, proven to stay inside `ModsSource`.
///
/// # Errors
///
/// [`PlanError`] when the id is unsafe or the joined path escapes its folder.
pub fn target_path(data_dir: &Path, raw_id: &str) -> Result<PathBuf, PlanError> {
    let id = sanitize_id(raw_id)?;
    let source_dir = paths::mods_source(data_dir);
    let target = source_dir.join(format!("{id}.wh.cpp"));

    if target.parent() != Some(source_dir.as_path()) {
        return Err(PlanError::Escape(target.display().to_string()));
    }

    Ok(target)
}

fn absolute(value: &str, field: &'static str) -> Result<PathBuf, PlanError> {
    let path = PathBuf::from(value.trim());
    if value.trim().is_empty() || !path.is_absolute() {
        return Err(PlanError::Path {
            field,
            value: value.to_owned(),
        });
    }

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_pass_and_come_back_lowercase() {
        assert_eq!(
            sanitize_id("accent-color-sync").expect("id"),
            "accent-color-sync"
        );
        assert_eq!(
            sanitize_id("  Accent-Color-Sync.wh.cpp  ").expect("id"),
            "accent-color-sync"
        );
        assert_eq!(sanitize_id("mod-2").expect("id"), "mod-2");
    }

    #[test]
    fn traversal_shapes_are_refused_before_a_path_is_built() {
        for raw in [
            "",
            " ",
            "../evil",
            r"..\evil",
            "..",
            "a/b",
            r"a\b",
            r"C:\Windows",
            "a:b",
            "a b",
            "a_b",
            "local@mod",
            "-leading",
            "trailing-",
            "a\0b",
        ] {
            assert!(sanitize_id(raw).is_err(), "{raw:?} must be refused");
        }
    }

    #[test]
    fn an_overlong_id_is_refused() {
        let id = "a".repeat(129);

        assert!(sanitize_id(&id).is_err());
        assert!(sanitize_id(&"a".repeat(128)).is_ok());
    }

    #[test]
    fn the_target_always_stays_in_mods_source() {
        let data = Path::new(r"C:\ProgramData\Windhawk");
        let target = target_path(data, "../../evil").expect_err("must fail");

        assert!(matches!(target, PlanError::Id(_)));
        assert_eq!(
            target_path(data, "accent-color-sync").expect("target"),
            PathBuf::from(r"C:\ProgramData\Windhawk\ModsSource\accent-color-sync.wh.cpp")
        );
    }

    #[test]
    fn a_plan_for_a_relative_data_directory_is_refused() {
        let plan = ApplyPlan {
            action: Action::Remove,
            id: "accent-color-sync".to_owned(),
            data_dir: "Windhawk".to_owned(),
            source: None,
            result: r"C:\Temp\result.json".to_owned(),
            disabled: None,
        };

        assert!(matches!(
            plan.validate(),
            Err(PlanError::Path {
                field: "dataDir",
                ..
            })
        ));
    }

    #[test]
    fn an_install_without_its_download_is_refused() {
        let plan = ApplyPlan {
            action: Action::Install,
            id: "accent-color-sync".to_owned(),
            data_dir: r"C:\ProgramData\Windhawk".to_owned(),
            source: Some(r"C:\Temp\missing.wh.cpp".to_owned()),
            result: r"C:\Temp\result.json".to_owned(),
            disabled: Some(false),
        };

        assert!(matches!(plan.validate(), Err(PlanError::SourceMissing(_))));
    }

    #[test]
    fn a_failure_is_never_reported_as_enabled() {
        let plan = ApplyPlan::remove(
            "accent-color-sync",
            Path::new(r"C:\ProgramData\Windhawk"),
            Path::new(r"C:\Temp\result.json"),
        );
        let result = ApplyResult::failure(&plan, "access denied");

        assert!(!result.ok);
        assert_eq!(result.enabled, "unknown");
        assert!(result.needs_windhawk_ui);
        assert_eq!(result.error.as_deref(), Some("access denied"));
    }
}
