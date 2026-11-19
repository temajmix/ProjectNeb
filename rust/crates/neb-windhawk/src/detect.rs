//! Finding the Windhawk installation and its data directory.
//!
//! The candidate order is the one the machine actually needs: the machine-wide install, the
//! portable folder this machine uses, the per-user install, then whatever the uninstall registry
//! names. The first candidate that exists wins. Nothing here writes anything.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::ptr;

use serde::Serialize;
use windows::Win32::Foundation::{CloseHandle, ERROR_NO_MORE_ITEMS, ERROR_SUCCESS};
use windows::Win32::Storage::FileSystem::{
    GetFileVersionInfoSizeW, GetFileVersionInfoW, VerQueryValueW,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, KEY_QUERY_VALUE, REG_DWORD, RegCloseKey,
    RegEnumKeyExW, RegOpenKeyExW, RegQueryValueExW,
};
use windows::core::{PCWSTR, w};

/// Executable name of every Windhawk build.
pub const EXE_NAME: &str = "windhawk.exe";

/// `%ProgramFiles%\Windhawk\windhawk.exe` is the installer's default; the portable layout on this
/// machine lives at `A:\Windhawk`.
const PORTABLE_PATH: &str = r"A:\Windhawk\windhawk.exe";

/// One place a Windhawk executable may be, and where the hint came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Executable path.
    pub path: PathBuf,
    /// `programFiles`, `portable`, `localAppData` or `uninstall`.
    pub origin: &'static str,
}

/// What detection reports over IPC and from `--state`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Detection {
    /// Whether an executable was found.
    pub installed: bool,
    /// Version from the executable's version resource, or the `Engine\<version>` folder name.
    pub version: Option<String>,
    /// The executable.
    pub path: Option<String>,
    /// `AppDataPath` from `windhawk.ini`, or `%ProgramData%\Windhawk`.
    pub data_dir: Option<String>,
    /// Whether `windhawk.exe` is running in this session.
    pub running: bool,
    /// Which candidate matched.
    pub source: Option<String>,
}

/// The machine reads detection needs, isolated so the tests are not machine-dependent.
pub trait Probe: std::fmt::Debug {
    /// Whether `path` is a file.
    fn is_file(&self, path: &Path) -> bool;

    /// Reads a small text file, e.g. `windhawk.ini`.
    fn read_text(&self, path: &Path) -> Option<String>;

    /// The executable's `FileVersion` string.
    fn file_version(&self, path: &Path) -> Option<String>;

    /// Whether a process with this executable name runs.
    fn process_running(&self, exe_name: &str) -> bool;

    /// Install locations named by the uninstall registry.
    fn uninstall_locations(&self) -> Vec<PathBuf>;

    /// `%ProgramData%`, injected so tests can keep the default out of the assertion.
    fn program_data(&self) -> PathBuf;
}

/// The real machine.
#[derive(Debug, Clone, Copy, Default)]
pub struct RealProbe;

impl Probe for RealProbe {
    fn is_file(&self, path: &Path) -> bool {
        path.is_file()
    }

    fn read_text(&self, path: &Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    fn file_version(&self, path: &Path) -> Option<String> {
        file_version(path)
    }

    fn process_running(&self, exe_name: &str) -> bool {
        process_running(exe_name)
    }

    fn uninstall_locations(&self) -> Vec<PathBuf> {
        uninstall_locations()
    }

    fn program_data(&self) -> PathBuf {
        std::env::var_os("ProgramData")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\ProgramData"))
    }
}

/// The candidate order, with environment folders passed in so tests can name their own.
pub fn candidate_paths(
    program_files: Option<&Path>,
    local_app_data: Option<&Path>,
    uninstall: &[PathBuf],
) -> Vec<Candidate> {
    let mut candidates = Vec::new();

    if let Some(folder) = program_files {
        candidates.push(Candidate {
            path: folder.join("Windhawk").join(EXE_NAME),
            origin: "programFiles",
        });
    }

    candidates.push(Candidate {
        path: PathBuf::from(PORTABLE_PATH),
        origin: "portable",
    });

    if let Some(folder) = local_app_data {
        candidates.push(Candidate {
            path: folder.join("Programs").join("Windhawk").join(EXE_NAME),
            origin: "localAppData",
        });
    }

    for path in uninstall {
        candidates.push(Candidate {
            path: path.clone(),
            origin: "uninstall",
        });
    }

    candidates
}

/// Detects the installation on this machine.
pub fn detect(probe: &dyn Probe) -> Detection {
    let program_files = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let local_app_data = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let uninstall = probe.uninstall_locations();
    let candidates = candidate_paths(
        program_files.as_deref(),
        local_app_data.as_deref(),
        &uninstall,
    );

    detect_with(probe, &candidates, &probe.program_data())
}

/// Detection over an explicit candidate list, used by the tests.
pub fn detect_with(probe: &dyn Probe, candidates: &[Candidate], program_data: &Path) -> Detection {
    let running = probe.process_running(EXE_NAME);

    for candidate in candidates {
        if !probe.is_file(&candidate.path) {
            continue;
        }

        let exe_dir = candidate.path.parent().unwrap_or_else(|| Path::new(""));
        let data_dir = data_dir_from(probe, exe_dir, program_data);
        let version = probe
            .file_version(&candidate.path)
            .or_else(|| engine_version(exe_dir));

        return Detection {
            installed: true,
            version,
            path: Some(candidate.path.display().to_string()),
            data_dir: Some(data_dir.display().to_string()),
            running,
            source: Some(candidate.origin.to_owned()),
        };
    }

    Detection {
        running,
        data_dir: Some(program_data.join("Windhawk").display().to_string()),
        ..Detection::default()
    }
}

/// `AppDataPath` from the installation's `windhawk.ini`, else `%ProgramData%\Windhawk`.
pub fn data_dir_from(probe: &dyn Probe, exe_dir: &Path, program_data: &Path) -> PathBuf {
    let ini = exe_dir.join("windhawk.ini");
    let fallback = program_data.join("Windhawk");
    let Some(text) = probe.read_text(&ini) else {
        return fallback;
    };

    let Some(value) = ini_value(&text, "Storage", "AppDataPath") else {
        return fallback;
    };

    let expanded = value
        .replace("%ProgramData%", &program_data.display().to_string())
        .replace("%programdata%", &program_data.display().to_string());

    let path = PathBuf::from(expanded.trim());
    if path.as_os_str().is_empty() {
        return fallback;
    }
    if path.is_relative() {
        return exe_dir.join(path);
    }

    path
}

/// A `key=value` from one `[section]` of an INI file, for the two-line `windhawk.ini`.
pub fn ini_value(text: &str, section: &str, key: &str) -> Option<String> {
    let mut inside = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with([';', '#']) {
            continue;
        }

        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|rest| rest.strip_suffix(']'))
        {
            inside = name.trim().eq_ignore_ascii_case(section);
            continue;
        }

        if !inside {
            continue;
        }

        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case(key) {
            return Some(value.trim().to_owned());
        }
    }

    None
}

/// `Engine\<version>` next to the executable, when the version resource is unreadable.
fn engine_version(exe_dir: &Path) -> Option<String> {
    let mut versions = std::fs::read_dir(exe_dir.join("Engine"))
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_dir())
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| name.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .collect::<Vec<_>>();

    versions.sort();
    versions.pop()
}

/// Reads the `FileVersion` string from a PE's version resource.
fn file_version(path: &Path) -> Option<String> {
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    // SAFETY: `wide` is NUL-terminated and outlives the call.
    let size = unsafe { GetFileVersionInfoSizeW(PCWSTR(wide.as_ptr()), None) };
    if size == 0 {
        return None;
    }

    let mut data = vec![0u8; size as usize];
    // SAFETY: `data` is a live buffer of the size just queried.
    unsafe { GetFileVersionInfoW(PCWSTR(wide.as_ptr()), None, size, data.as_mut_ptr().cast()) }
        .ok()?;

    let mut translations: *mut c_void = ptr::null_mut();
    let mut length = 0u32;
    // SAFETY: `data` is the version block; the output pointers are live locals.
    let translated = unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            w!("\\VarFileInfo\\Translation"),
            &mut translations,
            &mut length,
        )
    };
    if !translated.as_bool() || length < 4 || translations.is_null() {
        return None;
    }

    // SAFETY: the translation block is an array of language/code-page pairs; the first pair is
    // four readable bytes because `length >= 4`.
    let (language, code_page) = unsafe {
        let pair = translations.cast::<u16>();
        (*pair, *pair.add(1))
    };

    let query = format!("\\StringFileInfo\\{language:04x}{code_page:04x}\\FileVersion");
    let query: Vec<u16> = query.encode_utf16().chain(std::iter::once(0)).collect();
    let mut text: *mut c_void = ptr::null_mut();
    let mut text_length = 0u32;

    // SAFETY: as above, with a NUL-terminated sub-block name and live output pointers.
    let found = unsafe {
        VerQueryValueW(
            data.as_ptr().cast(),
            PCWSTR(query.as_ptr()),
            &mut text,
            &mut text_length,
        )
    };
    if !found.as_bool() || text.is_null() || text_length == 0 {
        return None;
    }

    // SAFETY: the string block has `text_length` UTF-16 units, including the terminator.
    let slice = unsafe { std::slice::from_raw_parts(text.cast::<u16>(), text_length as usize) };
    let value = String::from_utf16_lossy(slice)
        .trim_end_matches('\0')
        .trim()
        .to_owned();

    (!value.is_empty()).then_some(value)
}

/// Whether `windhawk.exe` is running, by executable name.
fn process_running(exe_name: &str) -> bool {
    // SAFETY: the snapshot is a plain handle closed below; no thread access is requested.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return false;
    };

    let mut entry = PROCESSENTRY32W {
        dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };
    let mut found = false;
    let mut next = unsafe { Process32FirstW(snapshot, &mut entry) };

    while next.is_ok() {
        let name = String::from_utf16_lossy(&entry.szExeFile)
            .trim_end_matches('\0')
            .to_owned();
        if name.eq_ignore_ascii_case(exe_name) {
            found = true;
            break;
        }

        // SAFETY: `entry` is a live struct of the size it reports.
        next = unsafe { Process32NextW(snapshot, &mut entry) };
    }

    // SAFETY: the handle came from the call above and is closed exactly once.
    unsafe {
        let _ = CloseHandle(snapshot);
    }

    found
}

/// Install locations named by the machine-wide and per-user uninstall keys.
fn uninstall_locations() -> Vec<PathBuf> {
    let mut locations = Vec::new();

    for hive in [HKEY_LOCAL_MACHINE, HKEY_CURRENT_USER] {
        for root in [
            r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
        ] {
            read_uninstall(hive, root, &mut locations);
        }
    }

    locations.sort();
    locations.dedup();
    locations
}

fn read_uninstall(hive: HKEY, root: &str, locations: &mut Vec<PathBuf>) {
    let Some(key) = reg_open(hive, root) else {
        return;
    };

    for index in 0..4096u32 {
        let mut buffer = [0u16; 256];
        let mut length = buffer.len() as u32;

        // SAFETY: `buffer` is a live writable array of the length passed in.
        let status = unsafe {
            RegEnumKeyExW(
                key.0,
                index,
                Some(windows::core::PWSTR(buffer.as_mut_ptr())),
                &mut length,
                None,
                None,
                None,
                None,
            )
        };

        if status == ERROR_NO_MORE_ITEMS {
            break;
        }
        if status != ERROR_SUCCESS {
            break;
        }

        let name = String::from_utf16_lossy(&buffer[..length as usize]);
        let Some(entry) = reg_open(hive, &format!(r"{root}\{name}")) else {
            continue;
        };

        let display_name = reg_string(entry.0, "DisplayName").unwrap_or_default();
        if !display_name.to_ascii_lowercase().contains("windhawk") {
            continue;
        }

        if let Some(location) = reg_string(entry.0, "InstallLocation") {
            let location = PathBuf::from(location.trim().trim_matches('"'));
            if !location.as_os_str().is_empty() {
                let candidate = if location.is_dir() {
                    location.join(EXE_NAME)
                } else {
                    location
                };
                locations.push(candidate);
            }
        }

        if let Some(icon) = reg_string(entry.0, "DisplayIcon") {
            let icon = icon
                .split(',')
                .next()
                .unwrap_or_default()
                .trim()
                .trim_matches('"');
            if !icon.is_empty() {
                locations.push(PathBuf::from(icon));
            }
        }
    }
}

/// A registry key this module closes on drop.
struct RegKey(HKEY);

impl Drop for RegKey {
    fn drop(&mut self) {
        // SAFETY: the key was opened here and is closed exactly once.
        unsafe {
            let _ = RegCloseKey(self.0);
        }
    }
}

fn reg_open(hive: HKEY, path: &str) -> Option<RegKey> {
    let wide: Vec<u16> = path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = HKEY(ptr::null_mut());

    // SAFETY: `wide` is NUL-terminated and outlives the call; `key` is a live out-parameter.
    let status =
        unsafe { RegOpenKeyExW(hive, PCWSTR(wide.as_ptr()), None, KEY_QUERY_VALUE, &mut key) };
    if status != ERROR_SUCCESS {
        return None;
    }

    Some(RegKey(key))
}

fn reg_string(key: HKEY, name: &str) -> Option<String> {
    let name: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut kind = REG_DWORD;
    let mut data = [0u8; 1024];
    let mut length = data.len() as u32;

    // SAFETY: `name` is NUL-terminated and `data` is a live buffer whose byte length is passed in.
    let status = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            Some(&mut kind),
            Some(data.as_mut_ptr()),
            Some(&mut length),
        )
    };

    if status != ERROR_SUCCESS {
        return None;
    }

    // REG_SZ: UTF-16 including its terminator.
    let units: Vec<u16> = data[..length as usize]
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect();

    Some(
        String::from_utf16_lossy(&units)
            .trim_end_matches('\0')
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{BTreeMap, BTreeSet};

    #[derive(Debug, Default)]
    struct FakeProbe {
        files: BTreeSet<PathBuf>,
        versions: BTreeMap<PathBuf, String>,
        running: bool,
        uninstall: Vec<PathBuf>,
        program_data: PathBuf,
        texts: BTreeMap<PathBuf, String>,
    }

    impl Probe for FakeProbe {
        fn is_file(&self, path: &Path) -> bool {
            self.files.contains(path)
        }

        fn read_text(&self, path: &Path) -> Option<String> {
            self.texts.get(path).cloned()
        }

        fn file_version(&self, path: &Path) -> Option<String> {
            self.versions.get(path).cloned()
        }

        fn process_running(&self, _exe_name: &str) -> bool {
            self.running
        }

        fn uninstall_locations(&self) -> Vec<PathBuf> {
            self.uninstall.clone()
        }

        fn program_data(&self) -> PathBuf {
            self.program_data.clone()
        }
    }

    fn probe(files: &[&str]) -> FakeProbe {
        let mut probe = FakeProbe {
            program_data: PathBuf::from(r"C:\PD"),
            ..FakeProbe::default()
        };

        for path in files {
            probe.files.insert(PathBuf::from(path));
        }

        probe
    }

    fn candidates(paths: &[&str]) -> Vec<Candidate> {
        paths
            .iter()
            .map(|path| Candidate {
                path: PathBuf::from(path),
                origin: "uninstall",
            })
            .collect()
    }

    #[test]
    fn the_installer_path_wins_over_the_portable_folder() {
        let probe = probe(&[
            r"C:\Program Files\Windhawk\windhawk.exe",
            r"A:\Windhawk\windhawk.exe",
        ]);
        let list = candidate_paths(
            Some(Path::new(r"C:\Program Files")),
            Some(Path::new(r"C:\Users\a\AppData\Local")),
            &[],
        );

        let detection = detect_with(&probe, &list, Path::new(r"C:\PD"));

        assert!(detection.installed);
        assert_eq!(
            detection.path.as_deref(),
            Some(r"C:\Program Files\Windhawk\windhawk.exe")
        );
        assert_eq!(detection.source.as_deref(), Some("programFiles"));
    }

    #[test]
    fn the_portable_folder_is_used_when_program_files_has_nothing() {
        let probe = probe(&[r"A:\Windhawk\windhawk.exe"]);
        let list = candidate_paths(
            Some(Path::new(r"C:\Program Files")),
            Some(Path::new(r"C:\Users\a\AppData\Local")),
            &[],
        );

        let detection = detect_with(&probe, &list, Path::new(r"C:\PD"));

        assert_eq!(detection.path.as_deref(), Some(r"A:\Windhawk\windhawk.exe"));
        assert_eq!(detection.source.as_deref(), Some("portable"));
    }

    #[test]
    fn a_per_user_install_and_an_uninstall_hint_are_both_considered() {
        let probe = probe(&[
            r"C:\Users\a\AppData\Local\Programs\Windhawk\windhawk.exe",
            r"E:\Tools\Windhawk\windhawk.exe",
        ]);
        let list = candidate_paths(
            Some(Path::new(r"C:\Program Files")),
            Some(Path::new(r"C:\Users\a\AppData\Local")),
            &[PathBuf::from(r"E:\Tools\Windhawk\windhawk.exe")],
        );

        let detection = detect_with(&probe, &list, Path::new(r"C:\PD"));
        assert_eq!(detection.source.as_deref(), Some("localAppData"));

        let only_uninstall = probe_with_uninstall(r"E:\Tools\Windhawk\windhawk.exe");
        let list = candidate_paths(
            Some(Path::new(r"C:\Program Files")),
            Some(Path::new(r"C:\Users\a\AppData\Local")),
            &[PathBuf::from(r"E:\Tools\Windhawk\windhawk.exe")],
        );
        let detection = detect_with(&only_uninstall, &list, Path::new(r"C:\PD"));
        assert_eq!(
            detection.path.as_deref(),
            Some(r"E:\Tools\Windhawk\windhawk.exe")
        );
        assert_eq!(detection.source.as_deref(), Some("uninstall"));
    }

    fn probe_with_uninstall(path: &str) -> FakeProbe {
        let mut probe = probe(&[path]);
        probe.uninstall.push(PathBuf::from(path));
        probe
    }

    #[test]
    fn an_absent_install_reports_the_default_data_directory() {
        let probe = probe(&[]);
        let detection = detect_with(&probe, &candidates(&[]), Path::new(r"C:\PD"));

        assert!(!detection.installed);
        assert_eq!(detection.version, None);
        assert_eq!(detection.path, None);
        assert_eq!(detection.data_dir.as_deref(), Some(r"C:\PD\Windhawk"));
        assert!(!detection.running);
    }

    #[test]
    fn the_version_comes_from_the_executable_then_the_engine_folder() {
        let mut probe = probe(&[r"A:\Windhawk\windhawk.exe"]);
        probe.versions.insert(
            PathBuf::from(r"A:\Windhawk\windhawk.exe"),
            "1.7.3".to_owned(),
        );

        let detection = detect_with(
            &probe,
            &candidates(&[r"A:\Windhawk\windhawk.exe"]),
            Path::new(r"C:\PD"),
        );
        assert_eq!(detection.version.as_deref(), Some("1.7.3"));
    }

    #[test]
    fn windhawk_ini_names_the_data_directory_and_env_vars_expand() {
        let mut probe = probe(&[r"A:\Windhawk\windhawk.exe"]);
        probe.texts.insert(
            PathBuf::from(r"A:\Windhawk\windhawk.ini"),
            "\u{feff}\n[Storage]\nPortable=0\nAppDataPath=%ProgramData%\\WindhawkCustom\n"
                .to_owned(),
        );

        let detection = detect_with(
            &probe,
            &candidates(&[r"A:\Windhawk\windhawk.exe"]),
            Path::new(r"C:\PD"),
        );

        assert_eq!(detection.data_dir.as_deref(), Some(r"C:\PD\WindhawkCustom"));
    }

    #[test]
    fn a_relative_ini_path_resolves_against_the_executable_folder() {
        let data = data_dir_from(
            &probe_ini("[Storage]\nAppDataPath=Data\n"),
            Path::new(r"A:\Windhawk"),
            Path::new(r"C:\PD"),
        );

        assert_eq!(data, PathBuf::from(r"A:\Windhawk\Data"));
    }

    fn probe_ini(text: &str) -> FakeProbe {
        let mut probe = FakeProbe {
            program_data: PathBuf::from(r"C:\PD"),
            ..FakeProbe::default()
        };
        probe
            .texts
            .insert(PathBuf::from(r"A:\Windhawk\windhawk.ini"), text.to_owned());
        probe
    }

    #[test]
    fn ini_parsing_is_case_insensitive_and_ignores_comments() {
        let text = "[Storage]\n; comment\nappdatapath = C:\\Data # not a comment\n[Other]\nAppDataPath=no\n";

        assert_eq!(
            ini_value(text, "storage", "AppDataPath").as_deref(),
            Some(r"C:\Data # not a comment")
        );
        assert_eq!(ini_value(text, "Storage", "missing"), None);
    }

    #[test]
    fn a_missing_ini_value_falls_back_to_program_data() {
        let probe = probe(&[]);
        let data = data_dir_from(&probe, Path::new(r"A:\Windhawk"), Path::new(r"C:\PD"));

        assert_eq!(data, PathBuf::from(r"C:\PD\Windhawk"));
    }

    #[test]
    fn the_real_probe_reads_this_machine_without_writing() {
        let detection = detect(&RealProbe);

        assert!(!RealProbe.is_file(Path::new("C:\\Nothing")));
        assert!(!RealProbe.process_running("definitely-not-a-process.exe"));
        let _ = detection;
    }
}
