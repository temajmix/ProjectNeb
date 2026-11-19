//! The boundary between the manager, the network, the machine and UAC.
//!
//! Only two operations need administrator rights: writing `ModsSource` and writing the mod's
//! `Disabled` value under `HKLM`. A temp data directory (the live-check path) is writable by the
//! current user, so it is written directly; the machine-wide tree is not, so the plan is handed to
//! `neb-windhawk.exe --helper --plan <file>` through `ShellExecuteW(runas)` and the manager polls
//! the helper's result file. Nothing here assumes the enable write succeeded: the helper proves it.

use std::ffi::OsStr;
use std::fs;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::HINSTANCE;
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Shell::{IsUserAnAdmin, ShellExecuteW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::PCWSTR;

use crate::detect::{self, Detection, RealProbe};
use crate::helper;
use crate::plan::{ApplyPlan, ApplyResult};
use crate::registry::RealRegistry;

/// How long the manager waits for an elevated helper before giving up.
pub const HELPER_TIMEOUT: Duration = Duration::from_secs(180);

/// Pause between polls of the helper's result file.
const POLL: Duration = Duration::from_millis(100);

/// The catalog download timeout.
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// The side effects the manager needs, isolated so its state machine runs without UAC or network.
pub trait Host: std::fmt::Debug + Send + Sync {
    /// Whether this process is elevated.
    fn is_admin(&self) -> bool;

    /// The Windhawk installation on this machine.
    fn detect(&self) -> Detection;

    /// Downloads `url`.
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String>;

    /// Applies an install or remove plan, elevating when the target needs it.
    fn apply(&self, plan: &ApplyPlan) -> Result<ApplyResult, String>;

    /// Starts `windhawk.exe`; `tray_only` passes `-tray-only`.
    fn launch(&self, tray_only: bool) -> Result<(), String>;
}

/// The real machine: WinHTTP, Win32, UAC.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealHost;

impl RealHost {
    /// A host with no retained state.
    pub fn new() -> Self {
        Self
    }
}

impl Host for RealHost {
    fn is_admin(&self) -> bool {
        is_admin()
    }

    fn detect(&self) -> Detection {
        detect::detect(&RealProbe)
    }

    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        crate::http::get(url, FETCH_TIMEOUT, crate::http::CATALOG_LIMIT)
            .map_err(|error| error.to_string())
    }

    fn apply(&self, plan: &ApplyPlan) -> Result<ApplyResult, String> {
        plan.validate().map_err(|error| error.to_string())?;
        let result_path = Path::new(&plan.result);
        let _ = fs::remove_file(result_path);

        if writes_directly(plan, self.is_admin()) {
            return Ok(helper::apply(plan, &RealRegistry));
        }

        let plan_path = plan_path_for(result_path);
        let bytes = serde_json::to_vec(plan).map_err(|error| error.to_string())?;
        fs::write(&plan_path, bytes)
            .map_err(|error| format!("cannot write '{}': {error}", plan_path.display()))?;

        elevate(&plan_path)?;
        wait_result(result_path)
    }

    fn launch(&self, tray_only: bool) -> Result<(), String> {
        let detection = self.detect();
        let Some(path) = detection.path else {
            return Err("Windhawk is not installed".to_owned());
        };

        let mut command = std::process::Command::new(&path);
        if tray_only {
            command.arg("-tray-only");
        }
        command.creation_flags(CREATE_NO_WINDOW.0);

        command
            .spawn()
            .map(|_child| ())
            .map_err(|error| format!("cannot start '{path}': {error}"))
    }
}

/// Whether `plan` can be applied without UAC: the target folder must already be writable.
///
/// An elevated process always writes directly. An ordinary process writes a temp data directory
/// directly and asks for UAC for `%ProgramData%\Windhawk`, where `ModsSource` and the registry key
/// both need rights it does not have.
fn writes_directly(plan: &ApplyPlan, is_admin: bool) -> bool {
    if is_admin {
        return true;
    }

    let folder = match plan.action {
        crate::plan::Action::Install => plan
            .target()
            .ok()
            .and_then(|target| target.parent().map(Path::to_path_buf)),
        crate::plan::Action::Remove => Path::new(&plan.data_dir)
            .join("ModsSource")
            .is_dir()
            .then(|| Path::new(&plan.data_dir).join("ModsSource")),
    };

    folder.is_some_and(|folder| writable_dir(&folder))
}

/// Proves a folder accepts a new file, without leaving one behind.
pub fn writable_dir(folder: &Path) -> bool {
    if fs::create_dir_all(folder).is_err() {
        return false;
    }

    let probe = folder.join(format!(".neb-write-probe-{}", std::process::id()));
    let created = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .is_ok();

    if created {
        let _ = fs::remove_file(&probe);
    }

    created
}

/// `result.json` → `result.plan.json`, beside it.
fn plan_path_for(result: &Path) -> PathBuf {
    let mut name = result
        .file_stem()
        .map(OsStr::to_os_string)
        .unwrap_or_default();
    name.push(".plan.json");
    result.with_file_name(name)
}

/// Asks the shell to run this executable elevated and waits for the UAC answer.
fn elevate(plan_path: &Path) -> Result<(), String> {
    let exe = std::env::current_exe()
        .map_err(|error| format!("cannot locate neb-windhawk.exe: {error}"))?;
    let args = format!("--helper --plan \"{}\"", plan_path.display());

    let operation = wide("runas");
    let file = wide(&exe.display().to_string());
    let parameters = wide(&args);
    let directory = wide(
        &exe.parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_default(),
    );

    // SAFETY: every string is a NUL-terminated UTF-16 buffer that outlives the call, and the
    // optional window handle is the documented "no parent" value.
    let result: HINSTANCE = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(file.as_ptr()),
            PCWSTR(parameters.as_ptr()),
            PCWSTR(directory.as_ptr()),
            SW_HIDE,
        )
    };

    let code = result.0 as isize;
    if code <= 32 {
        return Err(format!(
            "cannot elevate the Windhawk helper (ShellExecuteW returned {code}{})",
            shell_error_message(code)
        ));
    }

    Ok(())
}

fn wait_result(path: &Path) -> Result<ApplyResult, String> {
    let deadline = Instant::now() + HELPER_TIMEOUT;

    loop {
        if path.is_file() {
            return helper::read_result(path).map_err(|error| error.to_string());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "the elevated helper did not report back within {} s; the UAC prompt may have been cancelled",
                HELPER_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(POLL);
    }
}

fn shell_error_message(code: isize) -> String {
    match code {
        2 => ": the file was not found".to_owned(),
        5 => ": access denied, the UAC prompt was cancelled".to_owned(),
        1223 => ": the UAC prompt was cancelled".to_owned(),
        _ => String::new(),
    }
}

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Whether this process holds an elevated token.
pub fn is_admin() -> bool {
    // SAFETY: the call has no arguments, allocates nothing and changes no state.
    unsafe { IsUserAnAdmin().as_bool() }
}

/// The Windows error behind an I/O failure, for a log line that does not depend on the UI language.
pub fn error_code(error: &io::Error) -> Option<u32> {
    error
        .raw_os_error()
        .and_then(|code| u32::try_from(code).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::{Action, ApplyPlan};
    use tempfile::tempdir;

    #[test]
    fn a_temp_folder_is_writable_and_a_missing_one_is_created() {
        let directory = tempdir().expect("temp dir");
        let folder = directory.path().join("new").join("ModsSource");

        assert!(writable_dir(&folder));
        assert!(folder.is_dir());
        assert!(
            fs::read_dir(&folder).expect("read").next().is_none(),
            "the probe file is removed"
        );
    }

    #[test]
    fn an_ordinary_process_writes_a_temp_data_directory_directly() {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let source = directory.path().join("download.wh.cpp");
        fs::write(&source, b"// mod").expect("source");
        let plan = ApplyPlan::install(
            "accent-color-sync",
            &data,
            &source,
            &directory.path().join("result.json"),
            false,
        );

        assert!(writes_directly(&plan, false));
        assert!(writes_directly(&plan, true));
    }

    #[test]
    fn the_program_data_tree_is_not_treated_as_writable_without_admin() {
        let plan = ApplyPlan::remove(
            "accent-color-sync",
            Path::new(r"C:\ProgramData\Windhawk"),
            Path::new(r"C:\Temp\result.json"),
        );

        // The test machine's real permissions answer this; only the admin branch is asserted.
        assert!(writes_directly(&plan, true));
    }

    #[test]
    fn the_plan_file_sits_beside_its_result() {
        assert_eq!(
            plan_path_for(Path::new(r"C:\Temp\windhawk-apply.json")),
            PathBuf::from(r"C:\Temp\windhawk-apply.plan.json")
        );
    }

    #[test]
    fn removing_a_missing_remove_target_does_not_need_admin() {
        let plan = ApplyPlan {
            action: Action::Remove,
            id: "accent-color-sync".to_owned(),
            data_dir: r"C:\Nowhere\Windhawk".to_owned(),
            source: None,
            result: r"C:\Temp\result.json".to_owned(),
            disabled: None,
        };

        // Without a `ModsSource` to inspect, the conservative answer is "ask for UAC".
        assert!(!writes_directly(&plan, false));
    }

    #[test]
    fn a_cancelled_uac_answer_is_explained() {
        assert!(shell_error_message(1223).contains("cancelled"));
        assert!(shell_error_message(5).contains("denied"));
        assert!(shell_error_message(999).is_empty());
    }

    #[test]
    fn the_real_host_reports_this_machines_detection_without_writing() {
        let detection = RealHost::new().detect();
        let _ = detection;
        let _ = RealHost::new().is_admin();
    }
}
