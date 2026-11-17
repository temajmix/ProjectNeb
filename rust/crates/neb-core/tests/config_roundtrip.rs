//! Proves a configuration that the C# build wrote survives this crate: read it, rewrite it, read it
//! again, and nothing the user (or a newer build) put in the file is lost.
//!
//! The test uses the real `%LocalAppData%\ProjectNeb\config.json` when this machine has one — that is
//! the file the acceptance criterion names — copied into a temporary directory so the user's file is
//! never written. Without a real file the same path is exercised from defaults.

use std::fs;

use neb_core::config::{Config, ConfigStore, SCHEMA_VERSION};
use serde_json::{Value, json};
use tempfile::tempdir;

/// The real config file, if this machine has one.
fn real_config() -> Option<std::path::PathBuf> {
    let path = neb_core::paths::config_file();
    path.is_file().then_some(path)
}

/// Parses the file the way both builds do: UTF-8, a byte-order mark tolerated.
fn parse(bytes: &[u8]) -> Value {
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);

    serde_json::from_slice(bytes).expect("the config must be valid json")
}

#[test]
fn a_config_written_by_the_c_sharp_build_round_trips() {
    let dir = tempdir().expect("temp dir");
    let copy = dir.path().join("config.json");

    let original: Value = match real_config() {
        Some(path) => {
            fs::copy(&path, &copy).expect("copy the real config");
            parse(&fs::read(&copy).expect("read the copy"))
        }
        None => {
            eprintln!(
                "no config at {}; exercising defaults instead",
                neb_core::paths::config_file().display()
            );
            ConfigStore::new(&copy)
                .save(&Config::default())
                .expect("seed defaults");
            parse(&fs::read(&copy).expect("read the seeded file"))
        }
    };

    // One read/write cycle through this crate, as a save from any ProjectNeb process would do.
    let store = ConfigStore::new(&copy);
    let loaded = store.load();
    store.save(&loaded).expect("rewrite the config");

    let rewritten_bytes = fs::read(&copy).expect("read the rewritten file");
    let rewritten = parse(&rewritten_bytes);

    // The C# writer emits a byte-order mark and no trailing newline guarantee; this writer does the
    // opposite, and `File.ReadAllText` on the other side tolerates either.
    assert!(
        !rewritten_bytes.starts_with(&[0xEF, 0xBB, 0xBF]),
        "the rewritten file must not carry a byte-order mark"
    );
    assert!(
        rewritten_bytes.ends_with(b"\n"),
        "the rewritten file must end with a newline"
    );
    assert!(
        std::str::from_utf8(&rewritten_bytes).is_ok(),
        "the rewritten file must be utf-8"
    );

    // Every key and value the original had is still there, with the same meaning.
    assert_preserved(&original, &rewritten, "$");

    // The shape the C# reader deserialises.
    assert_eq!(rewritten["schemaVersion"], json!(SCHEMA_VERSION));
    assert!(rewritten["language"].is_string(), "{rewritten}");
    assert!(rewritten["theme"]["name"].is_string(), "{rewritten}");
    for module in ["wallpapers", "zapret", "windhawk", "discordRp"] {
        assert!(
            rewritten["modules"][module].is_boolean(),
            "modules.{module} must be a boolean: {rewritten}"
        );
    }
    assert!(rewritten["window"]["width"].is_number(), "{rewritten}");
    assert!(rewritten["window"]["height"].is_number(), "{rewritten}");
    assert!(rewritten["window"]["maximized"].is_boolean(), "{rewritten}");
    assert!(
        rewritten["wallpapers"]["enabled"].is_boolean(),
        "{rewritten}"
    );
    assert!(rewritten["wallpapers"]["kind"].is_string(), "{rewritten}");
    assert!(rewritten["wallpapers"]["color"].is_string(), "{rewritten}");
    assert!(
        rewritten["wallpapers"]["playlist"].is_array(),
        "{rewritten}"
    );
    assert!(
        rewritten["wallpapers"]["rotateMinutes"].is_number(),
        "{rewritten}"
    );

    // And the rewrite is lossless through this crate's own reader.
    assert_eq!(store.load(), loaded, "save/load must be lossless");
}

#[test]
fn defaults_round_trip_into_the_shape_the_c_sharp_build_reads() {
    let dir = tempdir().expect("temp dir");
    let store = ConfigStore::new(dir.path().join("config.json"));

    store.save(&Config::default()).expect("save");
    let saved = parse(&fs::read(store.path()).expect("read"));

    assert_eq!(saved["schemaVersion"], json!(SCHEMA_VERSION));
    assert_eq!(saved["language"], json!("ru"));
    assert_eq!(saved["theme"]["name"], json!("Dark"));
    assert_eq!(saved["wallpapers"]["kind"], json!("Color"));
    // The C# writer spells a `double` holding 1280 as `1280`, so the value is what is asserted here,
    // not the JSON spelling.
    assert_eq!(saved["window"]["width"].as_f64(), Some(1280.0));
    assert!(saved["window"]["left"].is_null());

    assert_eq!(store.load(), Config::default());
}

/// Asserts that everything in `original` is present and equal in `rewritten`.
///
/// Numbers are compared by value, because JSON does not distinguish `1280` from `1280.0` in meaning
/// and the two builds disagree on which they write for the same field.
fn assert_preserved(original: &Value, rewritten: &Value, at: &str) {
    match original {
        Value::Object(fields) => {
            let Value::Object(after) = rewritten else {
                panic!("{at} is no longer an object: {rewritten}");
            };

            for (key, value) in fields {
                let Some(after_value) = after.get(key) else {
                    panic!("{at}.{key} was dropped by the rewrite");
                };
                assert_preserved(value, after_value, &format!("{at}.{key}"));
            }
        }
        Value::Array(items) => {
            let Value::Array(after) = rewritten else {
                panic!("{at} is no longer an array: {rewritten}");
            };

            assert_eq!(items.len(), after.len(), "{at} changed length");
            for (index, (item, after_item)) in items.iter().zip(after).enumerate() {
                assert_preserved(item, after_item, &format!("{at}[{index}]"));
            }
        }
        Value::Number(number) => {
            let before = number.as_f64().expect("a finite number");
            let after = rewritten
                .as_f64()
                .unwrap_or_else(|| panic!("{at} is no longer a number: {rewritten}"));

            assert!(
                (before - after).abs() < f64::EPSILON,
                "{at} changed from {before} to {after}"
            );
        }
        other => assert_eq!(other, rewritten, "{at} changed"),
    }
}

#[test]
fn the_preservation_check_notices_a_dropped_field() {
    let original = json!({ "a": 1, "b": { "с": true } });
    let rewritten = json!({ "a": 1.0, "b": {} });

    let outcome = std::panic::catch_unwind(|| {
        assert_preserved(&original, &rewritten, "$");
    });

    assert!(outcome.is_err(), "a dropped nested key must be noticed");
}
