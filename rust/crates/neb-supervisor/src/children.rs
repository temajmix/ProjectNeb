//! The lifecycle of one managed process: start it, notice the exit, decide when to try again.
//!
//! Three rules shape this module, and each of them is visible in the C# supervisor it replaces:
//! a child is never started twice (a named mutex per child), a crash is retried at most once per
//! [`RESTART_THROTTLE`], and repeated crashes back off through [`BACKOFF_STEPS`] so a broken build
//! cannot turn into a busy loop.
//!
//! The timer arithmetic lives in [`RestartPlanner`], which is a plain value type with no Win32 in
//! it: every decision is a function of two durations, so the tests exercise the policy directly
//! instead of sleeping.

use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use windows::Win32::Foundation::{
    CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, WAIT_OBJECT_0,
};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};
use windows::Win32::System::Threading::{
    CreateMutexW, CreateProcessW, GetExitCodeProcess, INFINITE, OpenProcess, PROCESS_INFORMATION,
    PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    QueryFullProcessImageNameW, STARTUPINFOW, TerminateProcess, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

/// A child that stayed up this long is healthy: the next crash restarts it on the first step.
pub const STABLE_RUN: Duration = Duration::from_secs(120);

/// Never two starts of the same child within this window, whatever the backoff says.
pub const RESTART_THROTTLE: Duration = Duration::from_secs(30);

/// Wait before the next start, indexed by the number of consecutive short crashes.
pub const BACKOFF_STEPS: [Duration; 3] = [
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(300),
];

/// How long a child gets to die after `TerminateProcess` before the supervisor moves on.
const STOP_BUDGET: Duration = Duration::from_secs(2);

/// Pause between polls of a dying child inside [`Child::stop`].
const STOP_POLL: Duration = Duration::from_millis(50);

/// Longest image path [`QueryFullProcessImageNameW`] is asked for.
const PATH_UNITS: usize = 32_768;

/// Waits that [`RestartPlanner`] hands out: `step` is the backoff, `delay` what is actually used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestartDelay {
    /// The backoff step for this crash, before the throttle is applied.
    pub step: Duration,
    /// How long to wait before starting the child again.
    pub delay: Duration,
}

/// The restart policy: how many short crashes happened in a row, and how long to wait next.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RestartPlanner {
    short_crashes: u32,
}

impl RestartPlanner {
    /// How many consecutive short runs have ended in a crash.
    pub fn short_crashes(&self) -> u32 {
        self.short_crashes
    }

    /// Forgets the crash history, which is what a manual restart asks for.
    pub fn reset(&mut self) {
        self.short_crashes = 0;
    }

    /// Decides the wait before restarting a child that ran for `ran_for`.
    ///
    /// The throttle and the backoff both count from the same instant — the last start — so one
    /// duration is enough to answer both questions: a run of `ran_for` was started `ran_for` ago.
    ///
    /// A run of at least [`STABLE_RUN`] resets the ladder, so a child that worked for hours and
    /// then died is restarted in [`RESTART_THROTTLE`], not in five minutes.
    pub fn on_crash(&mut self, ran_for: Duration) -> RestartDelay {
        if ran_for >= STABLE_RUN {
            self.short_crashes = 0;
        }

        let step = BACKOFF_STEPS[(self.short_crashes as usize).min(BACKOFF_STEPS.len() - 1)];
        self.short_crashes = self.short_crashes.saturating_add(1);

        RestartDelay {
            step,
            delay: step.max(RESTART_THROTTLE.saturating_sub(ran_for)),
        }
    }
}

/// Which children the configuration asks for.
///
/// The keys are read from the raw JSON on purpose: `shell.enabled`, `wallpapers.enabled` and
/// `discord.enabled` are additive keys this build writes and the C# build ignores, so the typed
/// [`Config`] carries them in `extra` rather than as fields, and a missing key must mean "the
/// shipped default" rather than "whatever the typed default happens to be".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Enabled {
    /// Run `neb-shell.exe`.
    pub shell: bool,
    /// Run `neb-wallpaper.exe`.
    pub wallpaper: bool,
    /// Run `neb-discord.exe`.
    pub discord: bool,
    /// Run `neb-wm.exe`.
    pub wm: bool,
    /// Run `neb-zapret.exe`.
    pub zapret: bool,
    /// Run `neb-windhawk.exe`.
    pub windhawk: bool,
}

impl Default for Enabled {
    fn default() -> Self {
        Self {
            shell: true,
            wallpaper: false,
            discord: false,
            wm: false,
            zapret: false,
            windhawk: false,
        }
    }
}

impl Enabled {
    /// Reads the three switches out of a parsed `config.json`.
    pub fn from_json(config: &Value) -> Self {
        Self {
            // `shell.enabled` is this build's key; the C#-compatible file spells the same idea
            // `shell.barEnabled`, so either one turns the shell on.
            shell: switch(config, "shell", &["barEnabled"]).unwrap_or(true),
            wallpaper: switch(config, "wallpapers", &[]).unwrap_or(false),
            discord: switch(config, "discord", &[]).unwrap_or(false),
            wm: switch(config, "windowManager", &[]).unwrap_or(false),
            zapret: switch(config, "zapret", &[]).unwrap_or(false),
            windhawk: switch(config, "windhawk", &[]).unwrap_or(false),
        }
    }

    /// Reads the switches out of the config file, falling back to the defaults for anything that
    /// cannot be read: a supervisor that refuses to start because of a broken config is worse than
    /// one that starts the bar the user has always had.
    pub fn from_file(path: &Path) -> Self {
        let Ok(bytes) = std::fs::read(path) else {
            return Self::default();
        };
        // The C# build writes `config.json` with a UTF-8 BOM, and `serde_json` refuses it.
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        match serde_json::from_str::<Value>(text) {
            Ok(value) => Self::from_json(&value),
            Err(_) => Self::default(),
        }
    }
}

/// One `enabled` switch, or the first alternative key, or `None` when none are present.
fn switch(config: &Value, section: &str, alternatives: &[&str]) -> Option<bool> {
    let section = config.get(section)?;
    if let Some(value) = section.get("enabled").and_then(Value::as_bool) {
        return Some(value);
    }
    for key in alternatives {
        if let Some(value) = section.get(*key).and_then(Value::as_bool) {
            return Some(value);
        }
    }
    None
}

/// What a child is and how to run it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildSpec {
    /// Short name used in logs and in `supervisor.restart`.
    pub name: String,
    /// Executable to run.
    pub exe: PathBuf,
    /// Arguments after the executable.
    pub args: Vec<String>,
    /// Name of the mutex that guarantees one copy of this child per desktop session.
    pub mutex: String,
    /// Whether the configuration asks for this child at all.
    pub enabled: bool,
    /// Whether a copy that already runs from this executable should be adopted instead of spawned.
    ///
    /// Only the real children next to the supervisor set this: a diagnostic `--spawn-test` command
    /// names a shared executable such as `cmd.exe`, and adopting an unrelated `cmd.exe` would let
    /// the supervisor kill a process the user started.
    pub adopt: bool,
}

impl ChildSpec {
    /// A child called `name` running `exe`, enabled or not, with the standard mutex name.
    pub fn new(name: &str, exe: PathBuf, enabled: bool) -> Self {
        Self {
            name: name.to_owned(),
            exe,
            args: Vec::new(),
            mutex: format!(r"Local\ProjectNeb.supervisor.child.{name}"),
            enabled,
            adopt: false,
        }
    }

    /// The same child, with adoption of an already-running copy turned on.
    pub fn adopting(mut self) -> Self {
        self.adopt = true;
        self
    }

    /// A child running an arbitrary command line, used by `--spawn-test`.
    pub fn command(exe: PathBuf, args: Vec<String>) -> Self {
        Self {
            args,
            ..Self::new("test", exe, true)
        }
    }
}

/// Why a child could not be started or a mutex could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum ChildError {
    /// `CreateProcessW` refused.
    #[error("cannot start '{}': {source}", exe.display())]
    Spawn {
        /// Executable that was about to run.
        exe: PathBuf,
        /// The Win32 error.
        #[source]
        source: windows::core::Error,
    },

    /// `CreateMutexW` refused.
    #[error("cannot create the mutex '{name}': {source}")]
    Mutex {
        /// Name that could not be created.
        name: String,
        /// The Win32 error.
        #[source]
        source: windows::core::Error,
    },

    /// Another supervisor already owns this child slot.
    #[error("'{0}' is already owned by another ProjectNeb process")]
    Owned(String),
}

/// One exit the supervisor has to react to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exit {
    /// Process id that ended.
    pub pid: u32,
    /// Exit code Windows reports.
    pub code: u32,
    /// How long the child ran before it died.
    pub ran_for: Duration,
}

/// A named mutex held for as long as this process owns the slot it guards.
#[derive(Debug)]
pub struct NamedMutex {
    handle: HANDLE,
}

impl NamedMutex {
    /// Creates or opens the mutex `name`.
    ///
    /// The second value is `true` when this call created the object, i.e. when no other process in
    /// the session holds the slot. The handle is released by [`Drop`], which is exactly the moment
    /// the slot becomes free again — a crashed holder leaves nothing behind.
    ///
    /// # Errors
    ///
    /// [`ChildError::Mutex`] when Win32 refuses to create the object.
    pub fn acquire(name: &str) -> Result<(Self, bool), ChildError> {
        let wide = wide(name);

        // SAFETY: `wide` is a NUL-terminated UTF-16 buffer that outlives the call, no security
        // attributes are asked for and the mutex is created unowned, so nothing is left waiting.
        let handle =
            unsafe { CreateMutexW(None, false, PCWSTR(wide.as_ptr())) }.map_err(|source| {
                ChildError::Mutex {
                    name: name.to_owned(),
                    source,
                }
            })?;

        // SAFETY: the call above just succeeded; asking for the last error is how Win32 reports
        // that the object already existed.
        let first = unsafe { GetLastError() } != ERROR_ALREADY_EXISTS;

        Ok((Self { handle }, first))
    }
}

impl Drop for NamedMutex {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateMutexW` and is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// A running process the supervisor owns and reaps.
#[derive(Debug)]
pub struct Process {
    pid: u32,
    handle: HANDLE,
}

impl Process {
    /// Starts `exe` with `args` in `directory`.
    ///
    /// A bare name such as `cmd` is resolved against `PATH` first (see [`resolve_executable`]),
    /// because Windows only appends `.exe` and searches for a module named on the command line, not
    /// for one named in the application slot.
    ///
    /// # Errors
    ///
    /// [`ChildError::Spawn`] when Win32 refuses, which includes a missing executable.
    pub fn spawn(exe: &Path, args: &[String], directory: &Path) -> Result<Self, ChildError> {
        let resolved = resolve_executable(exe, std::env::var_os("PATH").as_deref());
        let application = wide(&resolved.to_string_lossy());
        let mut command_line = wide(&command_line(&resolved, args));
        let directory = wide(&directory.to_string_lossy());

        let startup = STARTUPINFOW {
            cb: size_of::<STARTUPINFOW>() as u32,
            ..STARTUPINFOW::default()
        };
        let mut info = PROCESS_INFORMATION::default();

        // SAFETY: every pointer is a live, NUL-terminated buffer for the duration of the call, the
        // startup structure declares its own size and the process information is a valid
        // out-parameter. The child inherits nothing, so no handle of ours leaks into it.
        let started = unsafe {
            CreateProcessW(
                PCWSTR(application.as_ptr()),
                Some(PWSTR(command_line.as_mut_ptr())),
                None,
                None,
                false,
                Default::default(),
                None,
                PCWSTR(directory.as_ptr()),
                &startup,
                &mut info,
            )
        };

        if let Err(source) = started {
            return Err(ChildError::Spawn {
                exe: exe.to_path_buf(),
                source,
            });
        }

        // SAFETY: the thread handle is not used by this module and is released once.
        unsafe {
            let _ = CloseHandle(info.hThread);
        }

        Ok(Self {
            pid: info.dwProcessId,
            handle: info.hProcess,
        })
    }

    /// Process id Windows assigned.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Takes over a process this supervisor did not start.
    ///
    /// The handle is opened with the three rights the child lifecycle needs: wait on it, read its
    /// exit code, and stop it on `app.quit`. A process that has already ended is not adopted, so a
    /// stale pid cannot turn into a phantom "running" child.
    pub fn adopt(pid: u32) -> Option<Self> {
        let access = PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE;

        // SAFETY: the call takes a plain pid and returns a handle owned by this value.
        let handle = unsafe { OpenProcess(access, false, pid) }.ok()?;
        let process = Self { pid, handle };

        (!process.has_exited()).then_some(process)
    }

    /// The pid of a process whose full image path is `exe`, matched case-insensitively.
    ///
    /// This is how a supervisor that starts after a crash finds the children the previous one left
    /// behind: a child's own single-instance mutex would make a second copy exit immediately.
    pub fn running_instance(exe: &Path) -> Option<u32> {
        let target = resolve_executable(exe, std::env::var_os("PATH").as_deref());
        let target = target.to_string_lossy().to_lowercase();
        let self_pid = std::process::id();

        // SAFETY: a process snapshot takes no pointer and returns a handle this function closes.
        let Ok(snapshot) = (unsafe { CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) }) else {
            return None;
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: size_of::<PROCESSENTRY32W>() as u32,
            ..PROCESSENTRY32W::default()
        };
        // SAFETY: `snapshot` is open and `entry` is live with its size declared in `dwSize`.
        let mut more = unsafe { Process32FirstW(snapshot, &mut entry) }.is_ok();
        let mut found = None;

        while more {
            let pid = entry.th32ProcessID;
            if pid != 0 && pid != self_pid {
                if let Some(path) = image_path(pid) {
                    if path.to_string_lossy().to_lowercase() == target {
                        found = Some(pid);
                        break;
                    }
                }
            }

            entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
            // SAFETY: same as the first call: `entry` stays live and correctly sized.
            more = unsafe { Process32NextW(snapshot, &mut entry) }.is_ok();
        }

        // SAFETY: the handle came from `CreateToolhelp32Snapshot` and is closed exactly once.
        unsafe {
            let _ = CloseHandle(snapshot);
        }

        found
    }

    /// Whether the process has ended, without blocking.
    pub fn has_exited(&self) -> bool {
        // SAFETY: the handle is a live process handle owned by this value; a zero timeout only
        // asks for the current state.
        (unsafe { WaitForSingleObject(self.handle, 0) }) == WAIT_OBJECT_0
    }

    /// The exit code, or `None` when the process is still running or Windows will not say.
    pub fn exit_code(&self) -> Option<u32> {
        let mut code = 0u32;
        // SAFETY: `code` is a live out-parameter and the handle is a live process handle.
        unsafe { GetExitCodeProcess(self.handle, &mut code) }.ok()?;
        Some(code)
    }

    /// Asks the process to die. A child that ignores this is killed by the kernel anyway, because
    /// `TerminateProcess` is not a request.
    pub fn terminate(&self) {
        // SAFETY: the handle is a live process handle owned by this value.
        unsafe {
            let _ = TerminateProcess(self.handle, 0);
        }
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        // SAFETY: the handle came from `CreateProcessW` and is closed exactly once, here.
        unsafe {
            let _ = CloseHandle(self.handle);
        }
    }
}

/// The full image path of `pid`, or `None` when the process cannot be queried.
fn image_path(pid: u32) -> Option<PathBuf> {
    // SAFETY: the call takes a plain pid and the returned handle is closed before returning.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }.ok()?;

    let mut buffer = [0u16; PATH_UNITS];
    let mut size = u32::try_from(buffer.len()).ok()?;
    // SAFETY: `buffer` and `size` are live; the call writes at most `size` units and reports how many.
    let queried = unsafe {
        QueryFullProcessImageNameW(
            process,
            PROCESS_NAME_FORMAT(0),
            PWSTR(buffer.as_mut_ptr()),
            &mut size,
        )
    };

    // SAFETY: the handle came from `OpenProcess` above and is closed exactly once, here.
    unsafe {
        let _ = CloseHandle(process);
    }

    if queried.is_err() || size == 0 {
        return None;
    }

    let path = PathBuf::from(String::from_utf16_lossy(
        &buffer[..usize::try_from(size).ok()?],
    ));

    (!path.as_os_str().is_empty()).then_some(path)
}

/// Blocks the calling thread until `pid` ends.
///
/// The waiter thread of an adopted child calls this: it holds a second `SYNCHRONIZE` handle so the
/// exit of a process this supervisor did not create is observed the same way as a spawned one. The
/// restart itself stays with the loop and its planner, so the throttle is never bypassed.
pub fn wait_for_exit(pid: u32) {
    // SAFETY: the call takes a plain pid and the handle is closed before returning.
    let Ok(handle) = (unsafe { OpenProcess(PROCESS_SYNCHRONIZE, false, pid) }) else {
        return;
    };

    // SAFETY: `handle` is a live synchronisation handle; `INFINITE` waits until the process ends.
    unsafe {
        let _ = WaitForSingleObject(handle, INFINITE);
        let _ = CloseHandle(handle);
    }
}

/// One managed child: its spec, the process it currently owns, and the restart state.
#[derive(Debug)]
pub struct Child {
    spec: ChildSpec,
    process: Option<Process>,
    planner: RestartPlanner,
    last_start: Option<Instant>,
    next_start: Option<Instant>,
    /// Whether `process` came from an earlier supervisor rather than from [`Child::start`].
    adopted: bool,
    /// Held for as long as this supervisor owns the slot; dropping it frees the slot for the next
    /// run, which is why the field is never read.
    _mutex: Option<NamedMutex>,
    blocked: Option<String>,
}

impl Child {
    /// Prepares a child without starting it, taking the mutex that guards its slot.
    ///
    /// A contended mutex is not an error: it means another ProjectNeb process already runs this
    /// child, so this supervisor must never start a second copy. The reason is kept for the log.
    ///
    /// When the mutex is free but the executable is already running, that process is adopted: the
    /// previous supervisor may have crashed and left its children behind, and their own
    /// single-instance mutexes would make a fresh copy exit immediately. Adoption is opt-in because
    /// a diagnostic command such as `cmd.exe` is shared with the rest of the session.
    pub fn new(spec: ChildSpec) -> Self {
        let (mutex, blocked) = match NamedMutex::acquire(&spec.mutex) {
            Ok((mutex, true)) => (Some(mutex), None),
            Ok((_mutex, false)) => (
                None,
                Some(format!(
                    "'{}' is already owned by another process",
                    spec.mutex
                )),
            ),
            Err(error) => (None, Some(error.to_string())),
        };

        let (process, adopted) = match &mutex {
            Some(_) if blocked.is_none() && spec.adopt => {
                match Process::running_instance(&spec.exe).and_then(Process::adopt) {
                    Some(process) => (Some(process), true),
                    None => (None, false),
                }
            }
            _ => (None, false),
        };
        let last_start = process.as_ref().map(|_| Instant::now());

        Self {
            spec,
            process,
            planner: RestartPlanner::default(),
            last_start,
            next_start: None,
            adopted,
            _mutex: mutex,
            blocked,
        }
    }

    /// Short name from the spec.
    pub fn name(&self) -> &str {
        &self.spec.name
    }

    /// Executable from the spec.
    pub fn exe(&self) -> &Path {
        &self.spec.exe
    }

    /// Whether the configuration asks for this child.
    pub fn enabled(&self) -> bool {
        self.spec.enabled
    }

    /// Whether the running process was adopted from an earlier supervisor.
    pub fn adopted(&self) -> bool {
        self.adopted
    }

    /// Why this child can never be started in this session, if it cannot.
    pub fn blocked(&self) -> Option<&str> {
        self.blocked.as_deref()
    }

    /// Whether the child is running under this supervisor right now.
    pub fn running(&self) -> bool {
        self.process
            .as_ref()
            .is_some_and(|process| !process.has_exited())
    }

    /// The pid of the running child.
    pub fn pid(&self) -> Option<u32> {
        self.process.as_ref().map(Process::pid)
    }

    /// How many short crashes happened in a row.
    pub fn short_crashes(&self) -> u32 {
        self.planner.short_crashes()
    }

    /// Whether a scheduled start is due at `now`.
    pub fn due(&self, now: Instant) -> bool {
        self.next_start.is_some_and(|at| at <= now)
    }

    /// Starts the child, clearing any scheduled start.
    ///
    /// # Errors
    ///
    /// [`ChildError::Owned`] when another process holds the slot, or [`ChildError::Spawn`] when the
    /// executable cannot run.
    pub fn start(&mut self) -> Result<u32, ChildError> {
        if let Some(reason) = &self.blocked {
            return Err(ChildError::Owned(reason.clone()));
        }

        if self.running() {
            return Ok(self.pid().unwrap_or_default());
        }

        let directory = self
            .spec
            .exe
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let process = Process::spawn(&self.spec.exe, &self.spec.args, &directory)?;

        let pid = process.pid();
        self.process = Some(process);
        self.adopted = false;
        self.last_start = Some(Instant::now());
        self.next_start = None;

        Ok(pid)
    }

    /// Reaps the child when it has ended; `None` while it is still running.
    pub fn poll(&mut self) -> Option<Exit> {
        let process = self.process.as_ref()?;
        if !process.has_exited() {
            return None;
        }

        let pid = process.pid();
        let code = process.exit_code().unwrap_or_default();
        self.process = None;

        let ran_for = self
            .last_start
            .take()
            .map_or(Duration::ZERO, |at| at.elapsed());

        Some(Exit { pid, code, ran_for })
    }

    /// Runs the restart policy for an exit and schedules the next start.
    ///
    /// Returns the wait, so the caller can log the decision instead of guessing it.
    pub fn schedule_restart(&mut self, ran_for: Duration) -> RestartDelay {
        let delay = self.planner.on_crash(ran_for);
        self.next_start = Some(Instant::now() + delay.delay);
        delay
    }
    /// Forgets the crash history; a manual restart is not a crash.
    pub fn reset_backoff(&mut self) {
        self.planner.reset();
    }

    /// Stops the child and returns the pid that was stopped.
    ///
    /// The handle is released as soon as the process is gone, so no exit is reported for a stop the
    /// supervisor asked for: a stop is not a crash.
    pub fn stop(&mut self) -> Option<u32> {
        self.next_start = None;
        let process = self.process.take()?;
        let pid = process.pid();

        process.terminate();
        let deadline = Instant::now() + STOP_BUDGET;
        while !process.has_exited() && Instant::now() < deadline {
            std::thread::sleep(STOP_POLL);
        }

        self.last_start = None;
        Some(pid)
    }
}

/// Resolves a bare executable name the way a console does.
///
/// A path stays as it is. A bare name (`cmd`, `neb-shell.exe`) is looked up in every `PATH`
/// directory, with `.exe` appended when the name has no extension, and returned as a full path;
/// when nothing matches the name is returned unchanged so the failure comes from Windows with the
/// name the user typed.
pub fn resolve_executable(name: &Path, path: Option<&std::ffi::OsStr>) -> PathBuf {
    let text = name.to_string_lossy();

    if name.is_absolute() || text.contains(['\\', '/']) {
        return name.to_path_buf();
    }

    let Some(path) = path else {
        return name.to_path_buf();
    };

    for directory in std::env::split_paths(path) {
        let direct = directory.join(name);
        if direct.is_file() {
            return direct;
        }

        if name.extension().is_none() {
            let with_extension = directory.join(format!("{text}.exe"));
            if with_extension.is_file() {
                return with_extension;
            }
        }
    }

    name.to_path_buf()
}

/// The command line `CreateProcessW` parses back into the arguments that were passed in.///
/// The quoting rules are the ones `CommandLineToArgvW` implements: a space or a tab forces quotes,
/// and a run of backslashes only doubles in front of a quote that is part of the value.
pub fn command_line(exe: &Path, args: &[String]) -> String {
    let mut line = quote(&exe.to_string_lossy());
    for arg in args {
        line.push(' ');
        line.push_str(&quote(arg));
    }
    line
}

/// One argument, quoted when it has to be.
fn quote(arg: &str) -> String {
    let needs_quotes = arg.is_empty() || arg.contains([' ', '\t', '"']);
    if !needs_quotes {
        return arg.to_owned();
    }

    let mut quoted = String::with_capacity(arg.len() + 2);
    quoted.push('"');

    let mut backslashes = 0;
    for character in arg.chars() {
        match character {
            '\\' => {
                backslashes += 1;
                quoted.push('\\');
            }
            '"' => {
                quoted.extend(std::iter::repeat_n('\\', backslashes));
                quoted.push('\\');
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                backslashes = 0;
                quoted.push(other);
            }
        }
    }

    quoted.extend(std::iter::repeat_n('\\', backslashes));
    quoted.push('"');
    quoted
}

/// A NUL-terminated UTF-16 copy of `text`.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(name: &str) -> ChildSpec {
        ChildSpec::new(name, PathBuf::from(r"C:\Windows\System32\cmd.exe"), true)
    }

    #[test]
    fn the_first_crash_waits_the_throttle_and_the_next_one_backs_off() {
        let mut planner = RestartPlanner::default();

        assert_eq!(
            planner.on_crash(Duration::from_secs(3)).delay,
            RESTART_THROTTLE
        );
        assert_eq!(
            planner.on_crash(Duration::from_secs(3)).delay,
            Duration::from_secs(60)
        );
        assert_eq!(
            planner.on_crash(Duration::from_secs(3)).delay,
            Duration::from_secs(300)
        );
        assert_eq!(
            planner.on_crash(Duration::from_secs(1)).delay,
            Duration::from_secs(300),
            "the ladder stops at the last step instead of growing without bound"
        );
    }

    #[test]
    fn a_stable_run_forgets_the_crash_ladder() {
        let mut planner = RestartPlanner::default();
        planner.on_crash(Duration::from_secs(1));
        planner.on_crash(Duration::from_secs(1));
        assert_eq!(planner.short_crashes(), 2);

        let delay = planner.on_crash(STABLE_RUN);

        assert_eq!(delay.step, RESTART_THROTTLE, "a healthy run starts over");
        assert_eq!(delay.delay, RESTART_THROTTLE);
    }

    #[test]
    fn a_child_is_never_started_twice_within_the_throttle() {
        let mut planner = RestartPlanner::default();

        // A crash straight after the start leaves the whole throttle to wait.
        assert_eq!(
            planner.on_crash(Duration::ZERO).delay,
            RESTART_THROTTLE,
            "a start at t=0 is not followed by one at t=1"
        );

        // A long first run still waits a full step, because the step is the floor.
        let mut slow = RestartPlanner::default();
        assert_eq!(
            slow.on_crash(Duration::from_secs(20)).delay,
            RESTART_THROTTLE,
            "20 s of uptime still owes 10 s of throttle, and 30 s of backoff wins"
        );
    }

    #[test]
    fn resetting_the_planner_makes_the_next_crash_a_first_one() {
        let mut planner = RestartPlanner::default();
        planner.on_crash(Duration::from_secs(1));
        planner.on_crash(Duration::from_secs(1));

        planner.reset();

        assert_eq!(planner.short_crashes(), 0);
        assert_eq!(
            planner.on_crash(Duration::from_secs(1)).step,
            RESTART_THROTTLE
        );
    }

    #[test]
    fn the_csharp_bar_enabled_key_turns_the_shell_on() {
        let csharp = serde_json::json!({
            "shell": { "barEnabled": true, "dockEnabled": true },
            "wallpapers": { "enabled": true }
        });
        assert_eq!(
            Enabled::from_json(&csharp),
            Enabled {
                shell: true,
                wallpaper: true,
                discord: false,
                wm: false,
                zapret: false,
                windhawk: false
            }
        );
    }

    #[test]
    fn the_shipped_defaults_are_a_shell_and_no_wallpaper() {
        assert_eq!(
            Enabled::default(),
            Enabled {
                shell: true,
                wallpaper: false,
                discord: false,
                wm: false,
                zapret: false,
                windhawk: false
            }
        );
    }

    #[test]
    fn the_switches_are_read_from_the_raw_json_and_missing_keys_keep_the_defaults() {
        let empty = serde_json::json!({});
        assert_eq!(Enabled::from_json(&empty), Enabled::default());

        let explicit = serde_json::json!({
            "shell": { "enabled": false },
            "wallpapers": { "enabled": true }
        });
        assert_eq!(
            Enabled::from_json(&explicit),
            Enabled {
                shell: false,
                wallpaper: true,
                discord: false,
                wm: false,
                zapret: false,
                windhawk: false
            }
        );

        let partial = serde_json::json!({ "wallpapers": { "enabled": true } });
        assert_eq!(
            Enabled::from_json(&partial),
            Enabled {
                shell: true,
                wallpaper: true,
                discord: false,
                wm: false,
                zapret: false,
                windhawk: false
            },
            "a missing shell section leaves the shell on"
        );

        let wrong_type = serde_json::json!({ "shell": { "enabled": "yes" } });
        assert!(
            Enabled::from_json(&wrong_type).shell,
            "a bad value is ignored"
        );

        let discord = serde_json::json!({ "discord": { "enabled": true } });
        assert!(
            Enabled::from_json(&discord).discord,
            "discord.enabled starts the Rich Presence process"
        );
        assert!(
            !Enabled::from_json(&empty).discord,
            "the shipped config leaves Discord off"
        );

        let windhawk = serde_json::json!({ "windhawk": { "enabled": true } });
        assert!(
            Enabled::from_json(&windhawk).windhawk,
            "windhawk.enabled starts the integration process"
        );
        assert!(!Enabled::from_json(&empty).windhawk, "Windhawk is opt-in");
    }

    #[test]
    fn a_missing_config_file_is_the_default_configuration() {
        let missing = PathBuf::from(r"C:\ProjectNeb\nothing-here\config.json");

        assert_eq!(Enabled::from_file(&missing), Enabled::default());
    }

    #[test]
    fn the_command_line_quotes_only_what_needs_it() {
        let exe = PathBuf::from(r"C:\Program Files\Neb\neb-shell.exe");

        assert_eq!(
            command_line(&exe, &["--flag".to_owned()]),
            r#""C:\Program Files\Neb\neb-shell.exe" --flag"#
        );
        assert_eq!(
            command_line(
                &PathBuf::from("cmd.exe"),
                &["/c".to_owned(), "exit 3".to_owned()]
            ),
            r#"cmd.exe /c "exit 3""#
        );
    }

    #[test]
    fn a_trailing_backslash_survives_quoting() {
        assert_eq!(quote(r"C:\dir with space\"), r#""C:\dir with space\\""#);
        assert_eq!(quote(r#"say "hi""#), r#""say \"hi\"""#);
        assert_eq!(quote(""), r#""""#);
        assert_eq!(quote("plain"), "plain");
    }

    #[test]
    fn a_path_is_not_resolved_but_a_bare_name_takes_the_extension() {
        let directory = tempfile::tempdir().expect("temp dir");
        let tool = directory.path().join("neb-tool.exe");
        std::fs::write(&tool, b"").expect("write");
        let path = std::ffi::OsString::from(directory.path());

        assert_eq!(
            resolve_executable(Path::new("neb-tool"), Some(&path)),
            tool,
            "a bare name gets .exe from the PATH entry"
        );
        assert_eq!(
            resolve_executable(Path::new("neb-tool.exe"), Some(&path)),
            tool
        );
        assert_eq!(
            resolve_executable(Path::new("nothing-here"), Some(&path)),
            PathBuf::from("nothing-here"),
            "an unresolvable name is handed to Windows unchanged"
        );
        assert_eq!(
            resolve_executable(Path::new(r"C:\Windows\System32\cmd.exe"), Some(&path)),
            PathBuf::from(r"C:\Windows\System32\cmd.exe"),
            "a path is never rewritten"
        );
        assert_eq!(
            resolve_executable(Path::new(r"tools\neb-tool.exe"), Some(&path)),
            PathBuf::from(r"tools\neb-tool.exe")
        );
    }

    #[test]
    fn a_second_mutex_for_the_same_name_reports_contention() {
        let name = format!(r"Local\ProjectNeb.supervisor.test.{}", std::process::id());

        let (first, created) = NamedMutex::acquire(&name).expect("the first mutex is created");
        assert!(created);

        let (second, created_again) = NamedMutex::acquire(&name).expect("the second open works");
        assert!(
            !created_again,
            "the second call must see the existing object"
        );

        drop(first);
        drop(second);
    }

    #[test]
    fn a_child_that_is_blocked_by_its_mutex_refuses_to_start() {
        let spec = spec("blocked");
        let child = Child::new(spec.clone());
        assert!(child.blocked().is_none());

        let mut rival = Child::new(spec);
        assert!(
            rival.blocked().is_some(),
            "the second child must be blocked"
        );
        assert!(matches!(rival.start(), Err(ChildError::Owned(_))));

        assert!(child.pid().is_none(), "nothing was started");
    }

    #[test]
    fn a_child_reports_its_exit_once() {
        let mut child = Child::new(ChildSpec {
            args: vec!["/c".to_owned(), "exit 3".to_owned()],
            ..spec("exit-three")
        });

        let pid = child.start().expect("cmd starts");
        assert_eq!(child.pid(), Some(pid));

        let deadline = Instant::now() + Duration::from_secs(10);
        let exit = loop {
            if let Some(exit) = child.poll() {
                break exit;
            }
            assert!(Instant::now() < deadline, "cmd /c exit 3 must finish");
            std::thread::sleep(Duration::from_millis(20));
        };

        assert_eq!(exit.code, 3);
        assert_eq!(exit.pid, pid);
        assert!(child.poll().is_none(), "an exit is reported exactly once");
        assert!(child.pid().is_none());
    }

    /// A private copy of `cmd.exe`: adoption matches full image paths, so a shared executable would
    /// make these tests depend on every other `cmd.exe` the session happens to run.
    fn stub_executable(directory: &Path) -> PathBuf {
        let source = std::env::var_os("COMSPEC")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"));
        let target = directory.join(format!("neb-adopt-stub-{}.exe", std::process::id()));
        std::fs::copy(source, &target).expect("the stub is a copy of cmd.exe");
        target
    }

    /// A stub that stays alive long enough to be adopted, killed and waited for.
    fn stub_process(exe: &Path) -> std::process::Child {
        std::process::Command::new(exe)
            .args(["/c", "timeout", "30"])
            .spawn()
            .expect("the stub starts")
    }

    fn poll_until_exit(child: &mut Child) -> Exit {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(exit) = child.poll() {
                return exit;
            }
            assert!(Instant::now() < deadline, "the stub must finish");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_process_is_found_by_its_full_image_path_whatever_the_case() {
        let directory = tempfile::tempdir().expect("temp dir");
        let exe = stub_executable(directory.path());
        let mut stub = stub_process(&exe);
        let upper = PathBuf::from(exe.to_string_lossy().to_uppercase());

        assert_eq!(
            Process::running_instance(&upper),
            Some(stub.id()),
            "a path match must be case-insensitive"
        );

        let _ = stub.kill();
        let _ = stub.wait();
    }

    #[test]
    fn a_running_child_left_by_a_crashed_supervisor_is_adopted_and_restarted_after_the_throttle() {
        let directory = tempfile::tempdir().expect("temp dir");
        let exe = stub_executable(directory.path());
        let mut stub = stub_process(&exe);

        let child_spec = ChildSpec {
            adopt: true,
            ..ChildSpec::new(&format!("adopted-{}", std::process::id()), exe, true)
        };
        let mut child = Child::new(child_spec);

        assert!(child.adopted(), "the running copy is taken over");
        assert!(child.running(), "the adopted pid is alive");
        assert_eq!(child.pid(), Some(stub.id()), "the real pid is published");
        assert!(
            child.blocked().is_none(),
            "adoption does not read as a mutex problem"
        );

        let _ = stub.kill();
        let _ = stub.wait();

        let exit = poll_until_exit(&mut child);
        assert_eq!(exit.pid, stub.id(), "the adopted process is the one reaped");
        assert!(child.pid().is_none());

        let delay = child.schedule_restart(exit.ran_for);
        assert_eq!(
            delay.delay, RESTART_THROTTLE,
            "an adopted child that dies immediately waits out the throttle"
        );
    }

    #[test]
    fn an_adopted_child_is_terminated_by_the_same_stop_as_a_spawned_one() {
        let directory = tempfile::tempdir().expect("temp dir");
        let exe = stub_executable(directory.path());
        let mut stub = stub_process(&exe);

        let child_spec = ChildSpec {
            adopt: true,
            ..ChildSpec::new(&format!("adopted-stop-{}", std::process::id()), exe, true)
        };
        let mut child = Child::new(child_spec);

        let stopped = child.stop().expect("the adopted process is stopped");
        assert_eq!(stopped, stub.id());
        assert!(!child.running(), "a stopped adopted child reads as stopped");
        assert!(stub.wait().is_ok(), "the real process ended");
    }

    #[test]
    fn a_diagnostic_command_never_adopts_a_shared_executable() {
        let directory = tempfile::tempdir().expect("temp dir");
        let exe = stub_executable(directory.path());
        let mut stub = stub_process(&exe);

        let child_spec = ChildSpec {
            args: vec!["/c".to_owned(), "timeout".to_owned(), "30".to_owned()],
            ..ChildSpec::new(&format!("not-adopted-{}", std::process::id()), exe, true)
        };
        let child = Child::new(child_spec);

        assert!(
            !child.adopted(),
            "--spawn-test must not take over a process the user started"
        );
        assert!(child.pid().is_none());

        let _ = stub.kill();
        let _ = stub.wait();
    }
}

#[test]
fn a_bom_in_the_config_does_not_hide_the_switches() {
    let dir = std::env::temp_dir().join(format!("neb-bom-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("config.json");
    std::fs::write(&path, "\u{feff}{\"wallpapers\":{\"enabled\":true}}").unwrap();
    assert!(
        Enabled::from_file(&path).wallpaper,
        "a UTF-8 BOM written by the C# build must not be read as a broken config"
    );
    std::fs::remove_dir_all(&dir).ok();
}
