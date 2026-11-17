//! Acceptance test for the phase-2 memory budget: an idle bar must stay under 35 MiB.
//!
//! The test runs the **release** binary with the hidden `--report-memory` flag, which draws the bar for
//! ten seconds and prints the working set it settled at *before* shutting anything down, and reads that
//! number back. A debug build's number would mean nothing — it carries no optimisation and twice the
//! bookkeeping — so a checkout without `target/release/neb-shell.exe` fails the test with a message
//! rather than passing on the wrong binary.
//!
//! It is the only test in this crate that reserves the monitor's top edge, for the ten seconds it runs.
//! Set `NEB_SKIP_DESKTOP_TESTS=1` to skip it on a build machine without an interactive session.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::SystemTime;

/// The budget from the phase-2 plan, in bytes.
const BUDGET: usize = 35 * 1024 * 1024;

/// How long the bar idles before it reports; the plan's acceptance run is ten seconds.
const IDLE_SECONDS: u32 = 10;

#[test]
fn an_idle_shell_stays_inside_its_memory_budget() {
    if std::env::var_os("NEB_SKIP_DESKTOP_TESTS").is_some() {
        eprintln!("skipped: NEB_SKIP_DESKTOP_TESTS is set");
        return;
    }

    let binary = release_binary().unwrap_or_else(|| {
        panic!(
            "this test measures the release binary; run `cargo build --release -p neb-shell` first"
        )
    });

    let output = Command::new(&binary)
        .args(["--report-memory", "--seconds", &IDLE_SECONDS.to_string()])
        .output()
        .expect("the shell binary must be runnable");

    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "the shell must exit successfully: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let working_set = parse_working_set(&report)
        .unwrap_or_else(|| panic!("the report must carry the working set, got: {report}"));
    let seconds = parse_seconds(&report)
        .unwrap_or_else(|| panic!("the report must carry how long it idled, got: {report}"));

    assert!(
        seconds >= f64::from(IDLE_SECONDS),
        "the sample must be taken after at least {IDLE_SECONDS} s of idle, got {seconds}:\n{report}"
    );
    assert!(
        report.contains("1 bar(s)"),
        "the sample must be taken with a bar on screen and its worker alive:\n{report}"
    );
    assert!(
        working_set <= BUDGET,
        "an idle bar used {} MiB, over the {} MiB budget:\n{report}",
        working_set / (1024 * 1024),
        BUDGET / (1024 * 1024)
    );
}

/// The release build of the binary under test, next to the debug one cargo points at.
///
/// `None` when it is missing *or older than the sources*: a stale release binary would report a number
/// from a build nobody is reviewing, which is worse than a failing test.
fn release_binary() -> Option<PathBuf> {
    let debug = PathBuf::from(env!("CARGO_BIN_EXE_neb-shell"));
    let target = debug.parent()?.parent()?;
    let release = target.join("release").join(debug.file_name()?);
    let built = release.metadata().ok()?.modified().ok()?;
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let newest = newest_source(&crate_root.join("src"))?;
    let manifest = crate_root
        .join("Cargo.toml")
        .metadata()
        .ok()?
        .modified()
        .ok()?;

    (built >= newest.max(manifest)).then_some(release)
}

/// The newest modification time of the Rust sources below `dir`.
fn newest_source(dir: &Path) -> Option<SystemTime> {
    let mut newest = None;

    for entry in std::fs::read_dir(dir).ok()? {
        let path = entry.ok()?.path();
        let stamp = if path.is_dir() {
            newest_source(&path)
        } else {
            path.metadata().ok().and_then(|meta| meta.modified().ok())
        };

        if let Some(stamp) = stamp {
            newest = Some(newest.map_or(stamp, |current: SystemTime| current.max(stamp)));
        }
    }

    newest
}

/// Reads `(22409216 bytes)` back out of the report.
fn parse_working_set(report: &str) -> Option<usize> {
    let start = report.find("bytes)")?;
    let before = &report[..start];
    let open = before.rfind('(')?;

    before[open + 1..].trim().parse().ok()
}

/// Reads `after 10.1 s idle` back out of the report.
fn parse_seconds(report: &str) -> Option<f64> {
    let rest = report.split_once("after ")?.1;
    let value = rest.split_once(' ').map_or(rest, |(value, _)| value);

    value.parse().ok()
}

#[test]
fn the_parser_reads_the_number_the_shell_prints() {
    let line = "neb-shell: working set 21.4 MiB (22409216 bytes) after 10.1 s idle, 1 bar(s)";

    assert_eq!(parse_working_set(line), Some(22_409_216));
    assert_eq!(parse_seconds(line), Some(10.1));
    assert_eq!(parse_working_set("nothing here"), None);
    assert_eq!(parse_seconds("nothing here"), None);
}
