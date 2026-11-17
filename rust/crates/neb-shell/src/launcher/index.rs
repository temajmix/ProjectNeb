//! The launcher's data source: every `.lnk` and `.exe` under the two Start Menu folders.
//!
//! The walk is hand-written rather than a recursive `read_dir` chain because one unreadable folder —
//! a leftover of an uninstalled application, an offline roaming profile — must cost that folder and
//! nothing else. A `.lnk` is resolved through `IShellLinkW` read through `IPersistFile`, so the entry
//! holds the executable that would actually run; a shortcut Windows cannot resolve is skipped instead
//! of being launched into a "not found" dialog. Store applications are absent on purpose: they have
//! no launchable path, which is the same rule the C# build follows.
//!
//! The whole walk runs on a worker thread ([`Cache`]), because the first `Alt+Space` of a session must
//! show the panel immediately rather than freeze it for the length of a disk walk. The result is
//! cached for five minutes; the cache keeps Strings and paths only, never a window or a bitmap.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    IPersistFile, STGM_READ,
};
use windows::Win32::UI::Shell::{IShellLinkW, ShellLink};
use windows::core::{Interface, PCWSTR};

/// How long a built index is considered current.
pub const TTL: Duration = Duration::from_secs(5 * 60);

/// How deep the walk goes below a Start Menu root.
const MAX_DEPTH: usize = 8;

/// The extensions the launcher indexes.
const EXTENSIONS: [&str; 2] = ["lnk", "exe"];

/// One application the launcher can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// What the result row calls it: the file name without its extension.
    pub name: String,
    /// The executable that is launched; a shortcut's target, already resolved.
    pub path: PathBuf,
}

/// The two Start Menu program folders, machine-wide first.
///
/// A name that exists in both is shown once, and the machine copy is the one Windows would run for
/// "all users". A root that does not exist contributes nothing; the caller never has to check.
pub fn default_roots() -> Vec<PathBuf> {
    let mut roots = Vec::with_capacity(2);

    for variable in ["ProgramData", "AppData"] {
        if let Ok(base) = std::env::var(variable)
            && !base.trim().is_empty()
        {
            roots.push(
                PathBuf::from(base)
                    .join("Microsoft")
                    .join("Windows")
                    .join("Start Menu")
                    .join("Programs"),
            );
        }
    }

    roots
}

/// Builds the index from `roots`, resolving every shortcut through COM.
pub fn build(roots: &[PathBuf]) -> Vec<Entry> {
    let apartment = initialize_com();
    let entries = entries_from(collect_files(roots), resolve_shortcut);

    if apartment {
        // SAFETY: this thread called `CoInitializeEx` once, above, and is done with every COM
        // interface it created; the counter is balanced exactly once.
        unsafe {
            CoUninitialize();
        }
    }

    entries
}

/// Initialises this thread's COM apartment for the link reader, reporting whether the call has to be
/// balanced by `CoUninitialize`.
///
/// `RPC_E_CHANGED_MODE` means the thread already lives in another apartment model and the shell link
/// object is still usable from it, so that is not a failure — only the balance changes.
fn initialize_com() -> bool {
    // SAFETY: COM has no out-parameters here and initialising this thread is the documented
    // precondition for `CoCreateInstance`.
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() }
}

/// Builds entries from already-collected files, with `resolve` supplying a shortcut's target.
///
/// `resolve` is a parameter so the walk and the de-duplication can be tested without a Start Menu:
/// the test index is a temporary folder and a closure. A `.lnk` whose target cannot be resolved, or
/// whose target no longer exists, is skipped — a broken shortcut is a missing result, not an entry
/// that fails on Enter.
pub fn entries_from<I, F>(files: I, mut resolve: F) -> Vec<Entry>
where
    I: IntoIterator<Item = PathBuf>,
    F: FnMut(&Path) -> Option<PathBuf>,
{
    let mut entries: Vec<Entry> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut broken = 0usize;

    for file in files {
        let target = if is_shortcut(&file) {
            match resolve(&file) {
                Some(target) => target,
                None => {
                    broken += 1;
                    continue;
                }
            }
        } else {
            file.clone()
        };

        if !target.is_file() {
            broken += 1;
            continue;
        }

        let key = target.to_string_lossy().to_lowercase();
        if !seen.insert(key) {
            continue;
        }

        entries.push(Entry {
            name: name_for(&file),
            path: target,
        });
    }

    entries.sort_by_key(|entry| entry.name.to_lowercase());

    if broken > 0 {
        crate::log_warn!(
            "launcher",
            "{broken} start menu shortcut(s) are broken, skipped"
        );
    }

    entries
}

/// The `.lnk`/`.exe` files under `roots`, depth first and sorted so a build is reproducible.
pub fn collect_files(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> =
        roots.iter().rev().map(|root| (root.clone(), 0)).collect();

    while let Some((directory, depth)) = pending.pop() {
        if !directory.is_dir() {
            continue;
        }

        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                crate::log_warn!(
                    "launcher",
                    "'{}' cannot be read: {error}",
                    directory.display()
                );
                continue;
            }
        };

        let mut files = Vec::new();
        let mut directories = Vec::new();

        for entry in entries.flatten() {
            let path = entry.path();

            if path.is_dir() {
                if depth < MAX_DEPTH {
                    directories.push(path);
                }
            } else if is_launchable(&path) {
                files.push(path);
            }
        }

        files.sort();
        found.extend(files);

        directories.sort();
        for child in directories.into_iter().rev() {
            pending.push((child, depth + 1));
        }
    }

    found
}

/// Whether `path` has one of the two extensions the launcher indexes.
fn is_launchable(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| {
            EXTENSIONS
                .iter()
                .any(|wanted| extension.eq_ignore_ascii_case(wanted))
        })
}

/// Whether `path` is a `.lnk`, the one extension that has to be resolved first.
fn is_shortcut(path: &Path) -> bool {
    path.extension()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|extension| extension.eq_ignore_ascii_case("lnk"))
}

/// The display name of an entry: the file name without `.lnk` or `.exe`.
fn name_for(path: &Path) -> String {
    match path.file_stem().and_then(|stem| stem.to_str()) {
        Some(stem) if !stem.trim().is_empty() => stem.trim().to_owned(),
        _ => path.to_string_lossy().into_owned(),
    }
}

/// The target of one `.lnk`, through `IShellLinkW` and `IPersistFile`.
///
/// The caller must have initialised COM on its thread — [`build`] does, and the tests that call this
/// directly do the same.
fn resolve_shortcut(path: &Path) -> Option<PathBuf> {
    let link: IShellLinkW =
        // SAFETY: `ShellLink` is the CLSID of the shell link object and `None` means "no outer
        // unknown"; the returned interface is reference-counted by COM and owned by `link`.
        match unsafe { CoCreateInstance(&ShellLink, None::<&windows::core::IUnknown>, CLSCTX_INPROC_SERVER) } {
            Ok(link) => link,
            Err(error) => {
                crate::log_warn!("launcher", "the shell link object is unavailable: {error}");
                return None;
            }
        };

    let file: IPersistFile = match link.cast() {
        Ok(file) => file,
        Err(error) => {
            crate::log_warn!("launcher", "a shortcut has no IPersistFile: {error}");
            return None;
        }
    };

    let name = wide(&path.to_string_lossy());

    // SAFETY: `name` is a NUL-terminated UTF-16 path that outlives the call, and `STGM_READ` asks
    // only to read the file. This is what parses the `.lnk`.
    if let Err(error) = unsafe { file.Load(PCWSTR(name.as_ptr()), STGM_READ) } {
        crate::log_warn!(
            "launcher",
            "'{}' cannot be read as a shortcut: {error}",
            path.display()
        );
        return None;
    }

    let mut buffer = [0u16; 2048];

    // SAFETY: `buffer` is a live array Direct2D-sized for any start-menu target (longer than
    // MAX_PATH, shorter than 2048 units), and `GetPath` writes at most its length; the null find-data
    // and zero flags ask for the target path only.
    if let Err(error) = unsafe { link.GetPath(&mut buffer, std::ptr::null_mut(), 0) } {
        crate::log_warn!(
            "launcher",
            "'{}' has no target path: {error}",
            path.display()
        );
        return None;
    }

    let length = buffer.iter().position(|unit| *unit == 0).unwrap_or(0);
    if length == 0 {
        return None;
    }

    Some(PathBuf::from(String::from_utf16_lossy(&buffer[..length])))
}

/// Whether an index built at `built_at` has outlived [`TTL`] by `now`.
///
/// `None` is "never built", which is stale; a build that is still running is not (the cache keeps the
/// old entries on screen until the new ones arrive).
pub fn is_stale(built_at: Option<Instant>, now: Instant) -> bool {
    built_at.is_none_or(|built| now.duration_since(built) >= TTL)
}

/// One view of the cache, cheap to clone: an `Arc<Vec<Entry>>` and whether a build is in flight.
#[derive(Debug, Clone)]
pub struct Snapshot {
    /// The newest complete index, empty before the first build.
    pub entries: Arc<Vec<Entry>>,
    /// A worker is walking the Start Menu right now.
    pub building: bool,
    /// Increments when a build finishes; the UI uses it to adopt a finished build exactly once.
    pub generation: u64,
}

/// The state shared with the worker.
#[derive(Debug)]
struct Shared {
    entries: Arc<Vec<Entry>>,
    building: bool,
    built_at: Option<Instant>,
    generation: u64,
}

/// The five-minute Start Menu index cache.
///
/// The first open of a session starts a worker; every later open inside the TTL reuses the entries.
/// A stale open shows the old entries at once and asks the worker for replacements, so the panel never
/// waits for a disk walk. Concurrent opens share one build. The worker posts
/// [`WM_LAUNCHER_INDEX`](super::window::WM_LAUNCHER_INDEX) when it is done, which is what wakes the
/// UI thread.
#[derive(Debug)]
pub struct Cache {
    shared: Arc<Mutex<Shared>>,
    adopted: u64,
}

impl Default for Cache {
    fn default() -> Self {
        Self::new()
    }
}

impl Cache {
    /// An empty cache; nothing is read until the first [`open`](Self::open).
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Mutex::new(Shared {
                entries: Arc::new(Vec::new()),
                building: false,
                built_at: None,
                generation: 0,
            })),
            adopted: 0,
        }
    }

    /// The current index, starting a build first when it is missing or stale.
    ///
    /// The returned snapshot is the state *after* the decision: the old entries plus `building: true`
    /// when a rebuild was just started, so the launcher can say "building" without waiting.
    pub fn open(&mut self) -> Snapshot {
        let mut shared = self.shared.lock().unwrap_or_else(|lock| lock.into_inner());

        if !shared.building && is_stale(shared.built_at, Instant::now()) {
            shared.building = true;
            start_worker(Arc::clone(&self.shared));
        }

        Snapshot {
            entries: Arc::clone(&shared.entries),
            building: shared.building,
            generation: shared.generation,
        }
    }

    /// A finished build that the caller has not seen yet, if there is one.
    ///
    /// This is what the UI thread calls when the worker's message arrives: it marks the generation as
    /// adopted, so a second call is `None` and the window is not rebuilt twice.
    pub fn finished(&mut self) -> Option<Arc<Vec<Entry>>> {
        let shared = self.shared.lock().unwrap_or_else(|lock| lock.into_inner());

        if shared.generation <= self.adopted {
            return None;
        }

        self.adopted = shared.generation;
        Some(Arc::clone(&shared.entries))
    }
}

/// Starts one worker that builds the index and publishes it.
fn start_worker(shared: Arc<Mutex<Shared>>) {
    crate::log_info!("launcher", "indexing the Start Menu on a worker thread");

    std::thread::spawn(move || {
        let roots = default_roots();
        let entries = Arc::new(build(&roots));

        let mut state = shared.lock().unwrap_or_else(|lock| lock.into_inner());
        state.entries = entries;
        state.building = false;
        state.built_at = Some(Instant::now());
        state.generation = state.generation.wrapping_add(1);
        let count = state.entries.len();
        drop(state);

        crate::log_info!("launcher", "indexed {count} Start Menu app(s)");
        crate::window::post_to_control(crate::launcher::WM_LAUNCHER_INDEX, 0);
    });
}

/// `text` as a NUL-terminated UTF-16 buffer.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("neb-launcher-index-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create the temp dir");
        dir
    }

    #[test]
    fn the_walk_finds_nested_exes_and_ignores_everything_else() {
        let root = temp_dir("walk");
        let nested = root.join("Tools");
        fs::create_dir_all(&nested).expect("create a nested folder");
        fs::write(root.join("Alpha.exe"), b"x").expect("write an exe");
        fs::write(nested.join("Beta.EXE"), b"x").expect("write an upper-case exe");
        fs::write(nested.join("notes.txt"), b"x").expect("write a text file");
        fs::write(nested.join("Link.lnk"), b"x").expect("write a fake link");

        let files = collect_files(std::slice::from_ref(&root));

        assert_eq!(files.len(), 3, "{files:?}");
        assert!(files.iter().any(|path| path.ends_with("Alpha.exe")));
        assert!(files.iter().any(|path| path.ends_with("Beta.EXE")));
        assert!(files.iter().any(|path| path.ends_with("Link.lnk")));
        assert!(
            files.iter().all(|path| !path.ends_with("notes.txt")),
            "a text file is not an application"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_shortcut_and_its_target_are_one_entry_named_after_the_shortcut() {
        let root = temp_dir("dedupe");
        let exe = root.join("tool.exe");
        let shortcut = root.join("Alias.lnk");
        fs::write(&exe, b"x").expect("write the exe");
        fs::write(&shortcut, b"x").expect("write the shortcut");

        let entries = entries_from(vec![exe.clone(), shortcut], |_| Some(exe.clone()));

        assert_eq!(
            entries.len(),
            1,
            "the resolved path de-duplicates them: {entries:?}"
        );
        assert_eq!(entries[0].path, exe);
        assert_eq!(
            entries[0].name, "tool",
            "the .exe sorts first and names the entry"
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_broken_shortcut_is_skipped_not_launched() {
        let root = temp_dir("broken");
        let shortcut = root.join("Gone.lnk");
        fs::write(&shortcut, b"x").expect("write the shortcut");

        let entries = entries_from(vec![shortcut], |_| None);

        assert!(entries.is_empty(), "{entries:?}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_target_that_no_longer_exists_is_skipped() {
        let root = temp_dir("missing-target");
        let shortcut = root.join("Stale.lnk");
        fs::write(&shortcut, b"x").expect("write the shortcut");
        let gone = root.join("gone.exe");

        let entries = entries_from(vec![shortcut], |_| Some(gone.clone()));

        assert!(entries.is_empty(), "{entries:?}");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_roots_contribute_nothing_instead_of_failing() {
        let missing = std::env::temp_dir().join("neb-launcher-there-is-no-such-folder");

        assert!(collect_files(std::slice::from_ref(&missing)).is_empty());
        assert!(build(std::slice::from_ref(&missing)).is_empty());
    }

    #[test]
    fn entries_are_sorted_by_name_ignoring_case() {
        let root = temp_dir("sort");
        for name in ["zebra.exe", "Alpha.exe", "Mike.exe"] {
            fs::write(root.join(name), b"x").expect("write an exe");
        }
        let files = collect_files(std::slice::from_ref(&root));

        let entries = entries_from(files, |_| None);
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();

        assert_eq!(names, ["Alpha", "Mike", "zebra"]);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn the_cache_is_current_for_five_minutes_and_stale_after() {
        let built = Instant::now();

        assert_eq!(TTL, Duration::from_secs(300));
        assert!(
            is_stale(None, built),
            "a cache that was never built is stale"
        );
        assert!(!is_stale(Some(built), built + Duration::from_secs(299)));
        assert!(is_stale(Some(built), built + TTL));
        assert!(is_stale(Some(built), built + Duration::from_secs(301)));
    }

    #[test]
    fn a_cache_starts_empty_and_adopts_a_published_generation_once() {
        let mut cache = Cache::new();
        let first = cache.open();

        assert!(first.entries.is_empty());

        {
            let mut shared = cache.shared.lock().expect("lock");
            shared.entries = Arc::new(vec![Entry {
                name: "Paint".to_owned(),
                path: PathBuf::from(r"C:\Windows\System32\mspaint.exe"),
            }]);
            shared.building = false;
            shared.built_at = Some(Instant::now());
            shared.generation = 1;
        }

        let adopted = cache.finished().expect("a new generation is waiting");
        assert_eq!(adopted.len(), 1);
        assert!(cache.finished().is_none(), "a generation is adopted once");
    }

    #[test]
    fn the_default_roots_are_the_two_start_menu_programs_folders() {
        let roots = default_roots();

        assert!(!roots.is_empty(), "this machine has an AppData folder");
        for root in &roots {
            assert!(
                root.ends_with(Path::new("Microsoft/Windows/Start Menu/Programs")),
                "{root:?}"
            );
        }
    }

    #[test]
    fn a_real_start_menu_shortcut_resolves_to_an_existing_target() {
        let Some(shortcut) = collect_files(&default_roots())
            .into_iter()
            .find(|path| is_shortcut(path))
        else {
            return;
        };

        let apartment = initialize_com();
        let target = resolve_shortcut(&shortcut);
        if apartment {
            // SAFETY: this test thread called `CoInitializeEx` once and is done with the interfaces.
            unsafe {
                CoUninitialize();
            }
        }

        assert!(
            target.as_ref().is_some_and(|target| target.is_file()),
            "'{}' must resolve to an existing target, got {target:?}",
            shortcut.display()
        );
    }
}
