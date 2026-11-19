//! `neb-cli themes …`: the Theme Manager, from a shell.
//!
//! `list` prints the six compiled-in presets (and any user themes), `export` writes one package as
//! JSON, and `apply` sets `shell.themeId` through [`ThemeStore::apply_to_config`] and asks a running
//! shell to reload. The last one exists mostly for diagnostics and for the acceptance screenshots;
//! the settings window uses the same core API.

use std::time::Duration;

use neb_core::config::ConfigStore;
use neb_core::theming::{ThemeStore, is_builtin};
use serde_json::json;

use super::{CliError, to_json, write_json, write_line};
use crate::args::Options;

/// Pipe the resident shell answers on.
const SHELL_PIPE: &str = "ProjectNeb.shell";

/// How long `apply` waits for the shell to acknowledge the reload.
const RELOAD_TIMEOUT: Duration = Duration::from_millis(750);

/// Runs `neb-cli themes <subcommand>`.
pub fn run(args: &[String]) -> Result<(), CliError> {
    let options = Options::parse(args)?;

    let Some(subcommand) = options.positional.first().map(String::as_str) else {
        return Err(CliError::Usage(
            "themes needs one of: list, export <id>, apply <id>".to_owned(),
        ));
    };

    match subcommand {
        "list" => list(&options),
        "export" => export(&options),
        "apply" => apply(&options),
        other => Err(CliError::Usage(format!(
            "unknown themes command '{other}': expected list, export or apply"
        ))),
    }
}

/// Prints every theme the settings window would offer.
fn list(options: &Options) -> Result<(), CliError> {
    if options.positional.len() > 1 {
        return Err(CliError::Usage("themes list takes no arguments".to_owned()));
    }

    let store = ThemeStore::at_default_location();
    let themes = store.list();

    if options.json {
        let list: Vec<serde_json::Value> = themes
            .iter()
            .map(|theme| {
                json!({
                    "id": theme.id,
                    "name": theme.name,
                    "description": theme.description,
                    "author": theme.author,
                    "builtIn": is_builtin(&theme.id),
                })
            })
            .collect();

        return write_json(&json!({ "themes": list }));
    }

    for theme in themes {
        write_line(&format!(
            "{:<14} {:<18} {}",
            theme.id,
            theme.name,
            if is_builtin(&theme.id) {
                "built-in"
            } else {
                "user"
            }
        ))?;
    }

    Ok(())
}

/// Prints one theme as an importable package.
fn export(options: &Options) -> Result<(), CliError> {
    let id = operand(&options.positional, "export")?;
    let store = ThemeStore::at_default_location();
    let text = store
        .export(id)
        .map_err(|error| CliError::Failed(error.to_string()))?;

    write_line(text.trim_end_matches('\n'))
}

/// Applies a theme to the config and asks the shell to repaint.
fn apply(options: &Options) -> Result<(), CliError> {
    let id = operand(&options.positional, "apply")?;
    let store = ThemeStore::at_default_location();
    let config_store = ConfigStore::at_default_location();
    let config = store
        .apply_to_config(id, &config_store)
        .map_err(|error| CliError::Failed(error.to_string()))?;

    let reload = neb_core::ipc::call(SHELL_PIPE, "shell.reload", None, RELOAD_TIMEOUT);
    let reloaded = reload.is_ok();

    if options.json {
        let value = to_json(&config)?;
        return write_json(&json!({
            "themeId": id,
            "reloaded": reloaded,
            "config": value,
        }));
    }

    match reload {
        Ok(_) => write_line(&format!("applied '{id}' and reloaded the shell")),
        Err(error) => write_line(&format!(
            "applied '{id}'; the shell did not answer ({error}), so it will pick the theme up on \
             the next reload or start"
        )),
    }
}

/// Reads `<subcommand> <id>`, refusing a missing or extra operand.
fn operand<'a>(positional: &'a [String], subcommand: &str) -> Result<&'a str, CliError> {
    let Some(id) = positional.get(1).map(String::as_str) else {
        return Err(CliError::Usage(format!(
            "themes {subcommand} needs a theme id"
        )));
    };

    if positional.len() > 2 {
        return Err(CliError::Usage(format!(
            "themes {subcommand} takes exactly one theme id"
        )));
    }

    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn a_missing_subcommand_is_a_usage_error() {
        match run(&[]) {
            Err(CliError::Usage(message)) => assert!(message.contains("list")),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_subcommand_is_refused_by_name() {
        match run(&argv(&["frobnicate"])) {
            Err(CliError::Usage(message)) => assert!(message.contains("frobnicate")),
            other => panic!("expected a usage error, got {other:?}"),
        }
    }

    #[test]
    fn export_needs_one_id_and_no_more() {
        assert!(matches!(run(&argv(&["export"])), Err(CliError::Usage(_))));
        assert!(matches!(
            run(&argv(&["export", "one", "two"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn list_takes_no_operands() {
        assert!(matches!(
            run(&argv(&["list", "extra"])),
            Err(CliError::Usage(_))
        ));
    }
}
