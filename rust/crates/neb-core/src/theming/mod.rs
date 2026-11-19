//! The Theme Manager: built-in presets, the package format, profiles and the day/night schedule.
//!
//! # Layout
//!
//! * [`presets`] — the six palettes compiled into `neb-core` with `include_str!`. They are available
//!   without an installation step: [`crate::theme::ThemeTokens::load`] falls back to them when no
//!   file answers, and [`ThemeStore`] lists them before any user theme.
//! * [`package`] — the import/export contract. A theme file is
//!   `{schemaVersion, id, name, description, author, tokens}`; a flat token document from an older
//!   build is still accepted, and unknown fields survive a round trip.
//! * [`profiles`] — named bundles of a theme id plus the shell settings the bar needs, written under
//!   `%LocalAppData%\ProjectNeb\profiles`. Applying one writes `config.json` atomically and returns
//!   the new config, so the caller can post `shell.reload`.
//! * [`schedule`] — the pure day/night model. The supervisor owns the timer; [`schedule::resolve`]
//!   and [`schedule::should_switch`] are the seam it calls.
//!
//! Nothing in this module touches a window, Direct2D or COM, so the settings window and the tests use
//! the same code the shell does.

mod package;
mod presets;
mod profiles;
pub mod schedule;

use std::fs;
use std::io::{self, Write};
use std::path::Path;

pub use package::{
    PACKAGE_MAX_DOCK_TILE, PACKAGE_MAX_RADIUS, PACKAGE_MAX_THEME_BAR_HEIGHT, PACKAGE_MIN_DOCK_TILE,
    PACKAGE_MIN_OPACITY, PACKAGE_MIN_THEME_BAR_HEIGHT, PACKAGE_SCHEMA_VERSION, Theme, ThemeError,
    ThemeStore, is_valid_id,
};
pub use presets::{BUILT_IN_IDS, built_in, built_in_ids, documents, is_builtin, tokens};
pub use profiles::{Profile, ProfileError, ProfileStore, ShellSnapshot};
pub use schedule::{
    DEFAULT_DAY_FROM, DEFAULT_NIGHT_FROM, ScheduleError, ThemeSchedule, TimeOfDay, resolve,
    schedule_path, should_switch,
};

/// Writes `bytes` to `path` through a temporary file and a rename.
///
/// The data root is on a local disk, so `sync_all` before the rename is enough to make a power cut
/// leave either the old file or the new one. The temporary name carries the process id, which keeps
/// two ProjectNeb processes from colliding in the same folder.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "theme.json".to_owned());
    let tmp = path.with_file_name(format!("{name}.tmp-{}", std::process::id()));

    let result = (|| -> io::Result<()> {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }

    result
}

/// Renders `value` as indented JSON with a trailing newline, the shape every ProjectNeb file uses.
pub(crate) fn pretty_json(value: &serde_json::Value) -> Result<String, serde_json::Error> {
    let mut text = serde_json::to_string_pretty(value)?;
    text.push('\n');
    Ok(text)
}
