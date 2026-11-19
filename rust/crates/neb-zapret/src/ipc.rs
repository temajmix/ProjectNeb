//! The `ProjectNeb.zapret` pipe.
//!
//! Six methods, all answered from the manager's state: `zapret.getState`, `zapret.listStrategies`,
//! `zapret.start`, `zapret.stop`, `zapret.import` and `zapret.selfTest`, plus `zapret.ping`.
//!
//! Handlers run on the IPC server's client threads and may block for a file copy or a UAC request;
//! that is the same trade `neb-supervisor` makes for its own commands. The manager behind the mutex
//! is the single writer, so two clients cannot start two strategies at once.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};

use neb_core::ipc::{IpcError, Request, Response, ServerHandle};
use serde_json::{Value, json};

use crate::host::Host;
use crate::manager::Manager;

/// Pipe the module answers on.
pub const PIPE: &str = "ProjectNeb.zapret";

/// Starts answering on `pipe`.
///
/// # Errors
///
/// [`IpcError`] when the pipe cannot be created.
pub fn serve<H>(manager: Arc<Mutex<Manager<H>>>, pipe: &str) -> Result<ServerHandle, IpcError>
where
    H: Host + 'static,
{
    neb_core::ipc::serve(pipe, move |request| dispatch(&manager, request))
}

/// Turns one request into one response.
pub fn dispatch<H: Host>(manager: &Mutex<Manager<H>>, request: Request) -> Response {
    let id = request.id.clone();

    match request.method.as_str() {
        "zapret.ping" => Response::success(id, json!({ "ok": true, "version": crate::VERSION })),
        "zapret.getState" => {
            let state = lock(manager).state();
            Response::success(id, json!(state))
        }
        "zapret.listStrategies" => match lock(manager).list_strategies() {
            Ok(list) => Response::success(id, json!(list)),
            Err(error) => Response::failure(id, error.to_string()),
        },
        "zapret.start" => match strategy(&request.params) {
            Some(strategy) => match lock(manager).start(strategy) {
                Ok(state) => Response::success(id, json!(state)),
                Err(error) => Response::failure(id, error.to_string()),
            },
            None => Response::failure(
                id,
                "'zapret.start' needs a strategy: {\"strategy\":\"general\"}",
            ),
        },
        "zapret.stop" => match lock(manager).stop() {
            Ok(state) => Response::success(id, json!(state)),
            Err(error) => Response::failure(id, error.to_string()),
        },
        "zapret.import" => match path(&request.params) {
            Some(path) => match lock(manager).import(&path) {
                Ok(state) => Response::success(id, json!(state)),
                Err(error) => Response::failure(id, error.to_string()),
            },
            None => Response::failure(
                id,
                "'zapret.import' needs a folder: {\"path\":\"C:\\\\zapret\"}",
            ),
        },
        "zapret.selfTest" => {
            let report = lock(manager).self_test();
            Response::success(id, json!(report))
        }
        other => Response::failure(id, format!("unknown method '{other}'")),
    }
}

fn lock<'a, H: Host>(manager: &'a Mutex<Manager<H>>) -> MutexGuard<'a, Manager<H>> {
    manager.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// The strategy name in `{"strategy":"…"}` or a bare string.
fn strategy(params: &Option<Value>) -> Option<&str> {
    match params.as_ref()? {
        Value::String(name) => Some(name.as_str()),
        Value::Object(object) => ["strategy", "name"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str)),
        _ => None,
    }
}

/// The folder in `{"path":"…"}` or a bare string.
fn path(params: &Option<Value>) -> Option<PathBuf> {
    let text = match params.as_ref()? {
        Value::String(path) => path.as_str(),
        Value::Object(object) => ["path", "folder"]
            .iter()
            .find_map(|key| object.get(*key).and_then(Value::as_str))?,
        _ => return None,
    };

    Some(PathBuf::from(text))
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
    use crate::host::{HostError, Launch};
    use crate::pack::{BIN_DIR, WINDIVERT_SYS, WINWS_EXE};
    use crate::paths::Paths;
    use crate::status::{self, StatusFile};
    use std::fs;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, Ordering};
    use tempfile::tempdir;

    #[derive(Debug, Default)]
    struct TestHost {
        alive: Mutex<Vec<u32>>,
        fail_start: AtomicBool,
    }

    impl Host for TestHost {
        fn is_admin(&self) -> bool {
            true
        }

        fn start(
            &self,
            strategy: &str,
            _pack: &Path,
            status_path: &Path,
            _elevate: bool,
        ) -> Result<Launch, HostError> {
            if self.fail_start.load(Ordering::SeqCst) {
                return Err(HostError::Spawn(std::io::Error::other("no")));
            }

            let pid = 7000;
            status::write(
                status_path,
                &StatusFile::started(pid, strategy, "0123456789abcdef", true),
            )
            .expect("status");
            self.alive
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(pid);
            Ok(Launch::Direct(pid))
        }

        fn stop(&self, _pack: &Path, status_path: &Path, _elevate: bool) -> Result<(), HostError> {
            let mut file = status::read(status_path).ok().flatten().unwrap_or_default();
            file.running = false;
            file.pid = None;
            file.stop_requested = true;
            file.message = "stopped".to_owned();
            status::write(status_path, &file).expect("status");
            self.alive
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clear();
            Ok(())
        }

        fn pid_alive(&self, pid: u32) -> bool {
            self.alive
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains(&pid)
        }

        fn take_error(&self) -> Option<String> {
            None
        }
    }

    fn manager() -> (tempfile::TempDir, Arc<Mutex<Manager<TestHost>>>) {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        fs::create_dir_all(paths.data_dir.join(BIN_DIR)).expect("bin");
        fs::write(paths.data_dir.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        fs::write(paths.data_dir.join(BIN_DIR).join(WINDIVERT_SYS), b"sys").expect("sys");
        fs::write(
            paths.data_dir.join("general.bat"),
            "@echo off\r\nstart \"\" \"%~dp0bin\\winws.exe\" --new\r\n",
        )
        .expect("strategy");

        let manager = Manager::at(paths, TestHost::default());
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
    fn ping_answers_with_the_version() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.ping", None));

        assert!(response.ok);
        assert_eq!(
            response.result.expect("result")["version"],
            json!(crate::VERSION)
        );
        assert_eq!(response.id, "7");
    }

    #[test]
    fn get_state_answers_with_the_documented_fields() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.getState", None));
        let state = response.result.expect("state");

        for key in [
            "installed",
            "packVersion",
            "strategy",
            "running",
            "pid",
            "lastError",
        ] {
            assert!(state.get(key).is_some(), "{key} missing from {state}");
        }
        assert_eq!(state["installed"], json!(true));
        assert_eq!(state["running"], json!(false));
    }

    #[test]
    fn list_strategies_returns_names_and_argv_lengths() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.listStrategies", None));
        let list = response.result.expect("list");

        assert_eq!(list["installed"], json!(true));
        assert_eq!(list["strategies"][0]["name"], json!("general"));
        assert_eq!(list["strategies"][0]["args"], json!(["--new"]));
        assert_eq!(list["strategies"][0]["argumentCount"], json!(1));
    }

    #[test]
    fn start_accepts_an_object_and_a_bare_string() {
        let (_directory, manager) = manager();

        let response = dispatch(
            &manager,
            request("zapret.start", Some(json!({ "strategy": "general" }))),
        );
        assert!(response.ok, "{response:?}");
        let state = response.result.expect("state");
        assert_eq!(state["pending"], json!(true));
        assert_eq!(state["strategy"], json!("general"));

        // The fake host wrote the status immediately, so the next read is the running state.
        let state = dispatch(&manager, request("zapret.getState", None))
            .result
            .expect("state");
        assert_eq!(state["running"], json!(true));

        assert!(dispatch(&manager, request("zapret.stop", None)).ok);
        assert!(
            !dispatch(&manager, request("zapret.stop", None)).ok,
            "a second stop has nothing to stop"
        );
    }

    #[test]
    fn start_without_a_strategy_is_refused_with_an_example() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.start", None));

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("strategy"))
        );
    }

    #[test]
    fn a_start_the_host_refuses_arrives_as_an_error() {
        let (_directory, manager) = manager();
        manager
            .lock()
            .expect("lock")
            .host()
            .fail_start
            .store(true, Ordering::SeqCst);

        let response = dispatch(
            &manager,
            request("zapret.start", Some(json!({ "name": "general" }))),
        );

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("helper")),
            "{response:?}"
        );
    }

    #[test]
    fn stop_without_a_process_is_refused_by_name() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.stop", None));

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("not running"))
        );
    }

    #[test]
    fn import_accepts_a_path_object_and_copies_the_pack() {
        let directory = tempdir().expect("temp dir");
        let paths = Paths::at(directory.path());
        let manager = Arc::new(Mutex::new(Manager::at(paths.clone(), TestHost::default())));

        let source = directory.path().join("download");
        fs::create_dir_all(source.join(BIN_DIR)).expect("bin");
        fs::write(source.join(BIN_DIR).join(WINWS_EXE), b"exe").expect("winws");
        fs::write(source.join(BIN_DIR).join(WINDIVERT_SYS), b"sys").expect("sys");
        fs::write(
            source.join("general.bat"),
            "@echo off\r\nstart \"\" \"%~dp0bin\\winws.exe\" --new\r\n",
        )
        .expect("strategy");

        let response = dispatch(
            &manager,
            request(
                "zapret.import",
                Some(json!({ "path": source.display().to_string() })),
            ),
        );

        assert!(response.ok, "{response:?}");
        let state = response.result.expect("state");
        assert_eq!(state["installed"], json!(true));
        assert!(paths.data_dir.join(BIN_DIR).join(WINWS_EXE).is_file());
    }

    #[test]
    fn import_without_a_path_is_refused_with_an_example() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.import", Some(json!({}))));

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("path"))
        );
    }

    #[test]
    fn self_test_answers_with_named_checks() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.selfTest", None));
        let report = response.result.expect("report");

        assert!(
            report["checks"]
                .as_array()
                .is_some_and(|checks| checks.len() == 7)
        );
        assert!(report["ok"].is_boolean());
        for id in ["pack", "windivert", "proxy", "hosts"] {
            assert!(
                report["checks"]
                    .as_array()
                    .expect("checks")
                    .iter()
                    .any(|check| check["id"] == json!(id)),
                "{id} missing from {report}"
            );
        }
    }

    #[test]
    fn an_unknown_method_is_refused_by_name() {
        let (_directory, manager) = manager();
        let response = dispatch(&manager, request("zapret.explode", None));

        assert!(!response.ok);
        assert!(
            response
                .error
                .as_deref()
                .is_some_and(|error| error.contains("zapret.explode"))
        );
    }

    #[test]
    fn the_surface_answers_over_a_real_pipe() {
        let (_directory, manager) = manager();
        let pipe = unique_pipe("dispatch");
        let server = serve(Arc::clone(&manager), &pipe).expect("serve");

        let state = neb_core::ipc::call(
            &pipe,
            "zapret.getState",
            None,
            neb_core::ipc::DEFAULT_TIMEOUT,
        )
        .expect("state");
        assert_eq!(state["installed"], json!(true));

        let strategies = neb_core::ipc::call(
            &pipe,
            "zapret.listStrategies",
            None,
            neb_core::ipc::DEFAULT_TIMEOUT,
        )
        .expect("strategies");
        assert_eq!(strategies["strategies"][0]["name"], json!("general"));

        server.shutdown();
    }
}
