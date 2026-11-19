#![forbid(unsafe_op_in_unsafe_fn)]

//! `neb-zapret` — import a zapret pack, list its strategies, run one through the UAC helper.
//!
//! Modes:
//!
//! * no command, or `--serve`: answer `ProjectNeb.zapret` until Ctrl+C;
//! * `--import <folder>`: copy the user's pack into `%LocalAppData%\ProjectNeb\zapret`;
//! * `--list`: print the parsed strategies;
//! * `--state`: print the manager's state document;
//! * `--start <strategy>`: start it (through UAC when this process is not elevated);
//! * `--stop`: stop the recorded `winws.exe`;
//! * `--self-test`: run the BFE/WinDivert/proxy/hosts checks;
//! * `--run <strategy>`: the elevated helper the manager invokes (not for people);
//! * `--stop --pack <dir> --status <file>`: the elevated stop helper.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use neb_zapret::host::{Host, RealHost};
use neb_zapret::ipc;
use neb_zapret::manager::{Manager, StateDoc, StrategyList};
use neb_zapret::selftest::Report;
use serde::Serialize;
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::core::BOOL;

/// How long a command waits for an elevated helper before it prints what it has and exits.
const DEFAULT_WAIT: Duration = Duration::from_secs(120);

/// Poll interval while waiting for the helper.
const POLL: Duration = Duration::from_millis(100);

const USAGE: &str = "\
usage: neb-zapret [command] [--pack <dir>] [--status <file>] [--wait <seconds>] [--json]

  (no command)          answer the 'ProjectNeb.zapret' pipe until Ctrl+C
  --serve [--pipe <n>]  the same, on an explicit pipe name
  --import <folder>     copy a downloaded zapret pack into the data directory
  --list                print the strategies of the current pack
  --state               print the state document
  --start <strategy>    start a strategy; asks for UAC when not elevated
  --stop                stop the recorded winws.exe; asks for UAC when not elevated
  --self-test           check BFE, WinDivert, the proxy and the hosts file
  --run <strategy>      helper mode: start winws.exe and write status.json (needs admin)
  --stop --pack <dir> --status <file>
                        helper mode: terminate the recorded pid (needs admin)
  --version             print the version
  -h, --help            print this text
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match run(&arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("neb-zapret: {error}");
            ExitCode::from(2)
        }
    }
}

/// One parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Serve { pipe: String },
    Import(String),
    List,
    State,
    Start(String),
    Stop,
    SelfTest,
    Run(String),
    Version,
    Help,
}

#[derive(Debug, Clone)]
struct Options {
    command: Option<Command>,
    pack: Option<String>,
    status: Option<String>,
    pipe: Option<String>,
    wait: Duration,
    json: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            command: None,
            pack: None,
            status: None,
            pipe: None,
            wait: DEFAULT_WAIT,
            json: false,
        }
    }
}

fn run(arguments: &[String]) -> Result<ExitCode, String> {
    let options = parse(arguments)?;

    match options.command.clone() {
        None => serve(options),
        Some(Command::Help) => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Version) => {
            println!("neb-zapret {}", neb_zapret::VERSION);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Serve { pipe }) => serve(Options {
            command: Some(Command::Serve { pipe }),
            ..options
        }),
        Some(Command::Run(strategy)) => run_helper(&strategy, &options),
        Some(Command::Stop) if options.pack.is_some() || options.status.is_some() => {
            stop_helper(&options)
        }
        Some(Command::Import(source)) => import(&source, &options),
        Some(Command::List) => list(&options),
        Some(Command::State) => state(&options),
        Some(Command::Start(strategy)) => start(&strategy, &options),
        Some(Command::Stop) => stop(&options),
        Some(Command::SelfTest) => self_test(&options),
    }
}

fn parse(arguments: &[String]) -> Result<Options, String> {
    let mut options = Options::default();
    let mut index = 0;

    while index < arguments.len() {
        let argument = arguments[index].as_str();

        match argument {
            "-h" | "--help" => set_command(&mut options, Command::Help)?,
            "--version" => set_command(&mut options, Command::Version)?,
            "--serve" => {
                let pipe = ipc::PIPE.to_owned();
                set_command(&mut options, Command::Serve { pipe })?;
            }
            "--pipe" => options.pipe = Some(next(arguments, &mut index, "--pipe")?.to_owned()),
            "--import" => {
                let path = next(arguments, &mut index, "--import")?;
                set_command(&mut options, Command::Import(path.to_owned()))?;
            }
            "--list" => set_command(&mut options, Command::List)?,
            "--state" => set_command(&mut options, Command::State)?,
            "--start" => {
                let strategy = next(arguments, &mut index, "--start")?;
                set_command(&mut options, Command::Start(strategy.to_owned()))?;
            }
            "--stop" => set_command(&mut options, Command::Stop)?,
            "--self-test" => set_command(&mut options, Command::SelfTest)?,
            "--run" => {
                let strategy = next(arguments, &mut index, "--run")?;
                set_command(&mut options, Command::Run(strategy.to_owned()))?;
            }
            "--pack" => options.pack = Some(next(arguments, &mut index, "--pack")?.to_owned()),
            "--status" => {
                options.status = Some(next(arguments, &mut index, "--status")?.to_owned())
            }
            "--wait" => {
                let seconds = next(arguments, &mut index, "--wait")?;
                let seconds: u64 = seconds
                    .parse()
                    .map_err(|_| format!("'--wait' needs seconds, got '{seconds}'"))?;
                options.wait = Duration::from_secs(seconds);
            }
            "--json" => options.json = true,
            other => return Err(format!("unknown argument '{other}'\n\n{USAGE}")),
        }

        index += 1;
    }

    if let Some(pipe) = options.pipe.take() {
        match &options.command {
            None => options.command = Some(Command::Serve { pipe }),
            Some(Command::Serve { .. }) => options.command = Some(Command::Serve { pipe }),
            Some(_) => return Err("'--pipe' only applies to '--serve'".to_owned()),
        }
    }

    Ok(options)
}

fn set_command(options: &mut Options, command: Command) -> Result<(), String> {
    if options.command.is_some() {
        return Err("give exactly one command; `--help` lists them".to_owned());
    }

    options.command = Some(command);
    Ok(())
}

fn next<'a>(arguments: &'a [String], index: &mut usize, flag: &str) -> Result<&'a str, String> {
    *index += 1;
    arguments
        .get(*index)
        .map(String::as_str)
        .ok_or_else(|| format!("'{flag}' needs a value"))
}

fn manager() -> Result<Manager<RealHost>, String> {
    Manager::open(RealHost::new()).map_err(|error| error.to_string())
}

fn import(source: &str, options: &Options) -> Result<ExitCode, String> {
    let mut manager = manager()?;
    let state = manager
        .import(std::path::Path::new(source))
        .map_err(|error| error.to_string())?;

    emit(&state, options)?;
    Ok(ExitCode::SUCCESS)
}

fn list(options: &Options) -> Result<ExitCode, String> {
    let mut manager = manager()?;
    let list: StrategyList = manager
        .list_strategies()
        .map_err(|error| error.to_string())?;

    if options.json {
        emit(&list, options)?;
        return Ok(ExitCode::SUCCESS);
    }

    if !list.installed {
        println!("no zapret pack is imported");
        return Ok(ExitCode::FAILURE);
    }

    println!("pack: {} (imported)", list.pack_version);
    for strategy in &list.strategies {
        println!("{}\t{}", strategy.name, strategy.argument_count());
    }
    for skipped in &list.skipped {
        eprintln!("skipped {}: {}", skipped.file, skipped.reason);
    }

    Ok(ExitCode::SUCCESS)
}

fn state(options: &Options) -> Result<ExitCode, String> {
    let mut manager = manager()?;
    let state = manager.state();
    emit(&state, options)?;
    Ok(ExitCode::SUCCESS)
}

fn start(strategy: &str, options: &Options) -> Result<ExitCode, String> {
    let mut manager = manager()?;
    let started = manager.start(strategy).map_err(|error| error.to_string())?;

    eprintln!("{}", started.message);
    let state = wait_start(&mut manager, options.wait);
    emit(&state, options)?;

    if state.last_error.is_some() && !state.running && !state.pending {
        return Ok(ExitCode::FAILURE);
    }

    Ok(ExitCode::SUCCESS)
}

fn stop(options: &Options) -> Result<ExitCode, String> {
    let mut manager = manager()?;
    let stopping = manager.stop().map_err(|error| error.to_string())?;

    eprintln!("{}", stopping.message);
    let state = wait_stop(&mut manager, options.wait);
    emit(&state, options)?;

    if state.running {
        return Ok(ExitCode::FAILURE);
    }

    Ok(ExitCode::SUCCESS)
}

fn self_test(options: &Options) -> Result<ExitCode, String> {
    let mut manager = manager()?;
    let report: Report = manager.self_test();

    if options.json {
        emit(&report, options)?;
    } else {
        for check in &report.checks {
            println!("{:<10} {:?}: {}", check.id, check.status, check.message);
        }
    }

    if report.ok {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

fn run_helper(strategy: &str, options: &Options) -> Result<ExitCode, String> {
    let pack = options.pack.as_deref().map(std::path::Path::new);
    let status = options.status.as_deref().map(std::path::Path::new);

    match neb_zapret::helper::run(strategy, pack, status) {
        Ok(file) => {
            emit(&file, options)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            eprintln!("neb-zapret: {error}");
            Ok(ExitCode::FAILURE)
        }
    }
}

fn stop_helper(options: &Options) -> Result<ExitCode, String> {
    let pack = options.pack.as_deref().map(std::path::Path::new);
    let status = options.status.as_deref().map(std::path::Path::new);

    match neb_zapret::helper::stop(pack, status) {
        Ok(file) => {
            emit(&file, options)?;
            Ok(ExitCode::SUCCESS)
        }
        Err(error) => {
            eprintln!("neb-zapret: {error}");
            Ok(ExitCode::FAILURE)
        }
    }
}

fn serve(options: Options) -> Result<ExitCode, String> {
    let pipe = match options.command {
        Some(Command::Serve { pipe }) => pipe,
        _ => ipc::PIPE.to_owned(),
    };
    let manager = manager()?;
    let shared = Arc::new(Mutex::new(manager));
    let server = ipc::serve(Arc::clone(&shared), &pipe).map_err(|error| error.to_string())?;

    eprintln!("neb-zapret: listening on '{pipe}'");
    install_ctrl_handler();

    while !STOP.load(Ordering::SeqCst) {
        std::thread::sleep(POLL);
    }

    server.shutdown();
    Ok(ExitCode::SUCCESS)
}

/// Set by the console control handler when the user presses Ctrl+C.
static STOP: AtomicBool = AtomicBool::new(false);

fn install_ctrl_handler() {
    // SAFETY: the handler has the signature Windows expects, touches only an atomic, and stays
    // installed for the rest of the process.
    unsafe extern "system" fn handler(_ctrl: u32) -> BOOL {
        STOP.store(true, Ordering::SeqCst);
        BOOL(1)
    }

    // SAFETY: the function pointer is valid for the lifetime of the process.
    unsafe {
        let _ = SetConsoleCtrlHandler(Some(handler), true);
    }
}

fn wait_start<H: Host>(manager: &mut Manager<H>, wait: Duration) -> StateDoc {
    let deadline = Instant::now() + wait;

    loop {
        let state = manager.state();
        let settled = !state.pending && (state.running || state.last_error.is_some());

        if settled || Instant::now() >= deadline {
            return state;
        }

        std::thread::sleep(POLL);
    }
}

fn wait_stop<H: Host>(manager: &mut Manager<H>, wait: Duration) -> StateDoc {
    let deadline = Instant::now() + wait;

    loop {
        let state = manager.state();
        let settled = !state.running && !state.stopping && !state.pending;

        if settled || Instant::now() >= deadline {
            return state;
        }

        std::thread::sleep(POLL);
    }
}

fn emit<T: Serialize>(value: &T, options: &Options) -> Result<(), String> {
    if options.json {
        let line = serde_json::to_string(value).map_err(|error| error.to_string())?;
        println!("{line}");
    } else {
        let pretty = serde_json::to_string_pretty(value).map_err(|error| error.to_string())?;
        println!("{pretty}");
    }

    Ok(())
}
