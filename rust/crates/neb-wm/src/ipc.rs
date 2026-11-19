//! The `ProjectNeb.wm` IPC surface.
//!
//! The manager is a separate process from the supervisor, so every control the supervisor, the
//! settings window or `neb-cli` needs goes through the same newline-delimited JSON pipe the rest of
//! ProjectNeb uses. The methods here only mutate the model and ask the message loop to apply it:
//! no Win32 call is made on the IPC thread, which keeps a slow `SetWindowPos` from blocking the
//! answer or the pipe.
//!
//! | Method | Params | Answer |
//! | --- | --- | --- |
//! | `wm.getState` | none | the full snapshot |
//! | `wm.setLayout` | `{"layout":"grid"}` | the snapshot |
//! | `wm.enable` / `wm.disable` | none | the snapshot |
//! | `wm.workspace` | `{"index":1}` or `1` | the snapshot |
//! | `wm.moveWindow` | `{"id":123,"workspace":1}` | the snapshot |
//! | `wm.toggleFloat` / `wm.cycleLayout` | none | the snapshot |
//! | `wm.quit` | none | `{"stopping":true}` |

use serde_json::{Value, json};

use neb_core::ipc::{Request, Response};

use crate::app::{self, with_engine};
use crate::layout::LayoutKind;
use crate::model::WindowId;
use crate::win::Win32Ops;

/// Pipe the manager listens on.
pub const PIPE: &str = "ProjectNeb.wm";

/// Answers one request.
pub fn handle(request: Request) -> Response {
    let id = request.id.clone();

    match request.method.as_str() {
        "wm.getState" => match snapshot() {
            Some(state) => Response::success(id, state),
            None => Response::failure(id, "the window manager is not running"),
        },
        "wm.setLayout" => set_layout(id, request.params),
        "wm.enable" => {
            let state = with_engine(|engine| {
                engine.enabled = true;
                let ops = Win32Ops::new(engine.accent);
                engine.to_json(&ops)
            });
            app::post_command();
            reply(id, state)
        }
        "wm.disable" => {
            let state = with_engine(|engine| {
                engine.enabled = false;
                let ops = Win32Ops::new(engine.accent);
                engine.to_json(&ops)
            });
            app::post_command();
            reply(id, state)
        }
        "wm.workspace" => workspace(id, request.params),
        "wm.moveWindow" => move_window(id, request.params),
        "wm.toggleFloat" => {
            let state = with_engine(|engine| {
                engine.toggle_float();
                let ops = Win32Ops::new(engine.accent);
                engine.to_json(&ops)
            });
            app::post_command();
            reply(id, state)
        }
        "wm.cycleLayout" => {
            let state = with_engine(|engine| {
                engine.cycle_layout();
                let ops = Win32Ops::new(engine.accent);
                engine.to_json(&ops)
            });
            app::post_command();
            reply(id, state)
        }
        "wm.quit" => {
            app::post_quit();
            Response::success(id, json!({ "stopping": true }))
        }
        "wm.candidates" => candidates(id),
        other => Response::failure(id, format!("unknown method: {other}")),
    }
}

/// `wm.getState`.
fn snapshot() -> Option<Value> {
    with_engine(|engine| {
        let ops = Win32Ops::new(engine.accent);
        engine.to_json(&ops)
    })
}

/// `wm.setLayout`.
fn set_layout(id: String, params: Option<Value>) -> Response {
    let Some(name) = params
        .as_ref()
        .and_then(|params| params.get("layout"))
        .and_then(Value::as_str)
    else {
        return Response::failure(id, "wm.setLayout needs params.layout as a string");
    };

    let Some(kind) = LayoutKind::parse(name) else {
        return Response::failure(
            id,
            format!(
                "'{name}' is not a layout; use one of {}",
                LayoutKind::ALL
                    .iter()
                    .map(|kind| kind.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
    };

    let state = with_engine(|engine| {
        engine.set_layout(kind);
        let ops = Win32Ops::new(engine.accent);
        engine.to_json(&ops)
    });

    app::post_command();
    reply(id, state)
}

/// `wm.workspace`.
fn workspace(id: String, params: Option<Value>) -> Response {
    let index = params
        .as_ref()
        .and_then(|params| {
            params
                .get("index")
                .and_then(Value::as_u64)
                .or_else(|| params.as_u64())
        })
        .map(|value| value as usize);

    let Some(index) = index else {
        return Response::failure(id, "wm.workspace needs params.index as a number");
    };

    let state = with_engine(|engine| {
        engine.model.set_active(index);
        let ops = Win32Ops::new(engine.accent);
        engine.to_json(&ops)
    });

    app::post_command();
    reply(id, state)
}

/// `wm.moveWindow`: by handle or by position in the window list, to a workspace.
fn move_window(id: String, params: Option<Value>) -> Response {
    let Some(params) = params else {
        return Response::failure(
            id,
            "wm.moveWindow needs {\"id\":<handle>,\"workspace\":<index>}",
        );
    };

    let workspace = params.get("workspace").and_then(Value::as_u64);
    let Some(workspace) = workspace else {
        return Response::failure(id, "wm.moveWindow needs params.workspace as a number");
    };

    let handle = params
        .get("id")
        .and_then(Value::as_i64)
        .map(|value| WindowId(value as isize));
    let position = params
        .get("index")
        .and_then(Value::as_u64)
        .map(|value| value as usize);

    let state = with_engine(|engine| {
        let target = handle.or_else(|| {
            position
                .and_then(|index| engine.model.windows().get(index))
                .map(|window| window.id)
        });

        if let Some(window) = target {
            engine.model.set_workspace(window, workspace as usize);
        }

        let ops = Win32Ops::new(engine.accent);
        engine.to_json(&ops)
    });

    app::post_command();
    reply(id, state)
}

/// `wm.candidates`: every titled window with the manager's decision and, when rejected, why.
fn candidates(id: String) -> Response {
    let managed = with_engine(|engine| {
        engine
            .model
            .windows()
            .iter()
            .map(|window| window.id.0)
            .collect::<Vec<_>>()
    })
    .unwrap_or_default();

    let windows: Vec<Value> = crate::win::titled_windows()
        .into_iter()
        .map(|window| {
            let inspected = crate::win::inspect(window.id);
            let (candidate, reason, exe) = match &inspected {
                Ok(candidate) => (true, Value::Null, Some(candidate.facts.exe.clone())),
                Err(reason) => (false, json!(reason), None),
            };

            json!({
                "id": window.id.0,
                "title": window.title,
                "class": window.class,
                "candidate": candidate,
                "managed": managed.contains(&window.id.0),
                "reason": reason,
                "exe": exe,
            })
        })
        .collect();

    Response::success(id, json!({ "windows": windows }))
}

/// A snapshot answer, or the reason the manager is not available.
fn reply(id: String, state: Option<Value>) -> Response {
    match state {
        Some(state) => Response::success(id, state),
        None => Response::failure(id, "the window manager is not running"),
    }
}

/// The manager state as JSON, for the supervisor follow-up and for tests.
pub fn state() -> Option<Value> {
    snapshot()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_method_is_reported_and_does_not_panic() {
        let response = handle(Request::new("1", "wm.nothing"));

        assert!(!response.ok);
        assert!(
            response
                .error
                .unwrap_or_default()
                .contains("unknown method"),
            "the error names the method"
        );
    }

    #[test]
    fn set_layout_rejects_an_unknown_name() {
        let response = handle(Request::with_params(
            "2",
            "wm.setLayout",
            json!({ "layout": "cascade" }),
        ));

        assert!(!response.ok);
        assert!(response.error.unwrap_or_default().contains("not a layout"));
    }

    #[test]
    fn set_layout_requires_a_string() {
        let response = handle(Request::with_params(
            "3",
            "wm.setLayout",
            json!({ "layout": 7 }),
        ));

        assert!(!response.ok);
    }

    #[test]
    fn the_quit_method_answers_without_a_running_engine() {
        let response = handle(Request::new("4", "wm.quit"));

        // `post_quit` is a no-op when no host window exists, which is the test process; the answer
        // is still a success so a caller can distinguish "asked" from "refused".
        assert!(response.ok);
        assert_eq!(response.result, Some(json!({ "stopping": true })));
    }
}
