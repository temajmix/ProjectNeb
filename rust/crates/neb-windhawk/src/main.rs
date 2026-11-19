#![forbid(unsafe_op_in_unsafe_fn)]

//! `neb-windhawk` — browse the Windhawk mod catalog, install sources and report installed mods.
//!
//! Modes:
//!
//! * no command, or `--serve`: answer `ProjectNeb.windhawk` until Ctrl+C;
//! * `--state`: print the state document;
//! * `--list-catalog [--query <text>] [--limit <n>]`: print a catalog page;
//! * `--list`: print installed sources, compiled DLLs and enabled state;
//! * `--install <id>`: download and install through the helper (UAC when needed);
//! * `--remove <id>`: delete the source, configuration key and compiled DLLs;
//! * `--open [--tray-only]`: start Windhawk;
//! * `--helper --plan <file>`: the elevated half the manager invokes.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use neb_windhawk::host::RealHost;
use neb_windhawk::ipc;
use neb_windhawk::manager::{DEFAULT_LIMIT, Manager};
use neb_windhawk::registry::RealRegistry;
use serde::Serialize;
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::core::BOOL;

/// Poll interval of the serve loop.
const POLL: Duration = Duration::from_millis(100);

const USAGE: &str = "\
usage: neb-windhawk [command] [--json]

  (no command)          answer the 'ProjectNeb.windhawk' pipe until Ctrl+C
  --serve [--pipe <n>]  the same, on an explicit pipe name
  --state               print the state document
  --list-catalog        print the catalog page (--query <text>, --limit <n>)
  --list                print the installed mods
  --install <id>        download and install a mod
  --remove <id>         remove a mod
  --open                start Windhawk; --tray-only starts it in the tray
  --helper --plan <f>   helper mode: apply one plan file (needs admin)
  --version             print the version
  -h, --help            print this text
";

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();

    match run(&arguments) {
        Ok(code) => code,
        Err(error) => {
            eprintln!("neb-windhawk: {error}");
            ExitCode::from(2)
        }
    }
}

/// One parsed command line.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Command {
    Serve { pipe: String },
    State,
    ListCatalog,
    List,
    Install(String),
    Remove(String),
    Open,
    Helper,
    Version,
    Help,
}

#[derive(Debug, Clone)]
struct Options {
    command: Option<Command>,
    query: String,
    limit: usize,
    tray_only: bool,
    plan: Option<String>,
    pipe: Option<String>,
    json: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            command: None,
            query: String::new(),
            limit: DEFAULT_LIMIT,
            tray_only: false,
            plan: None,
            pipe: None,
            json: false,
        }
    }
}

fn run(arguments: &[String]) -> Result<ExitCode, String> {
    let options = parse(arguments)?;

    // Logging is best effort: a module that refuses to start because its log folder is missing is
    // worse than one that runs silently.
    let _log = neb_core::logging::init(&neb_core::paths::logs_dir()).ok();

    match options.command.clone() {
        None => serve(options),
        Some(Command::Help) => {
            print!("{USAGE}");
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Version) => {
            println!("neb-windhawk {}", neb_windhawk::VERSION);
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Serve { pipe }) => serve(Options {
            command: Some(Command::Serve { pipe }),
            ..options
        }),
        Some(Command::Helper) => helper(&options),
        Some(Command::State) => state(&options),
        Some(Command::ListCatalog) => list_catalog(&options),
        Some(Command::List) => list(&options),
        Some(Command::Install(id)) => install(&id, &options),
        Some(Command::Remove(id)) => remove(&id, &options),
        Some(Command::Open) => open(&options),
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
            "--serve" => set_command(
                &mut options,
                Command::Serve {
                    pipe: ipc::PIPE.to_owned(),
                },
            )?,
            "--pipe" => options.pipe = Some(next(arguments, &mut index, "--pipe")?.to_owned()),
            "--state" => set_command(&mut options, Command::State)?,
            "--list-catalog" => set_command(&mut options, Command::ListCatalog)?,
            "--list" => set_command(&mut options, Command::List)?,
            "--install" => {
                let id = next(arguments, &mut index, "--install")?;
                set_command(&mut options, Command::Install(id.to_owned()))?;
            }
            "--remove" => {
                let id = next(arguments, &mut index, "--remove")?;
                set_command(&mut options, Command::Remove(id.to_owned()))?;
            }
            "--open" => set_command(&mut options, Command::Open)?,
            "--helper" => set_command(&mut options, Command::Helper)?,
            "--plan" => options.plan = Some(next(arguments, &mut index, "--plan")?.to_owned()),
            "--query" => options.query = next(arguments, &mut index, "--query")?.to_owned(),
            "--limit" => {
                let limit = next(arguments, &mut index, "--limit")?;
                options.limit = limit
                    .parse()
                    .map_err(|_| format!("'--limit' needs a number, got '{limit}'"))?;
            }
            "--tray-only" => options.tray_only = true,
            "--json" => options.json = true,
            other => return Err(format!("unknown argument '{other}'\n\n{USAGE}")),
        }

        index += 1;
    }

    if let Some(pipe) = options.pipe.take() {
        match &options.command {
            None | Some(Command::Serve { .. }) => {
                options.command = Some(Command::Serve { pipe });
            }
            Some(_) => return Err("'--pipe' only applies to '--serve'".to_owned()),
        }
    }

    if matches!(options.command, Some(Command::Helper)) && options.plan.is_none() {
        return Err("'--helper' needs '--plan <file>'".to_owned());
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
    Manager::open(RealHost::new(), Box::new(RealRegistry)).map_err(|error| error.to_string())
}

fn state(options: &Options) -> Result<ExitCode, String> {
    let manager = manager()?;
    emit(&manager.state(), options)?;
    Ok(ExitCode::SUCCESS)
}

fn list_catalog(options: &Options) -> Result<ExitCode, String> {
    let manager = manager()?;
    let page = manager
        .list_catalog(&options.query, options.limit)
        .map_err(|error| error.to_string())?;

    emit(&page, options)?;
    Ok(ExitCode::SUCCESS)
}

fn list(options: &Options) -> Result<ExitCode, String> {
    let manager = manager()?;
    let report = manager.list_installed();

    if options.json {
        emit(&report, options)?;
        return Ok(ExitCode::SUCCESS);
    }

    for entry in &report.mods {
        let enabled = match entry.enabled {
            Some(true) => "enabled",
            Some(false) => "disabled",
            None => "unknown",
        };
        let dlls = entry.compiled.len();
        println!("{}\t{}\t{enabled} ({dlls} dll)", entry.id, entry.name);
    }
    if report.mods.is_empty() {
        println!("no Windhawk mods are installed");
    }
    for issue in &report.issues {
        eprintln!("{issue}");
    }

    Ok(ExitCode::SUCCESS)
}

fn install(id: &str, options: &Options) -> Result<ExitCode, String> {
    let manager = manager()?;
    let outcome = manager.install(id).map_err(|error| error.to_string())?;

    eprintln!(
        "{} installed; enabled: {}{}",
        outcome.id,
        outcome.enabled,
        if outcome.needs_windhawk_ui {
            " (confirm it in Windhawk)"
        } else {
            ""
        }
    );
    emit(&outcome, options)?;
    Ok(ExitCode::SUCCESS)
}

fn remove(id: &str, options: &Options) -> Result<ExitCode, String> {
    let manager = manager()?;
    let outcome = manager.remove(id).map_err(|error| error.to_string())?;

    eprintln!("{} removed", outcome.id);
    emit(&outcome, options)?;
    Ok(ExitCode::SUCCESS)
}

fn open(options: &Options) -> Result<ExitCode, String> {
    let manager = manager()?;
    manager.launch(options.tray_only)?;

    eprintln!(
        "Windhawk started{}",
        if options.tray_only {
            " in the tray"
        } else {
            ""
        }
    );
    Ok(ExitCode::SUCCESS)
}

fn helper(options: &Options) -> Result<ExitCode, String> {
    let plan = options
        .plan
        .as_deref()
        .ok_or_else(|| "'--helper' needs '--plan <file>'".to_owned())?;

    match neb_windhawk::helper::run_plan_file(std::path::Path::new(plan), &RealRegistry) {
        Ok(result) => {
            emit(&result, options)?;
            if result.ok {
                Ok(ExitCode::SUCCESS)
            } else {
                Ok(ExitCode::FAILURE)
            }
        }
        Err(error) => Err(error.to_string()),
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

    eprintln!("neb-windhawk: listening on '{pipe}'");
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
