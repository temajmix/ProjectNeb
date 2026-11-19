//! The `ProjectNeb.windhawk` pipe.
//!
//! Six methods plus `ping`, all answered from the manager:
//!
//! * `windhawk.getState` — installation, data directory, counts and the module switches;
//! * `windhawk.listCatalog {query?, limit?}` — a catalog page with `installed` flags;
//! * `windhawk.listInstalled` — sources, compiled DLLs and enabled state;
//! * `windhawk.install {id}` — download and write through the helper;
//! * `windhawk.remove {id}` — source, configuration and compiled DLLs;
//! * `windhawk.open {trayOnly?}` — start Windhawk's window or tray process.

use std::sync::{Arc, Mutex, MutexGuard};

use neb_core::ipc::{IpcError, Request, Response, ServerHandle};
use serde_json::{Value, json};

use crate::host::Host;
use crate::manager::{DEFAULT_LIMIT, Manager};

/// Pipe the module answers on.
pub const PIPE: &str = "ProjectNeb.windhawk";

/// Starts answering on `pipe`.
///
/// # Errors
///
/// [`IpcError`] when the pipe cannot be created.
pub fn serve<H: Host + 'static>(
    manager: Arc<Mutex<Manager<H>>>,
    pipe: &str,
) -> Result<ServerHandle, IpcError> {
    neb_core::ipc::serve(pipe, move |request| dispatch(&manager, request))
}

/// Turns one request into one response.
pub fn dispatch<H: Host>(manager: &Mutex<Manager<H>>, request: Request) -> Response {
    let id = request.id.clone();

    match request.method.as_str() {
        "windhawk.ping" => Response::success(
            id,
            json!({ "ok": true, "version": crate::VERSION, "pipe": PIPE }),
        ),
        "windhawk.getState" => {
            let state = lock(manager).state();
            Response::success(id, json!(state))
        }
        "windhawk.listCatalog" => {
            let query = string_param(&request.params, &["query", "search"]).unwrap_or_default();
            let limit = limit_param(&request.params);
            match lock(manager).list_catalog(query, limit) {
                Ok(page) => Response::success(id, json!(page)),
                Err(error) => Response::failure(id, error.to_string()),
            }
        }
        "windhawk.listInstalled" => {
            let report = lock(manager).list_installed();
            Response::success(id, json!(report))
        }
        "windhawk.install" => match id_param(&request.params) {
            Some(id) => match lock(manager).install(id) {
                Ok(outcome) => Response::success(request.id.clone(), json!(outcome)),
                Err(error) => Response::failure(request.id.clone(), error.to_string()),
            },
            None => Response::failure(
                request.id.clone(),
                "'windhawk.install' needs an id: {\"id\":\"accent-color-sync\"}",
            ),
        },
        "windhawk.remove" => match id_param(&request.params) {
            Some(id) => match lock(manager).remove(id) {
                Ok(outcome) => Response::success(request.id.clone(), json!(outcome)),
                Err(error) => Response::failure(request.id.clone(), error.to_string()),
            },
            None => Response::failure(
                request.id.clone(),
                "'windhawk.remove' needs an id: {\"id\":\"accent-color-sync\"}",
            ),
        },
        "windhawk.open" => {
            let tray_only = bool_param(&request.params, &["trayOnly", "tray"]).unwrap_or(false);
            match lock(manager).launch(tray_only) {
                Ok(()) => Response::success(id, json!({ "opened": true, "trayOnly": tray_only })),
                Err(error) => Response::failure(id, error),
            }
        }
        other => Response::failure(id, format!("unknown method '{other}'")),
    }
}

fn lock<'a, H: Host>(manager: &'a Mutex<Manager<H>>) -> MutexGuard<'a, Manager<H>> {
    manager.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// The id in `{"id":"…"}`, `{"modId":"…"}` or a bare string.
fn id_param(params: &Option<Value>) -> Option<&str> {
    string_param(params, &["id", "modId"])
}

fn string_param<'a>(params: &'a Option<Value>, keys: &[&str]) -> Option<&'a str> {
    match params.as_ref()? {
        Value::String(text) => Some(text.as_str()),
        Value::Object(object) => keys
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str)),
        _ => None,
    }
}

fn limit_param(params: &Option<Value>) -> usize {
    let Some(Value::Object(object)) = params.as_ref() else {
        return DEFAULT_LIMIT;
    };

    object
        .get("limit")
        .and_then(Value::as_u64)
        .and_then(|limit| usize::try_from(limit).ok())
        .unwrap_or(DEFAULT_LIMIT)
}

fn bool_param(params: &Option<Value>, keys: &[&str]) -> Option<bool> {
    let Value::Object(object) = params.as_ref()? else {
        return None;
    };

    keys.iter()
        .find_map(|key| object.get(*key).and_then(Value::as_bool))
}

#[cfg(test)]
pub(crate) fn unique_pipe(tag: &str) -> String {
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT: AtomicUsize = AtomicUsize::new(0);

    format!(
        "{PIPE}.test.{tag}.{}.{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::SeqCst)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::detect::Detection;
    use crate::paths::Paths;
    use crate::plan::{Action, ApplyPlan, ApplyResult};
    use crate::registry::MemoryRegistry;
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tempfile::tempdir;

    #[derive(Debug)]
    struct TestHost {
        detection: Detection,
        sources: Mutex<BTreeMap<String, Vec<u8>>>,
        applied: Mutex<Vec<ApplyPlan>>,
        fetch_calls: AtomicU64,
    }

    impl TestHost {
        fn new(data: &Path) -> Self {
            Self {
                detection: Detection {
                    installed: true,
                    version: Some("1.7.3".to_owned()),
                    path: Some(r"A:\Windhawk\windhawk.exe".to_owned()),
                    data_dir: Some(data.display().to_string()),
                    running: false,
                    source: Some("portable".to_owned()),
                },
                sources: Mutex::new(BTreeMap::new()),
                applied: Mutex::new(Vec::new()),
                fetch_calls: AtomicU64::new(0),
            }
        }
    }

    impl Host for TestHost {
        fn is_admin(&self) -> bool {
            false
        }

        fn detect(&self) -> Detection {
            self.detection.clone()
        }

        fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
            self.fetch_calls.fetch_add(1, Ordering::Relaxed);
            if url == crate::catalog::CATALOG_URL {
                return Ok(br#"[
                    {"name":"accent-color-sync.wh.cpp"},
                    {"name":"uxtheme-hook.wh.cpp"}
                ]"#
                .to_vec());
            }

            let id = url
                .strip_prefix(crate::catalog::RAW_BASE)
                .and_then(|rest| rest.strip_prefix('/'))
                .and_then(|rest| rest.strip_suffix(".wh.cpp"))
                .ok_or_else(|| format!("unexpected url '{url}'"))?;

            self.sources
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .get(id)
                .cloned()
                .ok_or_else(|| format!("no source for '{id}'"))
        }

        fn apply(&self, plan: &ApplyPlan) -> Result<ApplyResult, String> {
            plan.validate().map_err(|error| error.to_string())?;
            self.applied
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(plan.clone());

            let target = plan.target().map_err(|error| error.to_string())?;
            if plan.action == Action::Install {
                let bytes = fs::read(plan.source.as_deref().expect("source"))
                    .map_err(|error| error.to_string())?;
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent).map_err(|error| error.to_string())?;
                }
                fs::write(&target, bytes).map_err(|error| error.to_string())?;
            } else {
                let _ = fs::remove_file(&target);
            }

            Ok(ApplyResult {
                ok: true,
                action: plan.action,
                id: plan.id.clone(),
                installed: plan.action == Action::Install,
                enabled: "unknown".to_owned(),
                needs_windhawk_ui: plan.action == Action::Install,
                source_path: Some(target.display().to_string()),
                config_key: None,
                error: None,
                note: None,
            })
        }

        fn launch(&self, tray_only: bool) -> Result<(), String> {
            Err(format!("launch tray_only={tray_only}"))
        }
    }

    fn manager() -> (tempfile::TempDir, Arc<Mutex<Manager<TestHost>>>) {
        let directory = tempdir().expect("temp dir");
        let data = directory.path().join("Windhawk");
        let paths = Paths::at(directory.path());
        let manager = Manager::at(paths, TestHost::new(&data), Box::new(MemoryRegistry::new()));

        (directory, Arc::new(Mutex::new(manager)))
    }

    fn request(method: &str, params: Option<Value>) -> Request {
        Request {
            id: "7".to_owned(),
            method: method.to_owned(),
            params,
        }
    }

    #[test]
    fn ping_answers_with_the_version_and_pipe() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("windhawk.ping", None));

        assert!(response.ok);
        let result = response.result.expect("result");
        assert_eq!(result["version"], json!(crate::VERSION));
        assert_eq!(result["pipe"], json!(PIPE));
    }

    #[test]
    fn get_state_has_the_documented_fields() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("windhawk.getState", None));
        let state = response.result.expect("state");

        for key in [
            "installed",
            "version",
            "path",
            "dataDir",
            "detectedDataDir",
            "running",
            "moduleEnabled",
            "autoStart",
            "installedCount",
            "enabledCount",
            "catalogCached",
        ] {
            assert!(state.get(key).is_some(), "{key} missing from {state}");
        }
        assert_eq!(state["installed"], json!(true));
        assert_eq!(state["running"], json!(false));
    }

    #[test]
    fn list_catalog_accepts_an_empty_object_and_a_query() {
        let (_directory, manager) = manager();

        let response = dispatch(
            &manager,
            request("windhawk.listCatalog", Some(json!({ "limit": 1 }))),
        );
        assert!(response.ok, "{response:?}");
        let page = response.result.expect("page");
        assert_eq!(page["total"], json!(2));
        assert_eq!(page["entries"].as_array().expect("entries").len(), 1);

        let response = dispatch(
            &manager,
            request("windhawk.listCatalog", Some(json!({ "query": "uxtheme" }))),
        );
        let page = response.result.expect("page");
        assert_eq!(page["entries"][0]["id"], json!("uxtheme-hook"));
    }

    #[test]
    fn list_installed_is_an_empty_report_before_anything_is_installed() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("windhawk.listInstalled", None));

        assert!(response.ok);
        let report = response.result.expect("report");
        assert_eq!(report["mods"], json!([]));
        assert_eq!(report["issues"], json!([]));
    }

    #[test]
    fn install_accepts_an_object_and_returns_the_honest_unknown_state() {
        let (_directory, manager) = manager();
        manager
            .lock()
            .expect("lock")
            .host()
            .sources
            .lock()
            .expect("sources")
            .insert("accent-color-sync".to_owned(), b"// mod".to_vec());

        let response = dispatch(
            &manager,
            request(
                "windhawk.install",
                Some(json!({ "id": "accent-color-sync" })),
            ),
        );

        assert!(response.ok, "{response:?}");
        let outcome = response.result.expect("outcome");
        assert_eq!(outcome["id"], json!("accent-color-sync"));
        assert_eq!(outcome["installed"], json!(true));
        assert_eq!(outcome["enabled"], json!("unknown"));
        assert_eq!(outcome["needsWindhawkUi"], json!(true));
    }

    #[test]
    fn install_without_an_id_is_refused_with_an_example() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("windhawk.install", Some(json!({}))));

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("accent-color-sync"))
        );
    }

    #[test]
    fn remove_reports_an_absent_mod_by_name() {
        let (_directory, manager) = manager();
        let response = dispatch(
            &manager,
            request("windhawk.remove", Some(json!({ "id": "uxtheme-hook" }))),
        );

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("not installed")),
            "{response:?}"
        );
    }

    #[test]
    fn open_passes_the_tray_switch_and_reports_host_refusals() {
        let (_directory, manager) = manager();

        let response = dispatch(
            &manager,
            request("windhawk.open", Some(json!({ "trayOnly": true }))),
        );
        assert!(!response.ok);
        assert!(response.error.expect("error").contains("tray_only=true"));
    }

    #[test]
    fn an_unknown_method_is_refused_by_name() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("windhawk.explode", None));

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("windhawk.explode"))
        );
    }

    #[test]
    fn the_surface_answers_over_a_real_pipe() {
        let (_directory, manager) = manager();
        let pipe = unique_pipe("dispatch");
        let server = serve(Arc::clone(&manager), &pipe).expect("serve");

        let state = neb_core::ipc::call(
            &pipe,
            "windhawk.getState",
            None,
            neb_core::ipc::DEFAULT_TIMEOUT,
        )
        .expect("state");
        assert_eq!(state["installed"], json!(true));

        let catalog = neb_core::ipc::call(
            &pipe,
            "windhawk.listCatalog",
            Some(json!({ "limit": 10 })),
            neb_core::ipc::DEFAULT_TIMEOUT,
        )
        .expect("catalog");
        assert_eq!(catalog["total"], json!(2));

        server.shutdown();
    }
}
