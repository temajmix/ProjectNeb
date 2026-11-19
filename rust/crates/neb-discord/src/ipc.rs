//! The dashboard's end of `\\.\pipe\ProjectNeb.discord`.
//!
//! Eight methods, all short:
//!
//! | method | effect |
//! |---|---|
//! | `discord.getState` | the current state document |
//! | `discord.connect` | ask the worker to connect now |
//! | `discord.disconnect` | stop and clear the activity |
//! | `discord.listPresets` | every valid preset plus broken files |
//! | `discord.savePreset` | validate and write one preset |
//! | `discord.setPreset` | pin a preset, or `null` to return to the rules |
//! | `discord.getRules` | the armed rules |
//! | `discord.setRules` | validate and persist a rule list |
//!
//! The handler never waits for Discord: `connect`, `disconnect`, `setPreset` and `setRules` post to
//! the worker and answer with the state as it is at that instant. That is what keeps a slow or absent
//! Discord from freezing the settings window.

use std::path::Path;

use neb_core::config::DiscordRule;
use neb_core::ipc::{IpcError, Request, Response, ServerHandle, serve as serve_pipe};
use serde_json::{Value, json};

use crate::presets::Preset;
use crate::service::Handle;

/// Pipe name shared with the supervisor, the dashboard and `neb-cli`.
pub const DISCORD_PIPE: &str = "ProjectNeb.discord";

/// Starts answering on [`DISCORD_PIPE`].
///
/// # Errors
///
/// [`IpcError`] when the pipe cannot be created.
pub fn serve(handle: Handle) -> Result<ServerHandle, IpcError> {
    serve_on(DISCORD_PIPE, handle)
}

/// Starts answering on an explicit pipe, for tests and for a second diagnostic instance.
///
/// # Errors
///
/// [`IpcError`] when the pipe cannot be created.
pub fn serve_on(pipe: &str, handle: Handle) -> Result<ServerHandle, IpcError> {
    serve_pipe(pipe, move |request| dispatch(&handle, request))
}

/// Turns one request into one response.
pub fn dispatch(handle: &Handle, request: Request) -> Response {
    let id = request.id.clone();

    match request.method.as_str() {
        "discord.getState" => Response::success(id, handle.state()),
        "discord.connect" => match handle.connect() {
            Ok(()) => Response::success(id, handle.state()),
            Err(error) => Response::failure(id, error),
        },
        "discord.disconnect" => match handle.disconnect() {
            Ok(()) => Response::success(id, handle.state()),
            Err(error) => Response::failure(id, error),
        },
        "discord.listPresets" => {
            let scan = handle.scan_presets();
            Response::success(
                id,
                json!({
                    "ok": true,
                    "presets": scan.presets,
                    "errors": scan.errors,
                }),
            )
        }
        "discord.savePreset" => save_preset(handle, id, request.params),
        "discord.setPreset" => set_preset(handle, id, request.params),
        "discord.getRules" => Response::success(id, json!({ "ok": true, "rules": handle.rules() })),
        "discord.setRules" => set_rules(handle, id, request.params),
        other => Response::failure(id, format!("unknown method '{other}'")),
    }
}

fn save_preset(handle: &Handle, id: String, params: Option<Value>) -> Response {
    let Some(params) = params else {
        return Response::failure(id, "discord.savePreset needs a preset object");
    };
    let body = params.get("preset").cloned().unwrap_or(params);

    let preset: Preset = match serde_json::from_value(body) {
        Ok(preset) => preset,
        Err(error) => return Response::failure(id, format!("invalid preset: {error}")),
    };

    match handle.save_preset(&preset) {
        Ok(path) => Response::success(
            id,
            json!({
                "ok": true,
                "preset": preset,
                "path": display(&path),
            }),
        ),
        Err(error) => Response::failure(id, error.to_string()),
    }
}

fn set_preset(handle: &Handle, id: String, params: Option<Value>) -> Response {
    let name = match preset_name(params) {
        Ok(name) => name,
        Err(error) => return Response::failure(id, error),
    };

    match handle.set_preset(name.clone()) {
        Ok(()) => Response::success(
            id,
            json!({
                "ok": true,
                "activePreset": name,
                "state": handle.state(),
            }),
        ),
        Err(error) => Response::failure(id, error),
    }
}

fn set_rules(handle: &Handle, id: String, params: Option<Value>) -> Response {
    let params = params.unwrap_or(Value::Null);
    let value = params.get("rules").cloned().unwrap_or(params);

    let rules: Vec<DiscordRule> = match serde_json::from_value(value) {
        Ok(rules) => rules,
        Err(error) => return Response::failure(id, format!("invalid rules: {error}")),
    };

    match handle.set_rules(rules.clone()) {
        Ok(()) => Response::success(
            id,
            json!({ "ok": true, "rules": rules, "state": handle.state() }),
        ),
        Err(error) => Response::failure(id, error),
    }
}

/// Reads a preset name, `null` to clear, or the common wrapper objects.
fn preset_name(params: Option<Value>) -> Result<Option<String>, String> {
    match params {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(name)) => Ok(Some(name)),
        Some(Value::Object(mut object)) => {
            let value = object
                .remove("name")
                .or_else(|| object.remove("preset"))
                .unwrap_or(Value::Null);
            match value {
                Value::Null => Ok(None),
                Value::String(name) => Ok(Some(name)),
                other => Err(format!("preset name must be a string, got {other}")),
            }
        }
        Some(other) => Err(format!("preset name must be a string or null, got {other}")),
    }
}

/// A path as the dashboard can log it.
fn display(path: &Path) -> String {
    path.display().to_string()
}

/// A pipe name unique to this test process.
#[cfg(test)]
pub fn unique_pipe(tag: &str) -> String {
    format!(
        "{DISCORD_PIPE}.test.{tag}.{}.{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets::PresetStore;
    use crate::service::{Options, Service};

    fn service() -> Service {
        Service::start(Options::default()).expect("start")
    }

    fn request(method: &str, params: Option<Value>) -> Request {
        Request {
            id: "9".to_owned(),
            method: method.to_owned(),
            params,
        }
    }

    #[test]
    fn get_state_is_the_same_document_the_service_reports() {
        let service = service();
        let handle = service.handle();

        let response = dispatch(&handle, request("discord.getState", None));

        assert!(response.ok);
        assert_eq!(response.id, "9");
        assert_eq!(response.result, Some(handle.state()));
        service.shutdown();
    }

    #[test]
    fn connect_without_an_app_id_is_an_honest_failure() {
        let service = service();
        let handle = service.handle();

        let response = dispatch(&handle, request("discord.connect", None));

        assert!(!response.ok, "{response:?}");
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("не настроено")),
            "{response:?}"
        );
        service.shutdown();
    }

    #[test]
    fn set_preset_accepts_the_three_documented_shapes_and_null() {
        assert_eq!(preset_name(None), Ok(None));
        assert_eq!(preset_name(Some(Value::Null)), Ok(None));
        assert_eq!(
            preset_name(Some(json!("Gaming"))),
            Ok(Some("Gaming".to_owned()))
        );
        assert_eq!(
            preset_name(Some(json!({ "name": "Gaming" }))),
            Ok(Some("Gaming".to_owned()))
        );
        assert_eq!(
            preset_name(Some(json!({ "preset": "Gaming" }))),
            Ok(Some("Gaming".to_owned()))
        );
        assert_eq!(preset_name(Some(json!({ "preset": null }))), Ok(None));
        assert!(preset_name(Some(json!(7))).is_err());
    }

    #[test]
    fn an_unknown_preset_is_refused_without_touching_the_config() {
        let service = service();
        let handle = service.handle();

        let response = dispatch(
            &handle,
            request("discord.setPreset", Some(json!("Missing"))),
        );

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("Missing")),
            "{response:?}"
        );
        service.shutdown();
    }

    #[test]
    fn rules_round_trip_and_an_empty_pattern_is_refused() {
        let service = service();
        let handle = service.handle();
        let rules = json!([
            { "match": "exe:game.exe", "preset": "Gaming", "enabled": true }
        ]);

        let response = dispatch(&handle, request("discord.setRules", Some(rules.clone())));
        assert!(response.ok, "{response:?}");

        let response = dispatch(&handle, request("discord.getRules", None));
        assert!(response.ok);
        assert_eq!(
            response.result.expect("rules")["rules"],
            rules,
            "the rule must come back with its 'match' key"
        );

        let response = dispatch(
            &handle,
            request(
                "discord.setRules",
                Some(json!([{ "match": " ", "preset": "Gaming", "enabled": true }])),
            ),
        );
        assert!(!response.ok);
        service.shutdown();
    }

    #[test]
    fn list_presets_reports_the_directory_contents() {
        let directory = tempfile::tempdir().expect("temp dir");
        let presets = PresetStore::new(directory.path());
        presets.save(&Preset::named("One")).expect("save");
        let service = Service::start(Options {
            presets: presets.clone(),
            ..Options::default()
        })
        .expect("start");
        let handle = service.handle();

        let response = dispatch(&handle, request("discord.listPresets", None));

        assert!(response.ok);
        assert_eq!(
            response.result.expect("result")["presets"][0]["name"],
            json!("One")
        );
        service.shutdown();
    }

    #[test]
    fn the_service_answers_over_a_real_pipe() {
        let service = service();
        let pipe = unique_pipe("dispatch");
        let server = serve_on(&pipe, service.handle()).expect("start the test server");

        let answer = neb_core::ipc::call(
            &pipe,
            "discord.getState",
            None,
            neb_core::ipc::DEFAULT_TIMEOUT,
        )
        .expect("the server must answer");

        assert_eq!(answer["configured"], json!(false));
        assert_eq!(answer["message"], json!("не настроено"));
        server.shutdown();
        service.shutdown();
    }
}
