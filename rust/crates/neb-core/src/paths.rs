use std::ffi::OsString;
use std::path::PathBuf;

/// `FOLDERID_LocalAppData`; the shell resolves it through the same table `AppPaths.Root` uses.
#[cfg(windows)]
const FOLDERID_LOCAL_APP_DATA: Guid = Guid {
    data1: 0xF1B3_2785,
    data2: 0x6FBA,
    data3: 0x4FCF,
    data4: [0x9D, 0x55, 0x7B, 0x8E, 0x7F, 0x15, 0x70, 0x91],
};

/// Directory ProjectNeb owns under `%LocalAppData%`.
pub const ROOT_NAME: &str = "ProjectNeb";

/// Environment variable that replaces the whole data root, i.e. the `ProjectNeb` folder itself.
///
/// It exists for the test suites and for portable runs, where a private directory is wanted without
/// touching the user's profile. It is *not* a LocalAppData substitute: pointing it at `D:\Data` makes
/// the config `D:\Data\config.json`, not `D:\Data\ProjectNeb\config.json`.
pub const HOME_VARIABLE: &str = "NEB_HOME";

/// Why the data root could not be resolved.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PathsError {
    /// No override, no shell answer and no environment variable: this process cannot know where its
    /// data belongs, and guessing would write in the wrong place.
    #[error(
        "no ProjectNeb data directory: NEB_HOME is unset, the shell has no local app data folder, and \
         %LOCALAPPDATA% is unset"
    )]
    NoDataRoot,
}

/// Resolves the data root the same way the C# build does, in this order:
///
/// 1. `NEB_HOME` — an explicit override, the whole `ProjectNeb` folder, used verbatim;
/// 2. `SHGetKnownFolderPath(FOLDERID_LocalAppData)\ProjectNeb` — what the shell says, which is what
///    `AppPaths.Root` reads and therefore what the C# app uses even when the variable is stale;
/// 3. `%LOCALAPPDATA%\ProjectNeb` — a fallback for environments without a shell;
/// 4. otherwise [`PathsError::NoDataRoot`], because a wrong directory is worse than a stopped process.
///
/// # Errors
///
/// [`PathsError::NoDataRoot`] when none of the three sources answers.
pub fn try_root() -> Result<PathBuf, PathsError> {
    resolve_root(
        std::env::var_os(HOME_VARIABLE),
        known_folder_local_app_data(),
        std::env::var_os("LOCALAPPDATA"),
    )
}

/// The data root, for callers that are already past the point of reporting a failure.
///
/// # Panics
///
/// Panics with the reason when [`try_root`] fails. That means Windows cannot name a local app data
/// folder at all — the process has nowhere to put its config or its log, so it has nothing to say and
/// no way to say it. Use [`try_root`] anywhere an error can be returned instead.
pub fn root() -> PathBuf {
    try_root().expect("ProjectNeb has no data directory")
}

/// The precedence from [`try_root`], with the three sources passed in so tests cover every branch.
fn resolve_root(
    home: Option<OsString>,
    known_folder: Option<PathBuf>,
    local_app_data: Option<OsString>,
) -> Result<PathBuf, PathsError> {
    if let Some(home) = home.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(home));
    }

    if let Some(base) = known_folder {
        return Ok(base.join(ROOT_NAME));
    }

    match local_app_data.filter(|value| !value.is_empty()) {
        Some(base) => Ok(PathBuf::from(base).join(ROOT_NAME)),
        None => Err(PathsError::NoDataRoot),
    }
}

/// The single settings file: `config.json` in [`root`].
pub fn config_file() -> PathBuf {
    root().join("config.json")
}

/// Directory for `neb-YYYYMMDD.log` files, see [`crate::logging`].
pub fn logs_dir() -> PathBuf {
    root().join("logs")
}

/// Directory holding shell profiles.
pub fn profiles_dir() -> PathBuf {
    root().join("profiles")
}

/// Directory holding theme token files.
pub fn themes_dir() -> PathBuf {
    root().join("themes")
}

/// A `GUID`, declared here so the crate needs no Win32 binding dependency for one call.
#[cfg(windows)]
#[repr(C)]
struct Guid {
    data1: u32,
    data2: u16,
    data3: u16,
    data4: [u8; 8],
}

#[cfg(windows)]
#[link(name = "shell32")]
unsafe extern "system" {
    fn SHGetKnownFolderPath(
        folder: *const Guid,
        flags: u32,
        token: *mut std::ffi::c_void,
        path: *mut *mut u16,
    ) -> i32;
}

#[cfg(windows)]
#[link(name = "ole32")]
unsafe extern "system" {
    fn CoTaskMemFree(pointer: *mut std::ffi::c_void);
}

/// `%LocalAppData%` through `SHGetKnownFolderPath(FOLDERID_LocalAppData)`, or `None` when the shell
/// refuses — in which case the caller falls back to the environment variable.
#[cfg(windows)]
fn known_folder_local_app_data() -> Option<PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::ptr;

    /// "Pick the default folder": the documented value for a plain lookup.
    const KF_FLAG_DEFAULT: u32 = 0;

    let mut wide: *mut u16 = ptr::null_mut();
    // SAFETY: the folder id is a valid, aligned GUID that outlives the call, `wide` is a valid
    // out-parameter for the returned `PWSTR`, and a null token asks for the current user.
    let status = unsafe {
        SHGetKnownFolderPath(
            &FOLDERID_LOCAL_APP_DATA,
            KF_FLAG_DEFAULT,
            ptr::null_mut(),
            &mut wide,
        )
    };
    if status < 0 || wide.is_null() {
        return None;
    }

    // SAFETY: on success the call hands back a NUL-terminated string allocated with
    // `CoTaskMemAlloc` that this process owns; nothing else aliases it while it is copied out.
    let mut length = 0;
    while unsafe { *wide.add(length) } != 0 {
        length += 1;
    }
    // SAFETY: `wide` points at a live allocation of at least `length` initialised `u16`s.
    let text = unsafe { std::slice::from_raw_parts(wide, length) };
    let path = PathBuf::from(OsString::from_wide(text));

    // SAFETY: the pointer came from the call above and is released exactly once, here.
    unsafe { CoTaskMemFree(wide.cast()) };

    (!path.as_os_str().is_empty()).then_some(path)
}

/// Without the shell there are no known folders; the environment variable is the only source.
#[cfg(not(windows))]
fn known_folder_local_app_data() -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neb_home_is_used_verbatim_and_wins_over_every_other_source() {
        let root = resolve_root(
            Some(OsString::from(r"D:\Portable\Neb")),
            Some(PathBuf::from(r"C:\Users\tester\AppData\Local")),
            Some(OsString::from(r"C:\stale\AppData\Local")),
        )
        .expect("NEB_HOME always answers");

        assert_eq!(root, PathBuf::from(r"D:\Portable\Neb"));
    }

    #[test]
    fn the_known_folder_beats_a_stale_local_app_data() {
        let root = resolve_root(
            None,
            Some(PathBuf::from(r"C:\Users\tester\AppData\Local")),
            Some(OsString::from(r"C:\stale\AppData\Local")),
        )
        .expect("the known folder answers");

        assert_eq!(
            root,
            PathBuf::from(r"C:\Users\tester\AppData\Local").join("ProjectNeb")
        );
    }

    #[test]
    fn the_variable_is_used_only_when_the_shell_has_nothing() {
        let root = resolve_root(
            None,
            None,
            Some(OsString::from(r"C:\Users\tester\AppData\Local")),
        )
        .expect("the variable answers");

        assert_eq!(
            root,
            PathBuf::from(r"C:\Users\tester\AppData\Local").join("ProjectNeb")
        );
    }

    #[test]
    fn nothing_known_is_an_error_not_a_guess() {
        assert_eq!(resolve_root(None, None, None), Err(PathsError::NoDataRoot));
        assert_eq!(
            resolve_root(Some(OsString::new()), None, Some(OsString::new())),
            Err(PathsError::NoDataRoot),
            "empty values are not an answer"
        );
    }

    #[test]
    fn well_known_directories_hang_off_the_root() {
        let root = root();

        assert_eq!(config_file(), root.join("config.json"));
        assert_eq!(logs_dir(), root.join("logs"));
        assert_eq!(profiles_dir(), root.join("profiles"));
        assert_eq!(themes_dir(), root.join("themes"));
    }

    #[test]
    fn root_ends_with_the_owned_directory_name() {
        let root = try_root().expect("this machine has a data directory");

        assert_eq!(
            root.file_name().and_then(|name| name.to_str()),
            Some(ROOT_NAME)
        );
    }

    #[cfg(windows)]
    #[test]
    fn the_known_folder_api_answers_with_an_absolute_path() {
        let base =
            known_folder_local_app_data().expect("every Windows profile has a local app data");

        assert!(base.is_absolute(), "{}", base.display());
        assert!(
            base.ends_with("AppData\\Local") || base.to_string_lossy().contains("Local"),
            "the folder must be the local app data one: {}",
            base.display()
        );
    }
}
