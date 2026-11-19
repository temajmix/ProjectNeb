//! `neb-cli release`: one command from a new version number to a published release.
//!
//! The pipeline is deliberately made of other programs вЂ” `cargo`, `gh`, `git` and Inno Setup вЂ” rather
//! than HTTP clients, so authentication and transport stay in the tools the maintainer already uses.
//! Every effect goes through [`Runner`], which is why `--dry-run` can print the exact same plan while
//! touching nothing.

mod manifest;
mod quote;
mod semver;
mod stage;

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::commands::CliError;
use semver::Version;

/// Where `latest.json` and `CHANGELOG.md` are published from when `--manifest-repo` is not given.
pub const DEFAULT_MANIFEST_REPO: &str = r"A:\ProjectNeb-wt\site";

/// The GitHub repository that receives the release.
const RELEASE_REPO: &str = "temajmix/ProjectNeb";

/// What `neb-cli release` was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// New version, `MAJOR.MINOR.PATCH`.
    pub version: String,
    /// Short release description, used in the GitHub notes, the manifest and the changelog.
    pub notes: String,
    /// Release channel; `beta` unless overridden.
    pub channel: String,
    /// Working copy whose `latest.json` and `CHANGELOG.md` are updated and pushed.
    pub manifest_repo: PathBuf,
    /// Print the plan without running anything.
    pub dry_run: bool,
    /// Skip `cargo test --workspace`.
    pub skip_tests: bool,
    /// Skip Inno Setup and publish the portable archive alone.
    pub skip_installer: bool,
}

impl Options {
    /// Parses the arguments that follow `release`.
    ///
    /// # Errors
    ///
    /// [`CliError::Usage`] for a missing required option, a missing value, an unknown option or a bare
    /// operand.
    pub fn parse(args: &[String]) -> Result<Self, CliError> {
        let mut version = None;
        let mut notes = None;
        let mut channel = "beta".to_owned();
        let mut manifest_repo = PathBuf::from(DEFAULT_MANIFEST_REPO);
        let mut dry_run = false;
        let mut skip_tests = false;
        let mut skip_installer = false;

        let mut rest = args.iter();

        while let Some(argument) = rest.next() {
            match argument.as_str() {
                "--version" => version = Some(value("--version", rest.next())?),
                "--notes" => notes = Some(value("--notes", rest.next())?),
                "--channel" => channel = value("--channel", rest.next())?,
                "--manifest-repo" => {
                    manifest_repo = PathBuf::from(value("--manifest-repo", rest.next())?);
                }
                "--dry-run" => dry_run = true,
                "--skip-tests" => skip_tests = true,
                "--skip-installer" => skip_installer = true,
                other if other.starts_with("--") => {
                    return Err(CliError::Usage(format!("unknown release option '{other}'")));
                }
                other => {
                    return Err(CliError::Usage(format!(
                        "release takes no operands, got '{other}'"
                    )));
                }
            }
        }

        let version =
            version.ok_or_else(|| CliError::Usage("release needs --version".to_owned()))?;
        let notes = notes.ok_or_else(|| CliError::Usage("release needs --notes".to_owned()))?;

        Ok(Self {
            version,
            notes,
            channel,
            manifest_repo,
            dry_run,
            skip_tests,
            skip_installer,
        })
    }
}

/// Prints the release usage text.
const USAGE: &str = "\
usage: neb-cli release --version MAJOR.MINOR.PATCH --notes TEXT [options]

options:
  --channel NAME         release channel (default: beta)
  --manifest-repo DIR    working copy with latest.json and CHANGELOG.md
                         (default: A:\\ProjectNeb-wt\\site)
  --dry-run              print every command and file write, change nothing
  --skip-tests           do not run cargo test --workspace
  --skip-installer       do not build the Inno Setup installer
  -h, --help             this text
";

/// Runs `neb-cli release`.
///
/// # Errors
///
/// [`CliError::Usage`] for a malformed invocation, and [`CliError::Failed`] for anything the pipeline
/// could not do; the message names the exact command or path.
pub fn run(args: &[String]) -> Result<(), CliError> {
    if args
        .iter()
        .any(|argument| argument == "--help" || argument == "-h")
    {
        print!("{USAGE}");
        return Ok(());
    }

    execute(&Options::parse(args)?)
}

/// Runs the pipeline; `execute` is separate from [`run`] so the plan can be tested without a process.
fn execute(options: &Options) -> Result<(), CliError> {
    let repo_root = repo_root()?;
    let rust_dir = repo_root.join("rust");
    let cargo_toml = rust_dir.join("Cargo.toml");
    let current = read_workspace_version(&cargo_toml)?;
    let version = Version::parse(&options.version)?;

    if version <= current {
        return Err(CliError::Failed(format!(
            "version {version} must be greater than the current workspace version {current}"
        )));
    }

    if options.notes.trim().is_empty() {
        return Err(CliError::Failed("--notes must not be empty".to_owned()));
    }

    let channel = options.channel.trim().to_owned();
    validate_channel(&channel)?;

    if !options.manifest_repo.is_dir() {
        return Err(CliError::Failed(format!(
            "manifest repo '{}' is not a directory",
            options.manifest_repo.display()
        )));
    }

    let date = utc_today()?;
    let runner = Runner::new(options.dry_run);
    let portable_name = stage::portable_filename(&version, &channel);
    let setup_name = stage::setup_filename(&version, &channel);
    let dist = repo_root.join("dist");
    let zip_path = dist.join(&portable_name);
    let setup_path = dist.join(&setup_name);

    // Scratch space is real only for a real run; a dry run prints paths under the system temp folder
    // and never creates them.
    let scratch_guard = if options.dry_run {
        None
    } else {
        Some(
            tempfile::tempdir()
                .map_err(|error| CliError::Failed(format!("cannot create a temp dir: {error}")))?,
        )
    };
    let scratch = scratch_guard.as_ref().map_or_else(
        || env::temp_dir().join(format!("neb-release-{version}")),
        |dir| dir.path().to_path_buf(),
    );

    // 1. Rewrite the workspace version, which every crate inherits.
    let text = fs::read_to_string(&cargo_toml).map_err(|error| {
        CliError::Failed(format!("cannot read '{}': {error}", cargo_toml.display()))
    })?;
    let updated = rewrite_workspace_version(&text, &version.to_string()).ok_or_else(|| {
        CliError::Failed(format!(
            "'{}' has no [workspace.package] version to rewrite",
            cargo_toml.display()
        ))
    })?;
    runner.write(&cargo_toml, &updated)?;

    // 2. Tests.
    if !options.skip_tests {
        runner.command(
            &CommandSpec::new("cargo")
                .args(["test", "--workspace"])
                .cwd(&rust_dir),
        )?;
    }

    // 3. Release build.
    runner.command(
        &CommandSpec::new("cargo")
            .args(["build", "--workspace", "--release"])
            .cwd(&rust_dir),
    )?;

    // 4. Portable tree and archive.
    let stage_dir = scratch.join("portable");
    runner.create_dir(&stage_dir)?;
    let mut entries: Vec<(String, PathBuf)> = Vec::new();

    for (name, relative) in stage::COPIED_FILES {
        let source = join_relative(&repo_root, relative);

        if !options.dry_run && !source.is_file() {
            return Err(CliError::Failed(format!(
                "missing release input '{}' (run `cargo build --workspace --release` first)",
                source.display()
            )));
        }

        let target = stage_dir.join(name);
        runner.copy(&source, &target)?;
        entries.push((name.to_owned(), target));
    }

    for (name, contents) in stage::generated_files(&version) {
        let target = stage_dir.join(name);
        runner.write(&target, &contents)?;
        entries.push((name.to_owned(), target));
    }

    // The built dashboard travels with the portable tree, so the main window works offline.
    let ui_source = repo_root.join("src").join("Neb.UI").join("dist");
    let ui_files = stage::ui_entries(&ui_source).map_err(|error| {
        CliError::Failed(format!(
            "cannot read the built UI at {}: {error}",
            ui_source.display()
        ))
    })?;
    for (relative, source) in ui_files {
        let name = format!("ui/{relative}");
        let target = stage_dir.join(&name);
        if let Some(parent) = target.parent() {
            runner.create_dir(parent)?;
        }
        runner.copy(&source, &target)?;
        entries.push((name, target));
    }

    runner.create_dir(&dist)?;
    runner.create_zip(&zip_path, &entries)?;
    let portable_sha = runner.sha256(&zip_path, &portable_name)?;

    // 5. Installer.
    let mut setup_sha = None;

    if !options.skip_installer {
        let template_path = repo_root.join("installer").join("ProjectNeb.iss");
        let template = fs::read_to_string(&template_path).map_err(|error| {
            CliError::Failed(format!(
                "cannot read '{}': {error}",
                template_path.display()
            ))
        })?;
        let script = stage::inno_script(&template, &version, &channel, &repo_root)?;
        let script_path = scratch.join("ProjectNeb.iss");
        runner.write(&script_path, &script)?;

        let iscc = iscc_path()?;

        if !options.dry_run && !iscc.is_file() {
            return Err(CliError::Failed(format!(
                "Inno Setup compiler '{}' is missing; install it or set ISCC",
                iscc.display()
            )));
        }

        runner.command(
            &CommandSpec::new(iscc.display().to_string())
                .arg(script_path.display().to_string())
                .cwd(&repo_root),
        )?;

        if !options.dry_run && !setup_path.is_file() {
            return Err(CliError::Failed(format!(
                "Inno Setup finished but '{}' is missing",
                setup_path.display()
            )));
        }

        setup_sha = Some(runner.sha256(&setup_path, &setup_name)?);
    }

    // 6. GitHub release.
    let notes_path = scratch.join("notes.md");
    runner.write(&notes_path, &manifest::release_notes(&options.notes))?;

    let mut arguments = vec![
        "release".to_owned(),
        "create".to_owned(),
        format!("v{version}"),
    ];

    if setup_sha.is_some() {
        arguments.push(setup_path.display().to_string());
    }

    arguments.push(zip_path.display().to_string());
    arguments.extend([
        "--repo".to_owned(),
        RELEASE_REPO.to_owned(),
        "--title".to_owned(),
        format!("ProjectNeb {version} Beta"),
        "--notes-file".to_owned(),
        notes_path.display().to_string(),
    ]);
    runner.command(&CommandSpec::new("gh").args(arguments))?;

    // 7. Update manifest.
    let portable = manifest::Asset {
        file: portable_name.clone(),
        sha256: portable_sha,
    };
    let setup = setup_sha.map(|sha256| manifest::Asset {
        file: setup_name.clone(),
        sha256,
    });
    let latest = manifest::render_manifest(
        &version,
        &channel,
        &date,
        &options.notes,
        &portable,
        setup.as_ref(),
    )?;
    let manifest_path = options.manifest_repo.join("latest.json");
    runner.write(&manifest_path, &latest)?;

    // 8. Changelog.
    let changelog_path = options.manifest_repo.join("CHANGELOG.md");
    let existing = match fs::read_to_string(&changelog_path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => "# Changelog\n\n".to_owned(),
        Err(error) => {
            return Err(CliError::Failed(format!(
                "cannot read '{}': {error}",
                changelog_path.display()
            )));
        }
    };
    let section = manifest::changelog_section(&version, &date, &options.notes);
    runner.write(
        &changelog_path,
        &manifest::prepend_changelog(&existing, &section),
    )?;

    // 9. Publish the manifest repository.
    let manifest_dir = options.manifest_repo.display().to_string();
    runner.command(&CommandSpec::new("git").args(["-C", &manifest_dir, "add", "-A"]))?;
    runner.command(&CommandSpec::new("git").args([
        "-C",
        &manifest_dir,
        "commit",
        "-m",
        &format!("release: v{version}"),
    ]))?;
    runner.command(&CommandSpec::new("git").args(["-C", &manifest_dir, "push"]))?;

    if options.dry_run {
        println!("dry run complete: no command was run and no file was written");
    } else {
        println!("released v{version} ({channel}): {portable_name}");
    }

    Ok(())
}

/// Joins a `/`-separated repository-relative path onto `root`.
///
/// [`Path::join`] keeps the `/` separators of its argument, which would make Windows output read
/// `A:\repo\rust/target/release/...`; splitting first keeps the printed plan native.
fn join_relative(root: &Path, relative: &str) -> PathBuf {
    relative
        .split('/')
        .fold(root.to_path_buf(), |path, part| path.join(part))
}

/// Checks that a channel can appear in a tag and a file name.
fn validate_channel(channel: &str) -> Result<(), CliError> {
    if channel.is_empty()
        || !channel
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(CliError::Failed(format!(
            "channel '{channel}' must be letters, digits, '-' or '_'"
        )));
    }

    Ok(())
}

/// Finds the repository root from this crate's location at build time.
fn repo_root() -> Result<PathBuf, CliError> {
    let crate_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let root = crate_dir
        .ancestors()
        .nth(3)
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            CliError::Failed(format!(
                "cannot find the repository root above '{}'",
                crate_dir.display()
            ))
        })?;

    if !root.join("rust").join("Cargo.toml").is_file() {
        return Err(CliError::Failed(format!(
            "'{}' does not look like the ProjectNeb repository",
            root.display()
        )));
    }

    Ok(root)
}

/// Reads `[workspace.package] version` from a workspace manifest.
fn read_workspace_version(path: &Path) -> Result<Version, CliError> {
    let text = fs::read_to_string(path)
        .map_err(|error| CliError::Failed(format!("cannot read '{}': {error}", path.display())))?;
    let value = workspace_version_text(&text).ok_or_else(|| {
        CliError::Failed(format!(
            "'{}' has no [workspace.package] version",
            path.display()
        ))
    })?;

    Version::parse(value)
}

/// Extracts the value of `[workspace.package] version`.
fn workspace_version_text(text: &str) -> Option<&str> {
    let mut in_section = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed.starts_with('[') {
            in_section = trimmed == "[workspace.package]";
            continue;
        }

        if !in_section {
            continue;
        }

        let Some(rest) = trimmed.strip_prefix("version") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = value.trim_start().strip_prefix('"')?;

        return value.split('"').next();
    }

    None
}

/// Rebuilds a workspace manifest with a new `[workspace.package] version`.
///
/// Only the quoted value changes: indentation, spacing, comments and line endings stay as they were.
/// `None` when the section or its quoted value is absent.
fn rewrite_workspace_version(text: &str, new_version: &str) -> Option<String> {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let trailing_newline = text.ends_with('\n');
    let mut in_section = false;
    let mut replaced = false;
    let mut output = String::with_capacity(text.len() + new_version.len());

    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            output.push_str(newline);
        }

        let trimmed = line.trim_start();

        if trimmed.starts_with('[') {
            in_section = trimmed == "[workspace.package]";
        } else if in_section && !replaced {
            let is_version = trimmed
                .strip_prefix("version")
                .map(str::trim_start)
                .is_some_and(|rest| rest.starts_with('='));

            if is_version {
                let open = line.find('"')?;
                let close = open + 1 + line[open + 1..].find('"')?;
                output.push_str(&line[..=open]);
                output.push_str(new_version);
                output.push_str(&line[close..]);
                replaced = true;
                continue;
            }
        }

        output.push_str(line);
    }

    if !replaced {
        return None;
    }

    if trailing_newline {
        output.push_str(newline);
    }

    Some(output)
}

/// Today's date in UTC, `YYYY-MM-DD`.
fn utc_today() -> Result<String, CliError> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| CliError::Failed(format!("system clock is before 1970: {error}")))?
        .as_secs();

    Ok(manifest::utc_date(seconds))
}

/// Locates the Inno Setup command-line compiler.
fn iscc_path() -> Result<PathBuf, CliError> {
    if let Some(value) = env::var_os("ISCC") {
        if !value.is_empty() {
            return Ok(PathBuf::from(value));
        }
    }

    let local = env::var_os("LOCALAPPDATA").ok_or_else(|| {
        CliError::Failed(
            "LOCALAPPDATA is not set, so Inno Setup cannot be found; set ISCC".to_owned(),
        )
    })?;

    Ok(PathBuf::from(local)
        .join("Programs")
        .join("Inno Setup 6")
        .join("ISCC.exe"))
}

/// Reads the value of an option, which must be present.
fn value(option: &str, value: Option<&String>) -> Result<String, CliError> {
    value
        .cloned()
        .ok_or_else(|| CliError::Usage(format!("{option} needs a value")))
}

/// One external command, with the directory it runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: String,
    arguments: Vec<String>,
    cwd: Option<PathBuf>,
}

impl CommandSpec {
    /// Starts a command line with its program.
    fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            arguments: Vec::new(),
            cwd: None,
        }
    }

    /// Appends one argument.
    fn arg(mut self, argument: impl Into<String>) -> Self {
        self.arguments.push(argument.into());
        self
    }

    /// Appends several arguments.
    fn args<I, S>(mut self, arguments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.arguments.extend(arguments.into_iter().map(Into::into));
        self
    }

    /// Runs the command in `directory`.
    fn cwd(mut self, directory: &Path) -> Self {
        self.cwd = Some(directory.to_path_buf());
        self
    }

    /// Renders the line exactly as it would be typed into `cmd.exe`.
    fn display(&self) -> String {
        let line = quote::command(&self.program, &self.arguments);

        match &self.cwd {
            Some(directory) => format!(
                "cd {} && {line}",
                quote::argument(&directory.display().to_string())
            ),
            None => line,
        }
    }
}

/// Carries out the pipeline, or prints it when `dry_run` is set.
struct Runner {
    dry_run: bool,
}

impl Runner {
    /// Creates a runner for one mode.
    const fn new(dry_run: bool) -> Self {
        Self { dry_run }
    }

    /// Writes a text file.
    fn write(&self, path: &Path, contents: &str) -> Result<(), CliError> {
        if self.dry_run {
            println!("would write {} ({} bytes)", path.display(), contents.len());
            return Ok(());
        }

        println!("write {}", path.display());
        fs::write(path, contents).map_err(|error| {
            CliError::Failed(format!("cannot write '{}': {error}", path.display()))
        })
    }

    /// Copies a file.
    fn copy(&self, source: &Path, destination: &Path) -> Result<(), CliError> {
        if self.dry_run {
            println!(
                "would copy {} -> {}",
                source.display(),
                destination.display()
            );
            return Ok(());
        }

        println!("copy {} -> {}", source.display(), destination.display());
        stage::copy_file(source, destination)
    }

    /// Creates a directory.
    fn create_dir(&self, path: &Path) -> Result<(), CliError> {
        if self.dry_run {
            println!("would create directory {}", path.display());
            return Ok(());
        }

        fs::create_dir_all(path).map_err(|error| {
            CliError::Failed(format!("cannot create '{}': {error}", path.display()))
        })
    }

    /// Builds the zip archive.
    fn create_zip(&self, archive: &Path, entries: &[(String, PathBuf)]) -> Result<(), CliError> {
        if self.dry_run {
            println!("would create zip {}", archive.display());

            for (name, source) in entries {
                println!("  + {name} <- {}", source.display());
            }

            return Ok(());
        }

        println!("zip {}", archive.display());
        stage::create_zip(archive, entries)
    }

    /// Hashes a built asset; a dry run has no bytes to hash and prints a placeholder instead.
    fn sha256(&self, path: &Path, name: &str) -> Result<String, CliError> {
        if self.dry_run {
            return Ok(format!("<sha256 of {name}>"));
        }

        let digest = stage::sha256(path)?;
        println!("sha256 {name} {digest}");
        Ok(digest)
    }

    /// Runs one command, printing its line first and failing with that same line.
    fn command(&self, spec: &CommandSpec) -> Result<(), CliError> {
        if self.dry_run {
            println!("would run: {}", spec.display());
            return Ok(());
        }

        println!("+ {}", spec.display());

        let mut command = Command::new(&spec.program);
        command.args(&spec.arguments);

        if let Some(directory) = &spec.cwd {
            command.current_dir(directory);
        }

        let output = command.output().map_err(|error| {
            CliError::Failed(format!("cannot run `{}`: {error}", spec.display()))
        })?;

        if !output.status.success() {
            let code = output
                .status
                .code()
                .map_or_else(|| "terminated".to_owned(), |code| code.to_string());
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);

            return Err(CliError::Failed(format!(
                "command failed (exit {code}): {}\n{}{}",
                spec.display(),
                stdout.trim_end(),
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!("\n{}", stderr.trim_end())
                }
            )));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        if !stdout.trim().is_empty() {
            println!("{}", stdout.trim_end());
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKSPACE: &str = "# top\n[workspace]\nmembers = []\n\n[workspace.package]\nversion = \"0.1.0\"\nedition = \"2024\"\n";

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn release_requires_a_version_and_notes() {
        assert!(matches!(
            Options::parse(&argv(&["--notes", "x"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            Options::parse(&argv(&["--version", "0.1.1"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            Options::parse(&argv(&["--version"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn the_defaults_match_the_documented_behaviour() {
        let options =
            Options::parse(&argv(&["--version", "0.1.1", "--notes", "x"])).expect("parse");

        assert_eq!(options.channel, "beta");
        assert_eq!(options.manifest_repo, PathBuf::from(DEFAULT_MANIFEST_REPO));
        assert!(!options.dry_run);
        assert!(!options.skip_tests);
        assert!(!options.skip_installer);
    }

    #[test]
    fn every_flag_is_read() {
        let options = Options::parse(&argv(&[
            "--version",
            "0.2.0",
            "--notes",
            "notes",
            "--channel",
            "stable",
            "--manifest-repo",
            "C:\\site",
            "--dry-run",
            "--skip-tests",
            "--skip-installer",
        ]))
        .expect("parse");

        assert_eq!(options.version, "0.2.0");
        assert_eq!(options.notes, "notes");
        assert_eq!(options.channel, "stable");
        assert_eq!(options.manifest_repo, PathBuf::from("C:\\site"));
        assert!(options.dry_run && options.skip_tests && options.skip_installer);
    }

    #[test]
    fn unknown_options_and_operands_are_refused() {
        assert!(matches!(
            Options::parse(&argv(&["--version", "0.1.1", "--notes", "x", "--verbose"])),
            Err(CliError::Usage(_))
        ));
        assert!(matches!(
            Options::parse(&argv(&["--version", "0.1.1", "--notes", "x", "stray"])),
            Err(CliError::Usage(_))
        ));
    }

    #[test]
    fn the_workspace_version_is_read_from_its_own_section() {
        assert_eq!(workspace_version_text(WORKSPACE), Some("0.1.0"));

        let with_package =
            "[package]\nversion = \"9.9.9\"\n\n[workspace.package]\nversion = \"0.1.0\"\n";
        assert_eq!(workspace_version_text(with_package), Some("0.1.0"));
        assert_eq!(
            workspace_version_text("[package]\nversion = \"9.9.9\"\n"),
            None
        );
    }

    #[test]
    fn rewriting_touches_only_the_workspace_version() {
        let with_package = format!("{WORKSPACE}\n[package]\nversion = \"1.0.0\"\n");
        let updated = rewrite_workspace_version(&with_package, "0.1.1").expect("rewrite");

        assert!(updated.contains("[workspace.package]\nversion = \"0.1.1\""));
        assert!(updated.contains("[package]\nversion = \"1.0.0\""));
        assert!(updated.ends_with('\n'));
    }

    #[test]
    fn rewriting_preserves_crlf_and_comments() {
        let text = "[workspace.package]\r\nversion   = \"0.1.0\" # shared\r\n";
        let updated = rewrite_workspace_version(text, "0.2.0").expect("rewrite");

        assert_eq!(
            updated,
            "[workspace.package]\r\nversion   = \"0.2.0\" # shared\r\n"
        );
    }

    #[test]
    fn rewriting_without_the_section_fails() {
        assert_eq!(
            rewrite_workspace_version("[package]\nversion = \"0.1.0\"\n", "0.2.0"),
            None
        );
    }

    #[test]
    fn channels_are_restricted_to_tag_safe_characters() {
        assert!(validate_channel("beta").is_ok());
        assert!(validate_channel("rc-1").is_ok());
        assert!(validate_channel("nightly_2").is_ok());
        assert!(validate_channel("").is_err());
        assert!(validate_channel("beta/1").is_err());
        assert!(validate_channel("beta 1").is_err());
    }

    #[test]
    fn a_missing_program_fails_with_its_command_line() {
        let error = Runner::new(false)
            .command(&CommandSpec::new("neb-cli-no-such-program").arg("--version"))
            .expect_err("the program does not exist");

        assert!(
            error
                .to_string()
                .contains("neb-cli-no-such-program --version"),
            "{error}"
        );
    }

    #[test]
    fn a_non_zero_exit_reports_the_code_and_the_line() {
        let error = Runner::new(false)
            .command(&CommandSpec::new("cmd").args(["/C", "exit", "3"]))
            .expect_err("exit 3 is a failure");

        let text = error.to_string();
        assert!(text.contains("command failed (exit 3)"), "{text}");
        assert!(text.contains("cmd /C exit 3"), "{text}");
    }

    #[test]
    fn a_dry_runner_does_not_run_anything() {
        assert!(
            Runner::new(true)
                .command(&CommandSpec::new("cmd").args(["/C", "exit", "3"]))
                .is_ok()
        );
    }

    #[test]
    fn repository_relative_sources_use_native_separators() {
        let path = join_relative(Path::new(r"A:\repo"), "rust/target/release/neb-cli.exe");

        assert_eq!(
            path,
            PathBuf::from(r"A:\repo\rust\target\release\neb-cli.exe")
        );
    }

    #[test]
    fn command_lines_include_the_working_directory() {
        let spec = CommandSpec::new("cargo")
            .args(["test", "--workspace"])
            .cwd(Path::new(r"A:\ProjectNeb\rust"));

        assert_eq!(
            spec.display(),
            r#"cd A:\ProjectNeb\rust && cargo test --workspace"#
        );
    }
}
