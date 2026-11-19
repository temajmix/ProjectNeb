//! The copy half: build a plan from the staging tree, then apply it with bounded retries.
//!
//! A plan is a pure list of `(source, destination)` pairs. It is computed before a single byte is
//! written, so a staging directory that cannot be read fails the update before it touches the
//! installation — and so the plan can be asserted in a test without a filesystem race.
//!
//! Nothing here deletes. A file in the installation directory that the staging tree does not contain
//! stays where it is: the updater cannot tell a stale binary from a user's data file, and deleting
//! the wrong one is not something an update is allowed to do.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use windows::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};

use crate::log::Log;

/// How many times one file is attempted while it is locked by a dying process.
pub const COPY_ATTEMPTS: u32 = 10;

/// How long to wait between those attempts.
pub const RETRY_DELAY: Duration = Duration::from_millis(300);

/// One file to copy.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct CopyStep {
    /// File in the staging directory.
    pub source: PathBuf,
    /// Where it belongs in the installation directory.
    pub destination: PathBuf,
}

/// How patient a copy is with a locked file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Total attempts per file, at least one.
    pub attempts: u32,
    /// Pause before the next attempt.
    pub delay: Duration,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            attempts: COPY_ATTEMPTS,
            delay: RETRY_DELAY,
        }
    }
}

/// What a finished copy did, for the closing log line.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Summary {
    /// Files written.
    pub files: usize,
    /// Bytes written, as the filesystem counted them.
    pub bytes: u64,
    /// Retries caused by a locked file.
    pub retries: u32,
}

/// Why a copy stopped.
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// A file could not be copied, even after the retries.
    #[error(
        "cannot copy '{}' to '{}' after {attempts} attempt(s): {source}",
        source_path.display(),
        dest_path.display()
    )]
    Copy {
        /// The staged file.
        source_path: PathBuf,
        /// The installation file.
        dest_path: PathBuf,
        /// Attempts made before giving up.
        attempts: u32,
        /// The last operating-system error.
        #[source]
        source: io::Error,
    },
}

/// Lists every file in `staging`, paired with its destination under `app`.
///
/// Subdirectories are included through the files they hold; the directories themselves are created by
/// [`apply`] when it needs them. Symbolic links are skipped: an archive verified by the supervisor is
/// plain files, and following a link out of the staging tree is exactly the kind of surprise an
/// updater must not have.
///
/// # Errors
///
/// The [`io::Error`] from reading the staging tree, including [`io::ErrorKind::NotFound`] when it is
/// missing and [`io::ErrorKind::InvalidInput`] when it is not a directory.
pub fn plan(staging: &Path, app: &Path) -> io::Result<Vec<CopyStep>> {
    let metadata = fs::symlink_metadata(staging)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("staging '{}' is not a directory", staging.display()),
        ));
    }

    let mut steps = Vec::new();
    collect(staging, staging, app, &mut steps)?;
    steps.sort();

    Ok(steps)
}

/// Walks one directory of the staging tree, appending a step for every plain file.
fn collect(root: &Path, directory: &Path, app: &Path, steps: &mut Vec<CopyStep>) -> io::Result<()> {
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let source = entry.path();
        let relative = source.strip_prefix(root).unwrap_or(&source);
        let metadata = fs::symlink_metadata(&source)?;

        if metadata.is_dir() {
            collect(root, &source, app, steps)?;
        } else if metadata.is_file() {
            steps.push(CopyStep {
                destination: app.join(relative),
                source,
            });
        }
    }

    Ok(())
}

/// Copies every step into the installation directory, making subdirectories as needed.
///
/// A sharing violation or an access denied is retried [`RetryPolicy::attempts`] times with
/// [`RetryPolicy::delay`] between attempts; anything else, and a lock that outlives the last attempt,
/// is [`ApplyError::Copy`].
///
/// # Errors
///
/// [`ApplyError::Copy`] when a file could not be written.
pub fn apply(steps: &[CopyStep], policy: RetryPolicy, log: &Log) -> Result<Summary, ApplyError> {
    let attempts = policy.attempts.max(1);
    let mut summary = Summary::default();

    for step in steps {
        let mut attempt = 1;
        loop {
            match copy_one(step) {
                Ok(bytes) => {
                    summary.files += 1;
                    summary.bytes += bytes;
                    log.info(
                        "copy",
                        &format!(
                            "{} <- {} ({bytes} bytes)",
                            step.destination.display(),
                            step.source.display()
                        ),
                    );
                    break;
                }
                Err(source) if is_transient(&source) && attempt < attempts => {
                    summary.retries += 1;
                    log.warn(
                        "copy",
                        &format!(
                            "{} is locked ({}), retry {attempt}/{attempts}",
                            step.destination.display(),
                            source
                        ),
                    );
                    thread::sleep(policy.delay);
                    attempt += 1;
                }
                Err(source) => {
                    return Err(ApplyError::Copy {
                        source_path: step.source.clone(),
                        dest_path: step.destination.clone(),
                        attempts: attempt,
                        source,
                    });
                }
            }
        }
    }

    Ok(summary)
}

/// Copies one file, creating its parent directory first.
fn copy_one(step: &CopyStep) -> io::Result<u64> {
    if let Some(parent) = step
        .destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }

    fs::copy(&step.source, &step.destination)
}

/// Whether an error means "another process still holds this file".
///
/// Only these two codes are worth waiting for. A missing path or a full disk is reported at once,
/// because retrying it changes nothing and keeps the user waiting for ten times the delay.
pub fn is_transient(error: &io::Error) -> bool {
    let Some(code) = error.raw_os_error() else {
        return false;
    };

    u32::try_from(code)
        .is_ok_and(|code| code == ERROR_SHARING_VIOLATION.0 || code == ERROR_ACCESS_DENIED.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plan_pairs_every_staged_file_with_its_place_in_the_app() {
        let root = tempfile::tempdir().expect("temp dir");
        let staging = root.path().join("staging");
        let app = root.path().join("app");
        fs::create_dir_all(staging.join("sub")).expect("staging tree");
        fs::write(staging.join("neb-shell.exe"), b"shell").expect("write");
        fs::write(staging.join("sub").join("neb-wallpaper.exe"), b"wall").expect("write");

        let steps = plan(&staging, &app).expect("plan");

        assert_eq!(
            steps,
            vec![
                CopyStep {
                    source: staging.join("neb-shell.exe"),
                    destination: app.join("neb-shell.exe"),
                },
                CopyStep {
                    source: staging.join("sub").join("neb-wallpaper.exe"),
                    destination: app.join("sub").join("neb-wallpaper.exe"),
                },
            ]
        );
    }

    #[test]
    fn an_empty_staging_directory_is_an_empty_plan() {
        let root = tempfile::tempdir().expect("temp dir");
        let staging = root.path().join("staging");
        fs::create_dir_all(&staging).expect("staging tree");

        assert_eq!(plan(&staging, &root.path().join("app")).expect("plan"), []);
    }

    #[test]
    fn a_missing_or_non_directory_staging_path_is_refused() {
        let root = tempfile::tempdir().expect("temp dir");
        let app = root.path().join("app");

        let missing = plan(&root.path().join("absent"), &app).expect_err("missing");
        assert_eq!(missing.kind(), io::ErrorKind::NotFound);

        let file = root.path().join("archive.zip");
        fs::write(&file, b"pk").expect("write");
        let not_a_directory = plan(&file, &app).expect_err("a file is not a tree");
        assert_eq!(not_a_directory.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn the_plan_orders_files_by_path_so_two_runs_copy_the_same_order() {
        let root = tempfile::tempdir().expect("temp dir");
        let staging = root.path().join("staging");
        fs::create_dir_all(staging.join("z")).expect("staging tree");
        for name in ["b.txt", "a.txt", "z/c.txt"] {
            fs::write(staging.join(name), name.as_bytes()).expect("write");
        }

        let steps = plan(&staging, &root.path().join("app")).expect("plan");
        let names: Vec<String> = steps
            .iter()
            .map(|step| {
                step.source
                    .strip_prefix(&staging)
                    .expect("a staged path")
                    .display()
                    .to_string()
            })
            .collect();

        assert_eq!(names, ["a.txt", "b.txt", "z\\c.txt"], "sorted for the log");
    }

    #[test]
    fn a_sharing_violation_is_retried_and_a_missing_file_is_not() {
        assert!(is_transient(&io::Error::from_raw_os_error(
            ERROR_SHARING_VIOLATION.0 as i32
        )));
        assert!(is_transient(&io::Error::from_raw_os_error(
            ERROR_ACCESS_DENIED.0 as i32
        )));
        assert!(!is_transient(&io::Error::from(io::ErrorKind::NotFound)));
        assert!(!is_transient(&io::Error::other("the disk is full")));
    }

    #[test]
    fn a_retry_policy_defaults_to_the_documented_budget() {
        let policy = RetryPolicy::default();

        assert_eq!(policy.attempts, COPY_ATTEMPTS);
        assert_eq!(policy.delay, RETRY_DELAY);
        assert_eq!(policy.attempts, 10);
        assert_eq!(policy.delay, Duration::from_millis(300));
    }
}
