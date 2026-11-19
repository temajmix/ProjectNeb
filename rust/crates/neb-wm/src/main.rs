#![forbid(unsafe_op_in_unsafe_fn)]

//! `neb-wm` — the resident tiling manager.
//!
//! Normal mode starts the manager and keeps it running. `--call` asks a running instance one
//! question, which is how the acceptance harness and `neb-cli` drive layouts and workspaces
//! without synthesising keyboard input.

use std::process::ExitCode;

use neb_wm::app::{self, Args};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

/// Name of the mutex that makes a second resident manager exit quietly.
const INSTANCE_MUTEX: &str = r"Local\ProjectNeb.Wm";

const USAGE: &str = "\
usage: neb-wm [--config <path>] [--log-dir <path>] [--quit-after <seconds>]
       neb-wm --call <method> [--params <json>]

  Starts the ProjectNeb tiling window manager.

  --config <path>       configuration file (default: %LocalAppData%\\ProjectNeb\\config.json)
  --log-dir <path>      where the log is written (default: logs next to the configuration)
  --quit-after <secs>   stop cleanly after this many seconds, restoring every window
  --call <method>       call a running manager, e.g. wm.getState or wm.setLayout
  --params <json>       parameters for --call, e.g. {\"layout\":\"grid\"}
  --help                this text

Hotkeys: Alt+arrows focus, Alt+Shift+arrows move, Alt+1..9 workspace,
Alt+Shift+1..9 move to workspace, Alt+F float, Alt+T cycle layout.
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    if arguments
        .iter()
        .any(|argument| matches!(argument.as_str(), "--help" | "-h"))
    {
        print!("{USAGE}");
        return ExitCode::SUCCESS;
    }

    let args = match Args::parse(&arguments) {
        Ok(args) => args,
        Err(error) => {
            eprintln!("neb-wm: {error}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if let Some(call) = args.call.clone() {
        return call_manager(&call);
    }

    // A second manager must not fight the first one for the same windows.
    let instance = Instance::acquire();
    if !instance.is_primary() {
        eprintln!("neb-wm: another manager is already running");
        return ExitCode::SUCCESS;
    }

    match app::run(args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("neb-wm: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Performs one IPC call and prints the result.
fn call_manager(call: &app::Call) -> ExitCode {
    match neb_core::ipc::call(
        neb_wm::ipc::PIPE,
        &call.method,
        call.params.clone(),
        std::time::Duration::from_secs(10),
    ) {
        Ok(result) => {
            match serde_json::to_string_pretty(&result) {
                Ok(text) => println!("{text}"),
                Err(error) => {
                    println!("{result}");
                    eprintln!("neb-wm: cannot format the result: {error}");
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("neb-wm: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The single-instance mutex, released when the process ends.
#[derive(Debug)]
struct Instance {
    handle: HANDLE,
    primary: bool,
}

impl Instance {
    /// Takes the mutex, or reports that another manager already holds it.
    fn acquire() -> Self {
        let name: Vec<u16> = INSTANCE_MUTEX
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect();

        // SAFETY: `name` is a NUL-terminated UTF-16 buffer that outlives the call, and no security
        // attributes are passed, so the mutex gets this user's default DACL.
        match unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) } {
            Ok(handle) => {
                // SAFETY: `GetLastError` reads this thread's last error and changes nothing.
                let existing = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;

                if existing {
                    // SAFETY: this handle is ours and is closed exactly once, here.
                    unsafe {
                        let _ = CloseHandle(handle);
                    }

                    return Self {
                        handle: HANDLE::default(),
                        primary: false,
                    };
                }

                Self {
                    handle,
                    primary: true,
                }
            }
            Err(error) => {
                // A mutex that cannot be created is not a reason to refuse to manage windows.
                eprintln!("neb-wm: cannot create the instance mutex ({error}); continuing");
                Self {
                    handle: HANDLE::default(),
                    primary: true,
                }
            }
        }
    }

    /// Whether this process is the manager.
    fn is_primary(&self) -> bool {
        self.primary
    }
}

impl Drop for Instance {
    fn drop(&mut self) {
        if !self.handle.is_invalid() {
            // SAFETY: the handle is owned by this value and closed exactly once, here.
            unsafe {
                let _ = CloseHandle(self.handle);
            }
        }
    }
}
