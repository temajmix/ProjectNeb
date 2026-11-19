#![forbid(unsafe_op_in_unsafe_fn)]
// The updater is started by the supervisor as the supervisor is closing: a console window appearing
// in the middle of that is a bug the user sees. WinExe is the PE subsystem Windows calls GUI.
#![windows_subsystem = "windows"]

//! `neb-updater` — wait, copy, restart. The library does the work; this file owns the exit code.
//!
//! Exit codes: `0` the update was applied and the supervisor restarted, `1` something failed, `2` the
//! waited-for pid outlived the timeout and **nothing was copied**. `2` is separate from `1` because
//! the supervisor can treat it as "try again later" without wondering whether the installation is
//! half updated.

use std::process::ExitCode;

use neb_updater::apply::{self, RetryPolicy};
use neb_updater::args::{Options, Parsed, USAGE};
use neb_updater::log::{self, Log};
use neb_updater::process::{self, WaitOutcome};

/// `1`: the command line was wrong or a step failed.
const FAILED: u8 = 1;

/// `2`: the process this run waits for did not exit; the installation was not touched.
const TIMED_OUT: u8 = 2;

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();

    let options = match Options::parse(&argv) {
        Ok(Parsed::Run(options)) => options,
        Ok(Parsed::Help) => {
            eprintln!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("neb-updater: {error}\n\n{USAGE}");
            return ExitCode::from(FAILED);
        }
    };

    let log = open_log(&options);
    run(&options, &log)
}

/// Opens the log, falling back to a silent sink: an update must not be stopped by its own diary.
fn open_log(options: &Options) -> Log {
    let path = options
        .log
        .clone()
        .unwrap_or_else(|| log::default_path(&options.app));

    match Log::open(&path) {
        Ok(log) => log,
        Err(error) => {
            eprintln!("neb-updater: cannot open '{}': {error}", path.display());
            Log::silent(path)
        }
    }
}

/// Runs the three steps and turns the outcome into an exit code.
fn run(options: &Options, log: &Log) -> ExitCode {
    log.info(
        "start",
        &format!(
            "neb-updater {} waiting for pid {} (timeout {} s), staging '{}', app '{}'",
            env!("CARGO_PKG_VERSION"),
            options.wait_pid,
            options.timeout.as_secs(),
            options.staging.display(),
            options.app.display()
        ),
    );

    match process::wait_for_pid(options.wait_pid, options.timeout) {
        Ok(WaitOutcome::Exited) => {
            log.info("wait", &format!("pid {} exited", options.wait_pid));
        }
        Ok(WaitOutcome::AlreadyGone) => {
            log.info(
                "wait",
                &format!("pid {} was already gone", options.wait_pid),
            );
        }
        Ok(WaitOutcome::TimedOut) => {
            log.error(
                "wait",
                &format!(
                    "pid {} is still running after {} s; nothing was copied",
                    options.wait_pid,
                    options.timeout.as_secs()
                ),
            );
            eprintln!(
                "neb-updater: pid {} is still running; nothing was copied",
                options.wait_pid
            );
            return ExitCode::from(TIMED_OUT);
        }
        Err(error) => {
            log.error("wait", &format!("{error}; nothing was copied"));
            eprintln!("neb-updater: {error}");
            return ExitCode::from(TIMED_OUT);
        }
    }

    let steps = match apply::plan(&options.staging, &options.app) {
        Ok(steps) => steps,
        Err(error) => {
            log.error(
                "plan",
                &format!(
                    "cannot read staging '{}': {error}",
                    options.staging.display()
                ),
            );
            eprintln!("neb-updater: cannot read staging: {error}");
            return ExitCode::from(FAILED);
        }
    };
    log.info(
        "plan",
        &format!(
            "{} file(s) from '{}' to '{}'",
            steps.len(),
            options.staging.display(),
            options.app.display()
        ),
    );

    match apply::apply(&steps, RetryPolicy::default(), log) {
        Ok(summary) => log.info(
            "copy",
            &format!(
                "{} file(s) and {} byte(s) written, {} retries",
                summary.files, summary.bytes, summary.retries
            ),
        ),
        Err(error) => {
            log.error("copy", &format!("{error}"));
            eprintln!("neb-updater: {error}");
            return ExitCode::from(FAILED);
        }
    }

    log.info(
        "restart",
        &format!(
            "starting '{}' in '{}'",
            options.restart.display(),
            options.app.display()
        ),
    );

    match process::restart(&options.restart, &options.restart_args, &options.app) {
        Ok(pid) => {
            log.info("done", &format!("started pid {pid}; update complete"));
            ExitCode::SUCCESS
        }
        Err(error) => {
            log.error("restart", &format!("{error}"));
            eprintln!("neb-updater: {error}");
            ExitCode::from(FAILED)
        }
    }
}
