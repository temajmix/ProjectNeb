#![forbid(unsafe_op_in_unsafe_fn)]
// A resident shell must not own a console window: the binary is a GUI-subsystem program, and the one
// mode that reports to a terminal attaches to its parent's console instead of creating one.
#![windows_subsystem = "windows"]

//! `neb-shell` — the resident ProjectNeb bar.
//!
//! The binary does three things the library must not: it decides that this process is the one shell
//! instance, it parses the command line, and it turns the run into an exit code and a report. The bar
//! itself lives in `neb_shell`.
//!
//! `--report-memory` is deliberately absent from the usage text: it is the acceptance harness from the
//! phase-2 plan, not a switch a user should find. It runs the real shell for ten seconds and prints
//! the working set that budget is measured against.

use std::process::ExitCode;

use neb_shell::app::{self, Args};
use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::PCWSTR;

/// Name of the mutex that makes a second `neb-shell.exe` exit quietly.
const INSTANCE_MUTEX: &str = r"Local\ProjectNeb.Shell.Rust";

/// The working set a bar is allowed to have, in bytes.
const MEMORY_BUDGET: usize = 35 * 1024 * 1024;

const USAGE: &str = "\
usage: neb-shell [--help]

  Draws the ProjectNeb bar on every monitor and stays resident.
  Stop it with `neb-cli` or by ending the process: the bar has no window to close.
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
            eprintln!("neb-shell: {error}");
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };

    if args.report_after.is_some() {
        // The binary is a GUI program, so there is no console of its own; borrowing the parent's is
        // what lets the report reach the terminal that started it.
        attach_parent_console();
    }

    // A second shell must not fight the first one for the edge, so it leaves silently and successfully:
    // an error here would make every launcher report a failure.
    let instance = Instance::acquire();
    if !instance.is_primary() {
        return ExitCode::SUCCESS;
    }

    match app::run(args) {
        Ok(report) => {
            if args.report_after.is_some() {
                print_report(&report);
            }

            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("neb-shell: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Borrows the console of the process that started this one, so `println!` has somewhere to go.
fn attach_parent_console() {
    use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};

    // SAFETY: the call only attaches this process to the console of its parent; the constant is the
    // documented "parent process" value.
    let _ = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) };
}

/// Prints the idle working set against the phase-2 budget.
fn print_report(report: &app::Report) {
    let seconds = report.elapsed.as_secs_f64();
    let bars = report.bars;

    match report.working_set {
        Some(bytes) => {
            let mib = bytes as f64 / (1024.0 * 1024.0);
            println!(
                "neb-shell: working set {mib:.1} MiB ({bytes} bytes) after {seconds:.1} s idle, \
                 {bars} bar(s), {docks} dock(s){}",
                if report.visible { "" } else { ", hidden" },
                docks = report.docks,
            );
            println!(
                "neb-shell: memory budget {:.0} MiB: {}",
                MEMORY_BUDGET as f64 / (1024.0 * 1024.0),
                if bytes <= MEMORY_BUDGET { "ok" } else { "over" }
            );
        }
        None => println!("neb-shell: the working set could not be read"),
    }
}

/// The single-instance mutex, released when the process ends.
#[derive(Debug)]
struct Instance {
    handle: HANDLE,
    primary: bool,
}

impl Instance {
    /// Takes the mutex, or reports that another shell already holds it.
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
                    // SAFETY: this handle is ours and is closed exactly once, here. On this path it is
                    // never stored, so `Drop` has nothing to close.
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
                // A mutex that cannot be created is not a reason to refuse to draw a bar.
                eprintln!("neb-shell: cannot create the instance mutex ({error}); continuing");
                Self {
                    handle: HANDLE::default(),
                    primary: true,
                }
            }
        }
    }

    /// Whether this process is the one shell.
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
