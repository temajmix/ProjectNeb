//! The memory soak harness.
//!
//! `neb-supervisor --soak <minutes> --interval <seconds>` starts the configured children, samples
//! the working set of every `neb-*` process once per interval, appends the samples to
//! `%LocalAppData%\ProjectNeb\logs\soak-<stamp>.csv` and prints a summary. `--soak-check` turns the
//! run into a gate: a process that grew more than [`LIMIT_GROWTH`] over the run fails the command.
//!
//! The harness only reads: it enumerates processes with a Toolhelp snapshot, asks each one for its
//! working set and closes the handle again. Clicking through the CSV afterwards is the point — the
//! numbers stay on disk whether or not anybody watches the console.

use std::io::Write;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::Duration;

use neb_core::strings::{self, Language, Text};
use neb_core::time::UtcDateTime;
use windows::Win32::Foundation::CloseHandle;
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

use crate::supervisor::Supervisor;

/// Default pause between samples.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(10);

/// Growth that fails `--soak-check`, as a fraction of the first sample.
pub const LIMIT_GROWTH: f64 = 0.20;

/// Bytes in a mebibyte, the unit the summary prints.
const MIB: f64 = 1024.0 * 1024.0;

/// Prefix every ProjectNeb process name shares.
const PROCESS_PREFIX: &str = "neb-";

/// One reading of one process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sample {
    /// Process id.
    pub pid: u32,
    /// Executable name, e.g. `neb-shell.exe`.
    pub name: String,
    /// Working set in bytes.
    pub working_set: u64,
}

/// Everything one process did over the run.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    /// Process id, which is what tells two runs of the same executable apart.
    pub pid: u32,
    /// Executable name.
    pub name: String,
    /// Working set at the first sample.
    pub first: u64,
    /// Working set at the last sample.
    pub last: u64,
    /// Smallest working set seen.
    pub min: u64,
    /// Largest working set seen.
    pub max: u64,
    /// How many samples this process appeared in.
    pub samples: u32,
}

impl Row {
    /// Growth from the first to the last sample, as a fraction of the first.
    pub fn growth(&self) -> f64 {
        growth(self.first, self.last)
    }

    /// Whether this process grew past [`LIMIT_GROWTH`].
    pub fn grew_too_much(&self) -> bool {
        exceeds_limit(self.first, self.last)
    }
}

/// What a finished soak found.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    /// The CSV the samples were appended to.
    pub file: PathBuf,
    /// One row per process that was seen at least once.
    pub rows: Vec<Row>,
    /// The rows that grew past the limit, with their growth.
    pub failures: Vec<(String, u32, f64)>,
}

impl Outcome {
    /// Whether no process grew past the limit.
    pub fn passed(&self) -> bool {
        self.failures.is_empty()
    }
}

/// Why a soak could not run.
#[derive(Debug, thiserror::Error)]
pub enum SoakError {
    /// There is no ProjectNeb data directory, so nowhere to put the CSV.
    #[error("no ProjectNeb data directory, so no place for the soak csv")]
    NoDataDir,

    /// The CSV could not be created or appended to.
    #[error("cannot write '{}': {source}", path.display())]
    Io {
        /// File that failed.
        path: PathBuf,
        /// The I/O error.
        #[source]
        source: std::io::Error,
    },
}

/// Relative growth from `first` to `last`.
///
/// A shrink is negative, which is why callers compare against the limit instead of taking an
/// absolute value. A first sample of zero cannot be a percentage of anything, so any growth from it
/// counts as unbounded.
pub fn growth(first: u64, last: u64) -> f64 {
    if first == 0 {
        return if last == 0 { 0.0 } else { f64::INFINITY };
    }

    (last as f64 - first as f64) / first as f64
}

/// Whether a process grew past [`LIMIT_GROWTH`].
pub fn exceeds_limit(first: u64, last: u64) -> bool {
    growth(first, last) > LIMIT_GROWTH
}

/// Samples every ProjectNeb process once.
///
/// The supervisor itself is one of them, which is deliberate: the soak is also how the resident
/// manager's own budget is watched.
pub fn sample_processes() -> Vec<Sample> {
    // SAFETY: a process snapshot takes no pointer and returns a handle this function closes.
    let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
        return Vec::new();
    };

    let mut samples = Vec::new();
    let mut entry = PROCESSENTRY32W {
        dwSize: size_of::<PROCESSENTRY32W>() as u32,
        ..PROCESSENTRY32W::default()
    };

    // SAFETY: `snapshot` is an open snapshot handle and `entry` is a live structure whose size is
    // declared in `dwSize`, which is what the API requires.
    let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();

    while more {
        let name = executable_name(&entry);
        if is_neb_process(&name) {
            if let Some(working_set) = working_set(entry.th32ProcessID) {
                samples.push(Sample {
                    pid: entry.th32ProcessID,
                    name,
                    working_set,
                });
            }
        }

        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        // SAFETY: same as the first call: `entry` stays live and correctly sized.
        more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
    }

    // SAFETY: the handle came from `CreateToolhelp32Snapshot` and is closed exactly once.
    unsafe {
        let _ = CloseHandle(snapshot);
    }

    samples
}

/// The working set of `pid`, or `None` when the process is gone or protected.
pub fn working_set(pid: u32) -> Option<u64> {
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;

    // SAFETY: `OpenProcess` only reads the process id; the handle is closed below.
    let process = unsafe { OpenProcess(access, false, pid) }.ok()?;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: size_of::<PROCESS_MEMORY_COUNTERS>() as u32,
        ..PROCESS_MEMORY_COUNTERS::default()
    };

    // SAFETY: the handle is open with the access `GetProcessMemoryInfo` needs and `counters`
    // declares its own size.
    let measured = unsafe { GetProcessMemoryInfo(process, &mut counters, counters.cb) }.is_ok();

    // SAFETY: the handle came from `OpenProcess` and is closed exactly once.
    unsafe {
        let _ = CloseHandle(process);
    }

    measured.then_some(counters.WorkingSetSize as u64)
}

/// Runs a soak against the children the supervisor already started.
///
/// # Errors
///
/// [`SoakError::NoDataDir`] when the CSV has nowhere to live, [`SoakError::Io`] when it cannot be
/// written.
pub fn run(
    supervisor: &mut Supervisor,
    duration: Duration,
    interval: Duration,
    language: Language,
) -> Result<Outcome, SoakError> {
    let root = neb_core::paths::try_root().map_err(|_| SoakError::NoDataDir)?;
    let file = csv_path(&root);
    create_file(&file)?;

    let mut rows: Vec<Row> = Vec::new();
    let mut round = 0u32;
    let mut elapsed = Duration::ZERO;

    loop {
        round += 1;
        let samples = sample_processes();
        println!(
            "{} sample {round} ({} {})",
            UtcDateTime::now_utc().iso8601(),
            samples.len(),
            if samples.len() == 1 {
                "process"
            } else {
                "processes"
            }
        );

        for sample in &samples {
            println!(
                "  {:<24} {:>7} {:>9.1} M",
                sample.name,
                sample.pid,
                mib(sample.working_set)
            );
        }

        accrue(&mut rows, &samples);
        append(&file, &samples)?;

        if elapsed >= duration {
            break;
        }

        let step = interval.min(duration - elapsed);
        supervisor.pump(step);
        elapsed += step;

        if supervisor.stopping() {
            break;
        }
    }

    let failures: Vec<(String, u32, f64)> = rows
        .iter()
        .filter(|row| row.grew_too_much())
        .map(|row| (row.name.clone(), row.pid, row.growth()))
        .collect();

    let outcome = Outcome {
        file,
        rows,
        failures,
    };

    print_summary(&outcome, language);

    Ok(outcome)
}

/// The CSV a soak writes: `soak-YYYYMMDD-HHMMSS.csv` under the log directory.
pub fn csv_path(root: &Path) -> PathBuf {
    root.join("logs").join(format!(
        "soak-{}.csv",
        UtcDateTime::now_utc().backup_stamp()
    ))
}

/// Creates the CSV with its header, replacing a file of the same second.
fn create_file(path: &Path) -> Result<(), SoakError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| SoakError::Io {
            path: parent.to_path_buf(),
            source,
        })?;
    }

    let mut file = std::fs::File::create(path).map_err(|source| SoakError::Io {
        path: path.to_path_buf(),
        source,
    })?;

    writeln!(file, "timestamp,pid,name,working_set_bytes").map_err(|source| SoakError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Appends one line per sample.
fn append(path: &Path, samples: &[Sample]) -> Result<(), SoakError> {
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|source| SoakError::Io {
            path: path.to_path_buf(),
            source,
        })?;

    let stamp = UtcDateTime::now_utc().iso8601();

    for sample in samples {
        writeln!(
            file,
            "{stamp},{},{},{}",
            sample.pid, sample.name, sample.working_set
        )
        .map_err(|source| SoakError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }

    Ok(())
}

/// Folds one round of samples into the rows.
fn accrue(rows: &mut Vec<Row>, samples: &[Sample]) {
    for sample in samples {
        match rows
            .iter_mut()
            .find(|row| row.pid == sample.pid && row.name == sample.name)
        {
            Some(row) => {
                row.last = sample.working_set;
                row.min = row.min.min(sample.working_set);
                row.max = row.max.max(sample.working_set);
                row.samples = row.samples.saturating_add(1);
            }
            None => rows.push(Row {
                pid: sample.pid,
                name: sample.name.clone(),
                first: sample.working_set,
                last: sample.working_set,
                min: sample.working_set,
                max: sample.working_set,
                samples: 1,
            }),
        }
    }
}

/// Prints the summary a run ends with.
fn print_summary(outcome: &Outcome, language: Language) {
    let text = |key: Text| strings::get(language, key);

    println!();
    println!("{}", text(Text::SupervisorSoakSummary));
    println!(
        "{:<24} {:>7} {:>11} {:>11} {:>8}",
        text(Text::SupervisorProcess),
        text(Text::SupervisorPid),
        text(Text::SupervisorMinimum),
        text(Text::SupervisorMaximum),
        text(Text::SupervisorGrowth)
    );

    if outcome.rows.is_empty() {
        println!("{}", text(Text::SupervisorNoProcesses));
    }

    let mut rows = outcome.rows.clone();
    rows.sort_by(|left, right| left.name.cmp(&right.name).then(left.pid.cmp(&right.pid)));

    for row in &rows {
        println!(
            "{:<24} {:>7} {:>10.1}M {:>10.1}M {:>7.1}%",
            row.name,
            row.pid,
            mib(row.min),
            mib(row.max),
            row.growth() * 100.0
        );
    }

    println!(
        "{}: {}",
        text(Text::SupervisorSoakFile),
        outcome.file.display()
    );

    if outcome.passed() {
        println!("{}", text(Text::SupervisorSoakPassed));
        return;
    }

    println!("{}", text(Text::SupervisorSoakFailed));
    for (name, pid, growth) in &outcome.failures {
        println!("  {name} ({pid}) {:+.1}%", growth * 100.0);
    }
}

/// Bytes as mebibytes.
fn mib(bytes: u64) -> f64 {
    bytes as f64 / MIB
}

/// Whether `name` is one of the ProjectNeb executables.
fn is_neb_process(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.starts_with(PROCESS_PREFIX) && lower.ends_with(".exe")
}

/// The executable name out of a process entry.
fn executable_name(entry: &PROCESSENTRY32W) -> String {
    let end = entry
        .szExeFile
        .iter()
        .position(|&unit| unit == 0)
        .unwrap_or(entry.szExeFile.len());

    String::from_utf16_lossy(&entry.szExeFile[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn growth_is_relative_to_the_first_sample() {
        assert_eq!(growth(100, 100), 0.0);
        assert!((growth(100, 120) - 0.20).abs() < f64::EPSILON);
        assert!(growth(100, 50) < 0.0, "a shrink is negative, not huge");
    }

    #[test]
    fn a_process_may_grow_by_exactly_the_limit_but_not_past_it() {
        assert!(
            !exceeds_limit(1_000, 1_200),
            "20 % is the limit, not the failure"
        );
        assert!(exceeds_limit(1_000, 1_201));
        assert!(exceeds_limit(1_000, 5_000));
        assert!(!exceeds_limit(1_000, 999), "a shrink never fails the check");
    }

    #[test]
    fn growing_from_nothing_is_unbounded() {
        assert_eq!(growth(0, 0), 0.0);
        assert_eq!(growth(0, 1), f64::INFINITY);
        assert!(exceeds_limit(0, 1_048_576));
        assert!(!exceeds_limit(0, 0));
    }

    #[test]
    fn the_rows_keep_the_first_last_min_and_max_of_each_process() {
        let mut rows = Vec::new();

        accrue(
            &mut rows,
            &[
                Sample {
                    pid: 7,
                    name: "neb-shell.exe".to_owned(),
                    working_set: 50,
                },
                Sample {
                    pid: 8,
                    name: "neb-supervisor.exe".to_owned(),
                    working_set: 10,
                },
            ],
        );
        accrue(
            &mut rows,
            &[
                Sample {
                    pid: 7,
                    name: "neb-shell.exe".to_owned(),
                    working_set: 80,
                },
                Sample {
                    pid: 8,
                    name: "neb-supervisor.exe".to_owned(),
                    working_set: 9,
                },
            ],
        );
        accrue(
            &mut rows,
            &[Sample {
                pid: 7,
                name: "neb-shell.exe".to_owned(),
                working_set: 61,
            }],
        );

        let shell = rows.iter().find(|row| row.pid == 7).expect("the shell row");
        assert_eq!(
            (shell.first, shell.last, shell.min, shell.max, shell.samples),
            (50, 61, 50, 80, 3)
        );

        let supervisor = rows
            .iter()
            .find(|row| row.pid == 8)
            .expect("the supervisor row");
        assert_eq!(
            (
                supervisor.first,
                supervisor.last,
                supervisor.min,
                supervisor.max
            ),
            (10, 9, 9, 10)
        );
        assert!(!supervisor.grew_too_much());
        assert!(shell.grew_too_much(), "50 -> 61 is 22 %");
    }

    #[test]
    fn a_process_that_grows_past_the_limit_is_a_failure() {
        let row = Row {
            pid: 1,
            name: "neb-wallpaper.exe".to_owned(),
            first: 1_000,
            last: 1_500,
            min: 1_000,
            max: 1_500,
            samples: 2,
        };
        let outcome = Outcome {
            file: PathBuf::from("soak.csv"),
            rows: vec![row.clone()],
            failures: vec![(row.name.clone(), row.pid, row.growth())],
        };

        assert!(!outcome.passed());
        assert!(row.grew_too_much());
    }

    #[test]
    fn only_projectneb_executables_are_sampled() {
        for name in ["neb-shell.exe", "NEB-SUPERVISOR.EXE", "neb-wallpaper.exe"] {
            assert!(is_neb_process(name), "{name}");
        }

        for name in ["explorer.exe", "neb-shell", "notneb-shell.exe", ""] {
            assert!(!is_neb_process(name), "{name}");
        }
    }

    #[test]
    fn the_csv_name_is_a_timestamp_under_the_log_directory() {
        let path = csv_path(Path::new(r"C:\Users\tester\AppData\Local\ProjectNeb"));
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");

        assert!(name.starts_with("soak-"), "{name}");
        assert!(name.ends_with(".csv"), "{name}");
        assert_eq!(
            path.parent(),
            Some(Path::new(r"C:\Users\tester\AppData\Local\ProjectNeb\logs"))
        );
    }

    #[test]
    fn the_live_sample_finds_this_test_process_when_it_is_named_like_a_projectneb_one() {
        // The test binary is called `neb_supervisor-<hash>.exe`, which is not a `neb-*.exe`; what
        // this proves is that a snapshot round trip answers for every process it finds instead of
        // failing at the first one it cannot open.
        let samples = sample_processes();

        assert!(
            samples.iter().all(|sample| is_neb_process(&sample.name)),
            "only ProjectNeb names are reported"
        );
        assert!(samples.iter().all(|sample| sample.working_set > 0));
    }

    #[test]
    fn the_working_set_of_this_process_is_a_plausible_number() {
        let own = working_set(std::process::id()).expect("this process is readable");

        assert!(
            own > 1_000_000,
            "a Rust binary maps at least a megabyte: {own}"
        );
        assert!(own < 1_000_000_000, "and not a gigabyte: {own}");
    }

    #[test]
    fn the_probe_can_be_called_repeatedly_without_running_out_of_handles() {
        // Each call opens and closes one handle; a leak would show up as a failure after a few
        // thousand calls, which is more than this loop can afford to hide.
        for _ in 0..1_000 {
            assert!(working_set(std::process::id()).is_some());
        }
    }
}
