//! End-to-end tests of the copy and of the shipped `neb-updater.exe`.
//!
//! The unit tests next to each module cover the pure decisions (the plan, the retry classification,
//! the quoting). These tests make the same facts physical: real directories, a real sharing violation,
//! a real short-lived process, and the real executable invoked the way the supervisor invokes it.

use std::fs::{self, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use neb_updater::apply::{self, CopyStep, RetryPolicy};
use neb_updater::log::Log;
use neb_updater::process::{self, WaitOutcome};
use tempfile::TempDir;

/// Creates `directory` and writes `contents` into `name` under it.
fn write(directory: &Path, name: &str, contents: &[u8]) -> PathBuf {
    let path = directory.join(name);
    fs::create_dir_all(path.parent().expect("a parent")).expect("create the directory");
    fs::write(&path, contents).expect("write the file");
    path
}

/// A staging and an installation directory under one temporary root.
fn trees() -> (TempDir, PathBuf, PathBuf) {
    let root = tempfile::tempdir().expect("temp dir");
    let staging = root.path().join("staging");
    let app = root.path().join("app");
    fs::create_dir_all(&staging).expect("staging");
    fs::create_dir_all(&app).expect("app");
    (root, staging, app)
}

#[test]
fn an_update_copies_the_staged_tree_byte_for_byte_and_deletes_nothing() {
    let (root, staging, app) = trees();
    write(&staging, "neb-shell.exe", b"the new shell");
    write(&staging, r"themes\dark.json", b"{\"accent\":\"#141021\"}");
    write(&staging, r"plugins\new.dll", b"new plugin");
    // A file the installation has and the archive does not: the updater must keep it.
    let keep = write(&app, "keep.txt", b"the user's notes");
    write(&app, "neb-shell.exe", b"the running shell");

    let steps = apply::plan(&staging, &app).expect("the tree is readable");
    assert_eq!(
        steps,
        vec![
            CopyStep {
                source: staging.join("neb-shell.exe"),
                destination: app.join("neb-shell.exe"),
            },
            CopyStep {
                source: staging.join("plugins").join("new.dll"),
                destination: app.join("plugins").join("new.dll"),
            },
            CopyStep {
                source: staging.join("themes").join("dark.json"),
                destination: app.join("themes").join("dark.json"),
            },
        ],
        "the plan is ordered and pairs every staged file"
    );

    let log = Log::open(&root.path().join("update.log")).expect("open the log");
    let summary = apply::apply(&steps, RetryPolicy::default(), &log).expect("the copy succeeds");

    assert_eq!(summary.files, 3);
    assert_eq!(
        summary.bytes,
        (b"the new shell".len() + b"new plugin".len() + b"{\"accent\":\"#141021\"}".len()) as u64
    );
    assert_eq!(summary.retries, 0);

    assert_eq!(
        fs::read(app.join("neb-shell.exe")).expect("read"),
        b"the new shell"
    );
    assert_eq!(
        fs::read(app.join("plugins").join("new.dll")).expect("read"),
        b"new plugin"
    );
    assert_eq!(
        fs::read(app.join("themes").join("dark.json")).expect("read"),
        b"{\"accent\":\"#141021\"}"
    );
    assert_eq!(
        fs::read(&keep).expect("read"),
        b"the user's notes",
        "a file the archive does not contain is never deleted"
    );

    let text = fs::read_to_string(log.path()).expect("read the log");
    assert!(
        text.contains(" INFO copy ") && text.contains("neb-shell.exe"),
        "the log names every copied file: {text}"
    );
}

#[test]
fn a_locked_destination_is_retried_until_the_handle_goes_away() {
    let (root, staging, app) = trees();
    write(&staging, "neb-shell.exe", b"the new shell");
    let destination = write(&app, "neb-shell.exe", b"the running shell");

    // Exactly what a dying child does: a handle that shares nothing, released a moment later.
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(0)
        .open(&destination)
        .expect("hold the destination");
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        drop(held);
    });

    let steps = apply::plan(&staging, &app).expect("plan");
    let log = Log::open(&root.path().join("update.log")).expect("open the log");
    let summary = apply::apply(
        &steps,
        RetryPolicy {
            attempts: 20,
            delay: Duration::from_millis(50),
        },
        &log,
    )
    .expect("a lock that goes away is not a failure");

    release.join().expect("the holder must not panic");

    assert!(
        summary.retries > 0,
        "the first attempt must observe the lock, got {summary:?}"
    );
    assert_eq!(
        fs::read(&destination).expect("read"),
        b"the new shell",
        "the retried copy wrote the staged bytes"
    );
    let text = fs::read_to_string(log.path()).expect("read the log");
    assert!(
        text.contains(" WARN copy ") && text.contains("retry 1/20"),
        "the retry is in the log: {text}"
    );
}

#[test]
fn waiting_for_a_short_lived_child_returns_as_soon_as_it_exits() {
    let mut child = Command::new("cmd")
        .args(["/c", "ping", "-n", "2", "127.0.0.1"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("start a short-lived child");
    let pid = child.id();

    let started = Instant::now();
    let outcome = process::wait_for_pid(pid, Duration::from_secs(10)).expect("the wait works");
    let waited = started.elapsed();

    assert_eq!(outcome, WaitOutcome::Exited);
    assert!(
        waited >= Duration::from_millis(200),
        "the wait must actually cover the child's life, it took {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(10),
        "a child that exits must not be waited out to the timeout, it took {waited:?}"
    );

    let status = child.wait().expect("reap the child");
    assert!(
        status.success(),
        "the ping child is expected to succeed, got {status}"
    );
}

#[test]
fn the_binary_waits_copies_and_restarts_with_a_default_log_file() {
    let (root, staging, app) = trees();
    write(&staging, "neb-shell.exe", b"the new shell");
    write(&app, "neb-shell.exe", b"the running shell");

    let mut waited = Command::new("cmd")
        .args(["/c", "exit", "0"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the child that has to exit first");
    let pid = waited.id();
    waited.wait().expect("reap it");

    let status = Command::new(env!("CARGO_BIN_EXE_neb-updater"))
        .arg("--wait-pid")
        .arg(pid.to_string())
        .arg("--staging")
        .arg(&staging)
        .arg("--app")
        .arg(&app)
        .arg("--restart")
        .arg("cmd.exe")
        .arg("--restart-arg")
        .arg("/c")
        .arg("--restart-arg")
        .arg("exit")
        .arg("--timeout-secs")
        .arg("10")
        .status()
        .expect("the updater must start");

    assert_eq!(status.code(), Some(0), "the run succeeds");
    assert_eq!(
        fs::read(app.join("neb-shell.exe")).expect("read"),
        b"the new shell",
        "the staged file replaced the old one"
    );

    let logs = root.path().join("logs");
    let file = fs::read_dir(&logs)
        .expect("the default log directory exists")
        .map(|entry| entry.expect("entry").path())
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("updater-") && name.ends_with(".log"))
        })
        .expect("a default updater-<unix>.log was written");

    let text = fs::read_to_string(&file).expect("read the log");
    for step in ["start", "wait", "plan", "copy", "restart", "done"] {
        assert!(
            text.contains(&format!(" INFO {step} ")) || text.contains(&format!(" WARN {step} ")),
            "the log has one line for '{step}': {text}"
        );
    }
    for line in text.lines() {
        let stamp = line.split_once(' ').expect("a stamp").0;
        assert!(stamp.ends_with('Z'), "timestamps are UTC: {line}");
        assert_eq!(&stamp[4..5], "-", "{line}");
        assert_eq!(&stamp[10..11], "T", "{line}");
    }
}

#[test]
fn the_binary_exits_2_and_copies_nothing_when_the_pid_outlives_a_zero_timeout() {
    let (root, staging, app) = trees();
    write(&staging, "neb-shell.exe", b"the new shell");
    let log = root.path().join("timeout.log");

    let status = Command::new(env!("CARGO_BIN_EXE_neb-updater"))
        .arg("--wait-pid")
        // This test process is alive for as long as the assertion below runs.
        .arg(std::process::id().to_string())
        .arg("--staging")
        .arg(&staging)
        .arg("--app")
        .arg(&app)
        .arg("--restart")
        .arg("cmd.exe")
        .arg("--log")
        .arg(&log)
        .arg("--timeout-secs")
        .arg("0")
        .status()
        .expect("the updater must start");

    assert_eq!(status.code(), Some(2), "a timeout is its own exit code");
    assert!(
        !app.join("neb-shell.exe").exists(),
        "the installation must not be touched when the wait fails"
    );
    let text = fs::read_to_string(&log).expect("read the log");
    assert!(text.contains(" ERROR wait "), "{text}");
    assert!(text.contains("nothing was copied"), "{text}");
}
