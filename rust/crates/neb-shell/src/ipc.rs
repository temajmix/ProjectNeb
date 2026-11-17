//! The shell's IPC surface on `\\.\pipe\ProjectNeb.shell`.
//!
//! Four methods, all of them cheap: `shell.ping` and `shell.getState` answer from atomics,
//! `shell.reload` and `shell.setBarVisible` post a message to the UI thread and answer immediately.
//! Nothing here waits for the UI thread, which is the rule that keeps a diagnostic client from being
//! able to freeze the bar, and the rule that keeps a bar that is painting from stalling a client.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use neb_core::ipc::{IpcError, Request, Response, ServerHandle, serve};
use serde_json::{Value, json};

use crate::metrics_feed::Metrics;
use crate::window::{self, WM_RELOAD, WM_VISIBILITY};

/// Pipe name shared with `Neb.App` and with the C# shell.
pub const SHELL_PIPE: &str = "ProjectNeb.shell";

/// State the IPC handlers read, shared with the UI thread and the metrics worker.
#[derive(Debug)]
pub struct Shared {
    metrics: Arc<Metrics>,
    bars: AtomicU32,
}

impl Shared {
    /// Shares `metrics` and reports no bars until the application says otherwise.
    pub fn new(metrics: Arc<Metrics>) -> Self {
        Self {
            metrics,
            bars: AtomicU32::new(0),
        }
    }

    /// Records how many bars are currently laid out.
    pub fn set_bars(&self, bars: usize) {
        self.bars
            .store(u32::try_from(bars).unwrap_or(u32::MAX), Ordering::SeqCst);
    }

    /// How many bars are currently laid out.
    pub fn bars(&self) -> u32 {
        self.bars.load(Ordering::SeqCst)
    }

    /// The answer to `shell.getState`: the counts and the readout, in one object.
    pub fn state(&self) -> Value {
        let snapshot = self.metrics.snapshot();

        json!({
            "bars": self.bars(),
            "cpu": snapshot.cpu,
            "ramMb": snapshot.ram_mb,
            "version": crate::VERSION,
        })
    }
}

/// Starts answering on [`SHELL_PIPE`].
///
/// # Errors
///
/// [`IpcError`] when the pipe cannot be created; the shell then has no diagnostic surface but still
/// draws, which the caller reports rather than treating as fatal.
pub fn start(shared: Arc<Shared>) -> Result<ServerHandle, IpcError> {
    serve(SHELL_PIPE, move |request: Request| {
        dispatch(&shared, request)
    })
}

/// Turns one request into one response. Never blocks on the UI thread.
fn dispatch(shared: &Shared, request: Request) -> Response {
    let id = request.id.clone();

    match request.method.as_str() {
        "shell.ping" | "shell.getState" => Response::success(id, shared.state()),
        "shell.reload" => {
            window::post_to_control(WM_RELOAD, 0);
            Response::success(id, shared.state())
        }
        "shell.setBarVisible" => match visible_parameter(&request) {
            Some(visible) => {
                window::post_to_control(WM_VISIBILITY, usize::from(visible));
                Response::success(id, shared.state())
            }
            None => Response::failure(id, "'shell.setBarVisible' needs a boolean 'visible'"),
        },
        other => Response::failure(id, format!("unknown method '{other}'")),
    }
}

/// Reads `params.visible`, which must be present and boolean.
fn visible_parameter(request: &Request) -> Option<bool> {
    request
        .params
        .as_ref()
        .and_then(|params| params.get("visible"))
        .and_then(Value::as_bool)
}

/// A pipe name no test run can collide with.
#[cfg(test)]
pub fn unique_pipe(tag: &str) -> String {
    use std::sync::atomic::AtomicUsize;

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    format!(
        "{SHELL_PIPE}.test.{tag}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shared() -> Arc<Shared> {
        Arc::new(Shared::new(Metrics::start()))
    }

    fn request(method: &str, params: Option<Value>) -> Request {
        Request {
            id: "7".to_owned(),
            method: method.to_owned(),
            params,
        }
    }

    #[test]
    fn a_state_answer_carries_the_four_documented_fields() {
        let shared = shared();
        shared.set_bars(2);

        let state = shared.state();

        assert_eq!(state["bars"], json!(2));
        assert_eq!(state["version"], json!(crate::VERSION));
        assert!(state["cpu"].is_number(), "{state}");
        assert!(state["ramMb"].is_number(), "{state}");
        assert_eq!(state.as_object().map(serde_json::Map::len), Some(4));
        shared.metrics.stop();
    }

    #[test]
    fn ping_answers_with_the_same_shape_as_get_state() {
        let shared = shared();

        let ping = dispatch(&shared, request("shell.ping", None));
        let state = dispatch(&shared, request("shell.getState", None));

        assert!(ping.ok);
        assert_eq!(
            ping.result.as_ref().map(|result| result["version"].clone()),
            state
                .result
                .as_ref()
                .map(|result| result["version"].clone())
        );
        assert_eq!(ping.id, "7");
        shared.metrics.stop();
    }

    #[test]
    fn reload_answers_at_once_and_leaves_the_work_to_the_ui_thread() {
        let shared = shared();

        let response = dispatch(&shared, request("shell.reload", None));

        assert!(response.ok);
        shared.metrics.stop();
    }

    #[test]
    fn set_bar_visible_needs_a_boolean_and_accepts_both_values() {
        let shared = shared();

        for visible in [true, false] {
            let response = dispatch(
                &shared,
                request("shell.setBarVisible", Some(json!({ "visible": visible }))),
            );
            assert!(response.ok, "{visible}: {response:?}");
        }

        for broken in [None, Some(json!({})), Some(json!({ "visible": "yes" }))] {
            let response = dispatch(&shared, request("shell.setBarVisible", broken.clone()));
            assert!(!response.ok, "{broken:?} must be refused");
            assert!(
                response.error.unwrap_or_default().contains("visible"),
                "the refusal must name the parameter"
            );
        }

        shared.metrics.stop();
    }

    #[test]
    fn an_unknown_method_is_refused_by_name() {
        let shared = shared();

        let response = dispatch(&shared, request("shell.explode", None));

        assert!(!response.ok);
        assert!(response.error.unwrap_or_default().contains("shell.explode"));
        shared.metrics.stop();
    }

    #[test]
    fn the_surface_answers_over_a_real_pipe() {
        let shared = shared();
        shared.set_bars(1);
        let pipe = unique_pipe("dispatch");
        let server = serve(&pipe, {
            let shared = Arc::clone(&shared);
            move |request| dispatch(&shared, request)
        })
        .expect("start the test server");

        let answer = neb_core::ipc::call(
            &pipe,
            "shell.getState",
            None,
            neb_core::ipc::DEFAULT_TIMEOUT,
        )
        .expect("the server must answer");

        assert_eq!(answer["bars"], json!(1));
        assert_eq!(answer["version"], json!(crate::VERSION));
        server.shutdown();
        shared.metrics.stop();
    }
}
