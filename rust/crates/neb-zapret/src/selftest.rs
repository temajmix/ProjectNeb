//! `zapret.selfTest`: the checks the pack's `service.bat` performs, turned into data.
//!
//! The interesting part is pure: [`evaluate`] takes what the machine looks like and returns the
//! report, so every combination is a unit test. [`collect`] is the thin Win32 layer that fills the
//! input in: the Base Filtering Engine, the WinDivert service, a running `winws.exe`, the system
//! proxy and the hosts file.

use std::fs;
use std::path::PathBuf;

use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, ERROR_SERVICE_DOES_NOT_EXIST, ERROR_SUCCESS};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW,
};
use windows::Win32::System::Services::{
    CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_MANAGER_CONNECT,
    SERVICE_QUERY_STATUS, SERVICE_RUNNING, SERVICE_START_PENDING, SERVICE_STATUS,
    SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP_PENDING,
};
use windows::core::{HRESULT, PCWSTR};

use crate::host;
use crate::paths::Paths;
use crate::settings;

/// Service names a leftover zapret or GoodbyeDPI installation can leave behind.
const WINDIVERT_SERVICES: [&str; 3] = ["WinDivert", "WinDivert14", "WinDivert1.4"];

/// Domains the pack's hosts check looks for.
const BLOCKED_DOMAINS: [&str; 2] = ["youtube.com", "youtu.be"];

/// How one check ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// The machine is ready in this respect.
    Ok,
    /// It will probably work, but something deserves attention.
    Warn,
    /// zapret will not work until this is fixed.
    Fail,
    /// Something to know, not a problem.
    Info,
}

/// One named result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Check {
    /// Stable identifier, e.g. `windivert`.
    pub id: String,
    /// Outcome.
    pub status: CheckStatus,
    /// What was found, in one sentence.
    pub message: String,
}

impl Check {
    fn new(id: &str, status: CheckStatus, message: impl Into<String>) -> Self {
        Self {
            id: id.to_owned(),
            status,
            message: message.into(),
        }
    }
}

/// The document `zapret.selfTest` returns.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Report {
    /// Whether no check failed.
    pub ok: bool,
    /// Every check, in the order the diagnostics screen shows them.
    pub checks: Vec<Check>,
}

/// What a Windows service is doing.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceState {
    /// The service is not installed.
    #[default]
    Absent,
    /// Installed but not running.
    Stopped,
    /// Running, starting or stopping.
    Running,
    /// The service manager could not be asked.
    Unknown,
}

/// Everything [`evaluate`] needs; the fields are plain data so tests can build any machine.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Input {
    /// Whether a pack was found.
    pub installed: bool,
    /// Version marker of that pack.
    pub pack_version: String,
    /// Whether the manager is elevated.
    pub admin: bool,
    /// Base Filtering Engine, required by WinDivert.
    pub bfe: ServiceState,
    /// Whether any process called `winws.exe` is alive.
    pub winws_running: bool,
    /// The WinDivert service family.
    pub windivert: ServiceState,
    /// The system proxy server when the proxy is enabled.
    pub proxy: Option<String>,
    /// Hosts entries that point a blocked domain somewhere else.
    pub hosts_conflicts: Vec<String>,
    /// Why the hosts file could not be read, if it could not.
    pub hosts_error: Option<String>,
}

/// Turns the input into the report. Pure: every branch is covered by a test.
pub fn evaluate(input: &Input) -> Report {
    let mut checks = Vec::new();

    checks.push(if input.installed {
        Check::new(
            "pack",
            CheckStatus::Ok,
            format!("zapret pack {}", input.pack_version),
        )
    } else {
        Check::new(
            "pack",
            CheckStatus::Fail,
            "no zapret pack is imported; import the folder with bin\\winws.exe first",
        )
    });

    checks.push(if input.admin {
        Check::new("admin", CheckStatus::Ok, "the manager is elevated")
    } else {
        Check::new(
            "admin",
            CheckStatus::Info,
            "the manager is not elevated; starting a strategy will show the UAC prompt",
        )
    });

    checks.push(match input.bfe {
        ServiceState::Running => {
            Check::new("bfe", CheckStatus::Ok, "Base Filtering Engine is running")
        }
        ServiceState::Unknown => Check::new(
            "bfe",
            CheckStatus::Warn,
            "the Base Filtering Engine could not be queried",
        ),
        _ => Check::new(
            "bfe",
            CheckStatus::Fail,
            "Base Filtering Engine (BFE) is not running; WinDivert needs it",
        ),
    });

    checks.push(match (input.windivert, input.winws_running) {
        (ServiceState::Running, true) => Check::new(
            "windivert",
            CheckStatus::Ok,
            "WinDivert is in use by winws.exe",
        ),
        (ServiceState::Running, false) => Check::new(
            "windivert",
            CheckStatus::Fail,
            "the WinDivert service is running but winws.exe is not; it will block a new start",
        ),
        (ServiceState::Stopped, _) => Check::new(
            "windivert",
            CheckStatus::Warn,
            "a stopped WinDivert service is left over; a new start should reuse it",
        ),
        (ServiceState::Unknown, _) => Check::new(
            "windivert",
            CheckStatus::Warn,
            "the WinDivert service could not be queried",
        ),
        (ServiceState::Absent, _) => Check::new(
            "windivert",
            CheckStatus::Ok,
            "no WinDivert service is active",
        ),
    });

    checks.push(if input.winws_running {
        Check::new("winws", CheckStatus::Ok, "winws.exe is running")
    } else {
        Check::new("winws", CheckStatus::Info, "winws.exe is not running")
    });

    checks.push(match &input.proxy {
        Some(server) => Check::new(
            "proxy",
            CheckStatus::Warn,
            format!("the system proxy is enabled: {server}"),
        ),
        None => Check::new("proxy", CheckStatus::Ok, "the system proxy is disabled"),
    });

    checks.push(match &input.hosts_error {
        Some(error) => Check::new(
            "hosts",
            CheckStatus::Warn,
            format!("the hosts file could not be read: {error}"),
        ),
        None if input.hosts_conflicts.is_empty() => Check::new(
            "hosts",
            CheckStatus::Ok,
            "the hosts file has no YouTube entries",
        ),
        None => Check::new(
            "hosts",
            CheckStatus::Warn,
            format!(
                "the hosts file contains entries for {}; they can break YouTube",
                input.hosts_conflicts.join(", ")
            ),
        ),
    });

    Report {
        ok: !checks.iter().any(|check| check.status == CheckStatus::Fail),
        checks,
    }
}

/// Reads the live machine and evaluates it.
pub fn collect() -> Report {
    let paths = Paths::try_default();
    let pack = match &paths {
        Ok(paths) => settings::resolve(&settings::load(paths), paths).pack,
        Err(_) => None,
    };

    let hosts = read_hosts();
    let input = Input {
        installed: pack.is_some(),
        pack_version: pack.map(|pack| pack.version).unwrap_or_default(),
        admin: host::is_admin(),
        bfe: service_state("BFE"),
        winws_running: winws_running(),
        windivert: windivert_state(),
        proxy: read_proxy(),
        hosts_conflicts: hosts
            .as_ref()
            .map_or_else(|_| Vec::new(), |text| hosts_conflicts(text)),
        hosts_error: hosts.err(),
    };

    evaluate(&input)
}

/// Hosts entries that map `youtube.com` or `youtu.be` anywhere.
///
/// Commented lines are ignored even when they mention YouTube: a note in the file is not a
/// redirection. The check mirrors the pack's `findstr` warning, but reads the syntax instead of the
/// raw bytes.
pub fn hosts_conflicts(text: &str) -> Vec<String> {
    let mut found = Vec::new();

    for line in text.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() {
            continue;
        }

        let mut fields = line.split_whitespace();
        let Some(_address) = fields.next() else {
            continue;
        };

        for host in fields {
            let lower = host.to_ascii_lowercase();
            if BLOCKED_DOMAINS
                .iter()
                .any(|domain| lower == *domain || lower.ends_with(&format!(".{domain}")))
            {
                found.push(host.to_owned());
            }
        }
    }

    found.sort();
    found.dedup();
    found
}

/// `%SystemRoot%\System32\drivers\etc\hosts`, read as UTF-8 with a lossy fallback.
fn read_hosts() -> Result<String, String> {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
    let path = root
        .join("System32")
        .join("drivers")
        .join("etc")
        .join("hosts");

    fs::read(&path)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .map_err(|error| format!("{}: {error}", path.display()))
}

/// The system proxy server, when `ProxyEnable` is set.
fn read_proxy() -> Option<String> {
    let subkey = wide(r"Software\Microsoft\Windows\CurrentVersion\Internet Settings");
    let enabled_name = wide("ProxyEnable");
    let mut enabled = 0u32;
    let mut size = u32::try_from(std::mem::size_of::<u32>()).unwrap_or(4);

    // SAFETY: both strings are NUL-terminated and outlive the call; `enabled` and `size` are live
    // out-parameters of the sizes the call documents.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(enabled_name.as_ptr()),
            RRF_RT_REG_DWORD,
            None,
            Some(std::ptr::from_mut(&mut enabled).cast()),
            Some(&mut size),
        )
    };

    if status != ERROR_SUCCESS || enabled == 0 {
        return None;
    }

    let server_name = wide("ProxyServer");
    let mut buffer = [0u16; 512];
    let mut size = u32::try_from(buffer.len() * 2).unwrap_or(1024);

    // SAFETY: the name is NUL-terminated, `buffer` is a live writable array and `size` is its byte
    // length, as `RegGetValueW` expects for a string value.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(subkey.as_ptr()),
            PCWSTR(server_name.as_ptr()),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut size),
        )
    };

    if status != ERROR_SUCCESS {
        return Some("(enabled, no server recorded)".to_owned());
    }

    let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(0);
    let server = String::from_utf16_lossy(&buffer[..length])
        .trim()
        .to_owned();

    Some(if server.is_empty() {
        "(enabled, no server recorded)".to_owned()
    } else {
        server
    })
}

/// The state of the first WinDivert service that exists.
fn windivert_state() -> ServiceState {
    let mut seen_stopped = false;

    for name in WINDIVERT_SERVICES {
        match service_state(name) {
            ServiceState::Running => return ServiceState::Running,
            ServiceState::Unknown => return ServiceState::Unknown,
            ServiceState::Stopped => seen_stopped = true,
            ServiceState::Absent => {}
        }
    }

    if seen_stopped {
        ServiceState::Stopped
    } else {
        ServiceState::Absent
    }
}

/// Queries one service.
fn service_state(name: &str) -> ServiceState {
    let manager =
        match unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) } {
            Ok(manager) => manager,
            Err(_) => return ServiceState::Unknown,
        };

    let wide_name = wide(name);
    // SAFETY: the service name is NUL-terminated and outlives the call.
    let service =
        unsafe { OpenServiceW(manager, PCWSTR(wide_name.as_ptr()), SERVICE_QUERY_STATUS) };

    let service = match service {
        Ok(service) => service,
        Err(error) => {
            // SAFETY: `manager` is a live service-manager handle owned by this function.
            unsafe {
                let _ = CloseServiceHandle(manager);
            }

            return if error.code() == HRESULT::from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0) {
                ServiceState::Absent
            } else {
                ServiceState::Unknown
            };
        }
    };

    let mut status = SERVICE_STATUS::default();
    // SAFETY: `service` has SERVICE_QUERY_STATUS and `status` is a live out-parameter.
    let queried = unsafe { QueryServiceStatus(service, &mut status) }.is_ok();

    // SAFETY: both handles are owned by this function and closed exactly once.
    unsafe {
        let _ = CloseServiceHandle(service);
        let _ = CloseServiceHandle(manager);
    }

    if !queried {
        return ServiceState::Unknown;
    }

    state_from(status.dwCurrentState)
}

fn state_from(state: SERVICE_STATUS_CURRENT_STATE) -> ServiceState {
    if state == SERVICE_RUNNING || state == SERVICE_START_PENDING || state == SERVICE_STOP_PENDING {
        ServiceState::Running
    } else {
        ServiceState::Stopped
    }
}

/// Whether any process is called `winws.exe`.
fn winws_running() -> bool {
    // SAFETY: the snapshot takes no buffers; the handle is closed below.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return false;
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: u32::try_from(std::mem::size_of::<PROCESSENTRY32W>()).unwrap_or(0),
        ..Default::default()
    };

    let mut found = false;
    // SAFETY: the snapshot is live and `entry` is a valid writable structure.
    if unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok() {
        loop {
            if wide_name(&entry.szExeFile).eq_ignore_ascii_case("winws.exe") {
                found = true;
                break;
            }

            // SAFETY: the snapshot is still live and `entry` is a valid writable structure.
            if unsafe { Process32NextW(snapshot, &mut entry) }.is_err() {
                break;
            }
        }
    }

    // SAFETY: the handle is owned by this function and closed exactly once.
    unsafe {
        let _ = CloseHandle(snapshot);
    }

    found
}

fn wide_name(name: &[u16]) -> String {
    let length = name
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(name.len());
    String::from_utf16_lossy(&name[..length])
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn healthy() -> Input {
        Input {
            installed: true,
            pack_version: "0123456789abcdef".to_owned(),
            admin: true,
            bfe: ServiceState::Running,
            winws_running: false,
            windivert: ServiceState::Absent,
            proxy: None,
            hosts_conflicts: Vec::new(),
            hosts_error: None,
        }
    }

    fn check<'a>(report: &'a Report, id: &str) -> &'a Check {
        report
            .checks
            .iter()
            .find(|check| check.id == id)
            .unwrap_or_else(|| panic!("no check called {id}"))
    }

    #[test]
    fn a_healthy_machine_passes_every_check() {
        let report = evaluate(&healthy());

        assert!(report.ok);
        assert_eq!(report.checks.len(), 7);
        for check in &report.checks {
            assert_ne!(check.status, CheckStatus::Fail, "{check:?}");
        }
        assert_eq!(check(&report, "pack").status, CheckStatus::Ok);
        assert_eq!(check(&report, "bfe").status, CheckStatus::Ok);
    }

    #[test]
    fn a_missing_pack_is_the_only_hard_failure_on_a_clean_machine() {
        let report = evaluate(&Input {
            installed: false,
            pack_version: String::new(),
            ..healthy()
        });

        assert!(!report.ok);
        assert_eq!(check(&report, "pack").status, CheckStatus::Fail);
    }

    #[test]
    fn a_running_windivert_without_winws_is_a_conflict() {
        let report = evaluate(&Input {
            windivert: ServiceState::Running,
            winws_running: false,
            ..healthy()
        });

        assert!(!report.ok);
        let windivert = check(&report, "windivert");
        assert_eq!(windivert.status, CheckStatus::Fail);
        assert!(windivert.message.contains("winws.exe"), "{windivert:?}");
    }

    #[test]
    fn a_running_windivert_used_by_winws_is_fine() {
        let report = evaluate(&Input {
            windivert: ServiceState::Running,
            winws_running: true,
            ..healthy()
        });

        assert_eq!(check(&report, "windivert").status, CheckStatus::Ok);
        assert_eq!(check(&report, "winws").status, CheckStatus::Ok);
    }

    #[test]
    fn a_stopped_windivert_is_a_warning_not_a_failure() {
        let report = evaluate(&Input {
            windivert: ServiceState::Stopped,
            ..healthy()
        });

        assert!(report.ok);
        assert_eq!(check(&report, "windivert").status, CheckStatus::Warn);
    }

    #[test]
    fn a_stopped_bfe_is_a_failure() {
        let report = evaluate(&Input {
            bfe: ServiceState::Stopped,
            ..healthy()
        });

        assert!(!report.ok);
        assert_eq!(check(&report, "bfe").status, CheckStatus::Fail);
    }

    #[test]
    fn an_enabled_proxy_is_reported_with_its_server() {
        let report = evaluate(&Input {
            proxy: Some("127.0.0.1:7890".to_owned()),
            ..healthy()
        });

        assert!(report.ok, "a proxy is a warning, not a failure");
        let proxy = check(&report, "proxy");
        assert_eq!(proxy.status, CheckStatus::Warn);
        assert!(proxy.message.contains("127.0.0.1:7890"), "{proxy:?}");
    }

    #[test]
    fn hosts_entries_for_youtube_are_reported() {
        let report = evaluate(&Input {
            hosts_conflicts: vec!["youtube.com".to_owned()],
            ..healthy()
        });

        assert!(report.ok);
        let hosts = check(&report, "hosts");
        assert_eq!(hosts.status, CheckStatus::Warn);
        assert!(hosts.message.contains("youtube.com"), "{hosts:?}");
    }

    #[test]
    fn an_unreadable_hosts_file_is_a_warning() {
        let report = evaluate(&Input {
            hosts_error: Some("access denied".to_owned()),
            ..healthy()
        });

        assert_eq!(check(&report, "hosts").status, CheckStatus::Warn);
    }

    #[test]
    fn hosts_parsing_ignores_comments_and_accepts_subdomains() {
        let text = "\
# 0.0.0.0 youtube.com in a comment
127.0.0.1 localhost
0.0.0.0 www.youtube.com
0.0.0.0 youtu.be # trailing comment
0.0.0.0 example.com
";
        let conflicts = hosts_conflicts(text);

        assert_eq!(conflicts, vec!["www.youtube.com", "youtu.be"]);
    }

    #[test]
    fn a_clean_hosts_file_reports_nothing() {
        assert!(hosts_conflicts("127.0.0.1 localhost\n::1 localhost\n").is_empty());
        assert!(hosts_conflicts("").is_empty());
    }

    #[test]
    fn the_live_report_is_well_formed() {
        let report = collect();

        assert_eq!(report.checks.len(), 7);
        assert_eq!(
            report.ok,
            !report
                .checks
                .iter()
                .any(|check| check.status == CheckStatus::Fail)
        );
    }
}
