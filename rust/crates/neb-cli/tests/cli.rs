//! End-to-end tests of the shipped `neb-cli` binary.
//!
//! Each test runs the real executable in a child process. Commands that read or write the config get a
//! private data root through `NEB_HOME`, so the developer's own ProjectNeb directory is never touched;
//! `ping` talks to a real named-pipe server running in the test process, which makes the pipe, the
//! framing and the timeout path cross-process facts rather than unit-test claims.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use neb_core::config::ConfigStore;
use neb_core::ipc::{Request, Response, serve};
use serde_json::{Value, json};
use tempfile::TempDir;

/// The binary cargo just built.
fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_neb-cli"))
}

/// Runs the CLI, optionally with a private data root as its `NEB_HOME`.
fn run(home: Option<&Path>, args: &[&str]) -> Output {
    let mut command = command();
    command.args(args);

    if let Some(root) = home {
        command.env("NEB_HOME", root);
    }

    command.output().expect("the cli must start")
}

/// Parses stdout as JSON, failing with the process's own words.
fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "stdout is not json ({error}): {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn stderr_text(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// A private data root (`NEB_HOME`) and the config path inside it.
fn isolated() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("temp dir");
    let config = dir.path().join("config.json");
    (dir, config)
}

/// Seeds a config the C# build could have written: a BOM, unknown keys, cyrillic paths.
fn seed_c_sharp_config(path: &Path) {
    fs::create_dir_all(path.parent().expect("parent")).expect("create the directory");
    let text = r##"{
  "schemaVersion": 1,
  "language": "ru",
  "theme": { "name": "Dark" },
  "modules": { "wallpapers": true, "zapret": false, "windhawk": false, "discordRp": false },
  "window": { "left": 223, "top": 64, "width": 1440, "height": 900, "maximized": false },
  "wallpapers": {
    "enabled": true,
    "kind": "Image",
    "path": "C:\\Users\\tester\\Рисунки\\обои.png",
    "color": "#141021",
    "playlist": [],
    "rotateMinutes": 30,
    "pauseOnFullscreen": true,
    "pauseOnBattery": true
  },
  "futureFlag": true
}"##;
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(text.as_bytes());
    fs::write(path, bytes).expect("seed the config");
}

#[test]
fn config_dump_prints_readable_json_and_keeps_unknown_keys() {
    let (dir, config) = isolated();
    seed_c_sharp_config(&config);

    let output = run(Some(dir.path()), &["config", "dump"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let value = stdout_json(&output);

    assert_eq!(value["language"], json!("ru"));
    assert_eq!(value["window"]["width"], json!(1440.0));
    assert_eq!(
        value["wallpapers"]["path"],
        json!(r"C:\Users\tester\Рисунки\обои.png")
    );
    assert_eq!(value["futureFlag"], json!(true), "unknown keys are kept");
    assert!(
        String::from_utf8_lossy(&output.stdout).ends_with('\n'),
        "the output ends with a newline"
    );
}

#[test]
fn neb_home_wins_over_a_stale_local_app_data() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stale = PathBuf::from(r"C:\Users\stale\AppData\Local");

    let output = command()
        .args(["config", "path"])
        .env("NEB_HOME", dir.path())
        .env("LOCALAPPDATA", &stale)
        .output()
        .expect("the cli must start");

    assert!(output.status.success(), "{}", stderr_text(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        dir.path().join("config.json").display().to_string(),
        "NEB_HOME is the data root itself and must win over the variable"
    );
}

#[test]
fn a_stale_local_app_data_does_not_move_the_config() {
    let stale = PathBuf::from(r"C:\Users\stale\AppData\Local");

    let output = command()
        .args(["config", "path"])
        .env_remove("NEB_HOME")
        .env("LOCALAPPDATA", &stale)
        .output()
        .expect("the cli must start");

    assert!(output.status.success(), "{}", stderr_text(&output));
    let printed = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    assert!(
        !printed.contains("stale"),
        "the shell's known folder must win over a stale variable: {printed}"
    );
    assert!(
        printed.ends_with(r"ProjectNeb\config.json"),
        "the config hangs off the shell's local app data: {printed}"
    );
}

#[test]
fn config_dump_json_is_a_single_compact_document() {
    let (dir, config) = isolated();
    seed_c_sharp_config(&config);

    let output = run(Some(dir.path()), &["config", "dump", "--json"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert_eq!(text.lines().count(), 1, "one line of json: {text}");
    let value = stdout_json(&output);
    assert_eq!(value["schemaVersion"], json!(1));
}

#[test]
fn config_dump_of_a_missing_file_prints_defaults() {
    let (dir, _config) = isolated();

    let output = run(Some(dir.path()), &["config", "dump", "--json"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let value = stdout_json(&output);
    assert_eq!(value["schemaVersion"], json!(1));
    assert_eq!(value["theme"]["name"], json!("Dark"));
}

#[test]
fn config_path_names_the_configuration_file() {
    let (dir, config) = isolated();

    let output = run(Some(dir.path()), &["config", "path"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        config.display().to_string()
    );
}

#[test]
fn config_validate_accepts_a_good_file_and_reports_a_broken_one() {
    let (dir, config) = isolated();
    seed_c_sharp_config(&config);

    let good = run(Some(dir.path()), &["config", "validate"]);
    assert!(good.status.success(), "{}", stderr_text(&good));
    assert!(String::from_utf8_lossy(&good.stdout).contains("ok"));

    fs::write(&config, r#"{"schemaVersion": 1, "window": {"width": 0}}"#)
        .expect("write a broken config");
    let bad = run(Some(dir.path()), &["config", "validate", "--json"]);

    assert!(!bad.status.success(), "a broken config must exit 1");
    let value = stdout_json(&bad);
    assert_eq!(value["valid"], json!(false));
    assert!(
        value["problems"]
            .as_array()
            .expect("problems are a list")
            .iter()
            .any(|problem| problem.as_str().unwrap_or_default().contains("width")),
        "{value}"
    );
    assert!(
        stderr_text(&bad).contains("problem"),
        "{}",
        stderr_text(&bad)
    );
}

#[test]
fn metrics_prints_the_requested_number_of_samples() {
    let output = run(
        None,
        &["metrics", "--count", "3", "--interval-ms", "20", "--json"],
    );

    assert!(output.status.success(), "{}", stderr_text(&output));
    let samples = stdout_json(&output);
    let samples = samples.as_array().expect("a list of samples");

    assert_eq!(samples.len(), 3);
    for sample in samples {
        let cpu = sample["cpu"].as_f64().expect("cpu is a number");
        assert!((0.0..=100.0).contains(&cpu), "{sample}");
        assert!(
            sample["ramTotalGb"].as_f64().unwrap_or(0.0) > 0.0,
            "{sample}"
        );
    }
}

#[test]
fn metrics_prints_human_lines_by_default() {
    let output = run(None, &["metrics", "--count", "1", "--interval-ms", "20"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("cpu:"), "{text}");
    assert!(text.contains("ram:"), "{text}");
}

#[test]
fn metrics_reports_a_non_zero_cpu_when_the_machine_is_busy() {
    // Keep every core but one spinning while the child samples, so the interval it measures really
    // contains work. Without load, a correct 0.0% would be indistinguishable from the placeholder
    // the first reading of a process always is.
    let stop = Arc::new(AtomicBool::new(false));
    let hogs: Vec<_> = (0..busy_threads())
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::hint::spin_loop();
                }
            })
        })
        .collect();

    let output = run(
        None,
        &["metrics", "--count", "2", "--interval-ms", "200", "--json"],
    );

    stop.store(true, Ordering::Relaxed);
    for hog in hogs {
        hog.join().expect("a hog must not panic");
    }

    assert!(output.status.success(), "{}", stderr_text(&output));
    let value = stdout_json(&output);
    let samples = value.as_array().expect("a list of samples");

    assert_eq!(samples.len(), 2, "{value}");
    assert!(
        samples
            .iter()
            .any(|sample| sample["cpu"].as_f64().unwrap_or(0.0) > 0.0),
        "with every core but one busy, at least one reported sample must show load: {value}"
    );
}

/// How many cores to keep busy, leaving one for the sampling process.
fn busy_threads() -> usize {
    std::thread::available_parallelism().map_or(1, |count| count.get().saturating_sub(1).max(1))
}

#[test]
fn windows_lists_visible_windows_in_the_documented_format() {
    let output = run(None, &["windows"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        let mut fields = line.splitn(4, ' ');
        assert!(
            fields.next().unwrap_or_default().starts_with("0x"),
            "{line}"
        );
        assert!(fields.next().is_some(), "{line}");
        assert!(fields.next().is_some(), "{line}");
        assert!(fields.next().is_some(), "{line}");
    }
}

#[test]
fn windows_json_is_a_list_of_window_objects() {
    let output = run(None, &["windows", "--json"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let value = stdout_json(&output);
    let windows = value.as_array().expect("a list of windows");

    for window in windows {
        assert!(window["hwnd"].is_number(), "{window}");
        assert!(window["pid"].is_number(), "{window}");
        assert!(window["class"].is_string(), "{window}");
        assert!(
            window["title"]
                .as_str()
                .is_some_and(|title| !title.is_empty()),
            "{window}"
        );
    }
}

#[test]
fn mem_reports_this_processes_working_set() {
    let output = run(None, &["mem", "--json"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let value = stdout_json(&output);
    let bytes = value["workingSetBytes"].as_u64().expect("a byte count");

    assert!(bytes > 0, "{value}");
    assert!(
        bytes < 8 * 1024 * 1024,
        "the cli's working set must stay below the 8 MB budget, got {bytes} bytes"
    );
    assert_eq!(
        value["workingSetMb"],
        json!((bytes as f64 / (1024.0 * 1024.0) * 100.0).round() / 100.0),
        "{value}"
    );
}

#[test]
fn ping_reaches_a_running_server_across_processes() {
    let pipe = format!("ProjectNeb.cli-test.{}", std::process::id());
    let _server = serve(&pipe, |request: Request| {
        Response::success(request.id, json!("pong"))
    })
    .expect("start the server");

    let output = run(None, &["ping", &pipe, "--json"]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    let value = stdout_json(&output);
    assert_eq!(value["ok"], json!(true), "{value}");
    assert_eq!(value["result"], json!("pong"));
    assert_eq!(value["pipe"], json!(pipe));
}

#[test]
fn ping_prints_the_result_on_its_own_line() {
    let pipe = format!("ProjectNeb.cli-text.{}", std::process::id());
    let _server = serve(&pipe, |request: Request| {
        Response::success(request.id, json!("pong"))
    })
    .expect("start the server");

    let output = run(None, &["ping", &pipe]);

    assert!(output.status.success(), "{}", stderr_text(&output));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "pong");
}

#[test]
fn ping_fails_cleanly_when_nobody_listens() {
    let pipe = format!("ProjectNeb.cli-absent.{}", std::process::id());

    let output = run(None, &["ping", &pipe, "--timeout-ms", "300"]);

    assert!(!output.status.success(), "a missing server must exit 1");
    assert!(
        stderr_text(&output).contains("no ipc server"),
        "{}",
        stderr_text(&output)
    );
}

#[test]
fn an_unknown_command_is_a_usage_error() {
    let output = run(None, &["frobnicate"]);

    assert!(!output.status.success());
    assert!(stderr_text(&output).contains("frobnicate"));
    assert!(
        stderr_text(&output).contains("usage"),
        "the usage is printed"
    );
}

#[test]
fn dump_and_store_agree_on_the_same_file() {
    let (dir, config) = isolated();
    seed_c_sharp_config(&config);

    let output = run(Some(dir.path()), &["config", "dump", "--json"]);
    assert!(output.status.success(), "{}", stderr_text(&output));

    let store = ConfigStore::new(&config);
    let loaded = serde_json::to_value(store.load()).expect("serialise");

    assert_eq!(stdout_json(&output), loaded);
}
