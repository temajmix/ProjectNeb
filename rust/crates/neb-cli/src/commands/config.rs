//! `neb-cli config …`: the configuration as the app sees it.
//!
//! `dump` prints the JSON the C# build reads, produced by the same types and the same store, so a
//! field the app would lose on its next save shows up here first. `path` answers "where is it", and
//! `validate` says whether the file on disk can be read at all.

use std::fs;

use neb_core::config::{Config, ConfigStore, SCHEMA_VERSION};
use serde_json::json;

use super::{CliError, to_json, write_json, write_json_pretty, write_line};
use crate::args::Options;

/// Runs `neb-cli config <subcommand>`.
pub fn run(args: &[String]) -> Result<(), CliError> {
    let options = Options::parse(args)?;

    let Some(subcommand) = options.positional.first().map(String::as_str) else {
        return Err(CliError::Usage(
            "config needs one of: dump, path, validate".to_owned(),
        ));
    };

    if options.positional.len() > 1 {
        return Err(CliError::Usage(format!(
            "config {subcommand} takes no arguments"
        )));
    }

    match subcommand {
        "dump" => dump(&options),
        "path" => path(&options),
        "validate" => validate(&options),
        other => Err(CliError::Usage(format!(
            "unknown config command '{other}': expected dump, path or validate"
        ))),
    }
}

/// Prints the configuration the app would load.
///
/// `--json` prints one compact line; without it the same document is indented. Either way the output
/// is only JSON, so it can be piped into a reader — including the C# app.
fn dump(options: &Options) -> Result<(), CliError> {
    let config = ConfigStore::at_default_location().load();
    let value = to_json(&config)?;

    if options.json {
        write_json(&value)
    } else {
        write_json_pretty(&value)
    }
}

/// Prints the config file's location.
fn path(options: &Options) -> Result<(), CliError> {
    let config_path = ConfigStore::at_default_location().path().to_path_buf();

    if options.json {
        return write_json(&json!({ "path": config_path.display().to_string() }));
    }

    write_line(&config_path.display().to_string())
}

/// Reads the file from disk and reports every problem found.
///
/// A missing file is not a problem: a fresh installation starts on defaults. A file that cannot be
/// parsed is, because the app would silently replace it with defaults on the next save.
fn validate(options: &Options) -> Result<(), CliError> {
    let config_path = ConfigStore::at_default_location().path().to_path_buf();
    let exists = config_path.is_file();
    let mut problems: Vec<String> = Vec::new();
    let mut config = Config::default();
    let mut source = "defaults";

    if exists {
        match fs::read(&config_path) {
            Ok(bytes) => match std::str::from_utf8(strip_bom(&bytes)) {
                Ok(text) => match serde_json::from_str::<Config>(text) {
                    Ok(loaded) => {
                        config = loaded;
                        source = "file";
                    }
                    Err(error) => problems.push(format!("cannot parse the file: {error}")),
                },
                Err(error) => problems.push(format!("the file is not utf-8: {error}")),
            },
            Err(error) => {
                problems.push(format!("cannot read '{}': {error}", config_path.display()))
            }
        }
    }

    problems.extend(check(&config).into_iter().map(|problem| problem.to_owned()));
    let valid = problems.is_empty();

    if options.json {
        write_json(&json!({
            "path": config_path.display().to_string(),
            "exists": exists,
            "source": source,
            "schemaVersion": config.schema_version,
            "language": config.language,
            "valid": valid,
            "problems": problems,
        }))?;
    } else {
        write_line(&format!("path:   {}", config_path.display()))?;
        write_line(&format!("source: {source}"))?;
        if valid {
            write_line("ok")?;
        } else {
            for problem in &problems {
                write_line(&format!("error: {problem}"))?;
            }
        }
    }

    if valid {
        Ok(())
    } else {
        Err(CliError::Failed(format!(
            "the config has {} problem(s)",
            problems.len()
        )))
    }
}

/// Everything wrong with an in-memory config, each entry phrased for the user.
pub(crate) fn check(config: &Config) -> Vec<&'static str> {
    let mut problems = Vec::new();

    if config.schema_version != SCHEMA_VERSION {
        problems.push("schemaVersion is not the version this build writes");
    }
    if config.language.trim().is_empty() {
        problems.push("language is empty");
    }
    if !is_positive(config.window.width) || !is_positive(config.window.height) {
        problems.push("window.width and window.height must be positive numbers");
    }
    if !is_finite(config.window.left) || !is_finite(config.window.top) {
        problems.push("window.left and window.top must be finite numbers when set");
    }
    if config.wallpapers.rotate_minutes < 1 {
        problems.push("wallpapers.rotateMinutes must be at least 1");
    }
    if !is_hex_color(&config.wallpapers.color) {
        problems.push("wallpapers.color must be a #RRGGBB value");
    }
    if config.wallpapers.path.as_deref().is_some_and(str::is_empty) {
        problems.push("wallpapers.path is empty");
    }
    if config.wallpapers.playlist.iter().any(String::is_empty) {
        problems.push("wallpapers.playlist contains an empty entry");
    }

    problems
}

/// Whether `value` is present, finite and greater than zero.
fn is_positive(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

/// Whether an optional coordinate is absent or finite.
fn is_finite(value: Option<f64>) -> bool {
    value.is_none_or(f64::is_finite)
}

/// Whether `value` is a `#RRGGBB` colour.
fn is_hex_color(value: &str) -> bool {
    value.len() == 7
        && value.starts_with('#')
        && value[1..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Drops a UTF-8 byte-order mark, which the C# writer leaves on the file.
fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

/// Renders a config as the JSON value `dump` prints; exposed for the tests.
#[cfg(test)]
pub(crate) fn dump_value(config: &Config) -> serde_json::Value {
    serde_json::to_value(config).expect("a config is always serialisable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use neb_core::config::{SceneKind, WallpaperSettings};

    #[test]
    fn a_default_config_has_no_problems() {
        assert_eq!(check(&Config::default()), Vec::<&str>::new());
    }

    #[test]
    fn a_schema_version_from_another_build_is_reported() {
        let config = Config {
            schema_version: SCHEMA_VERSION + 1,
            ..Config::default()
        };

        assert!(
            check(&config)
                .iter()
                .any(|problem| problem.contains("schemaVersion"))
        );
    }

    #[test]
    fn broken_numbers_are_reported() {
        let mut config = Config::default();
        config.window.width = 0.0;
        config.window.height = f64::NAN;
        config.window.left = Some(f64::INFINITY);

        let problems = check(&config);

        assert!(problems.iter().any(|problem| problem.contains("width")));
        assert!(problems.iter().any(|problem| problem.contains("finite")));
    }

    #[test]
    fn wallpapers_are_checked_too() {
        let config = Config {
            wallpapers: WallpaperSettings {
                kind: SceneKind::Color,
                color: "purple".to_owned(),
                playlist: vec![String::new()],
                rotate_minutes: 0,
                path: Some(String::new()),
                ..WallpaperSettings::default()
            },
            ..Config::default()
        };

        let problems = check(&config);

        assert!(problems.iter().any(|problem| problem.contains("#RRGGBB")));
        assert!(
            problems
                .iter()
                .any(|problem| problem.contains("rotateMinutes"))
        );
        assert!(problems.iter().any(|problem| problem.contains("playlist")));
        assert!(problems.iter().any(|problem| problem.contains("path")));
    }

    #[test]
    fn colours_are_checked_as_hex_triplets() {
        for valid in ["#141021", "#AABBCC", "#000000"] {
            assert!(is_hex_color(valid), "{valid}");
        }
        for invalid in ["141021", "#14102", "#1410211", "#GGGGGG", "", "紫色色色色"] {
            assert!(!is_hex_color(invalid), "{invalid}");
        }
    }

    #[test]
    fn dumping_renders_the_c_sharp_shape() {
        let value = dump_value(&Config::default());

        assert_eq!(value["schemaVersion"], json!(SCHEMA_VERSION));
        assert_eq!(value["theme"]["name"], json!("Dark"));
        assert!(value["wallpapers"]["playlist"].is_array());
        assert!(value["window"]["left"].is_null());
    }
}
