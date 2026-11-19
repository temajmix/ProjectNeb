//! Reading the pack's `general*.bat` scripts into executable argument vectors.
//!
//! A strategy script is a small cmd program: it sets `BIN` and `LISTS` from `%~dp0`, calls
//! `service.bat` for the game-filter variables, and ends with one `start … winws.exe …` line that
//! continues over a dozen physical lines with `^`. Running that line through `cmd.exe` would mean
//! running all the `call`s too, so the module performs the small, safe subset the scripts actually
//! use: `set` with expansion, `%~dp0`/`%~n0`/`%~f0`, `%NAME%`, caret line continuations, comments,
//! and Windows command-line quoting.
//!
//! Anything else is ignored, and a script without a `winws.exe` invocation is reported as skipped
//! rather than silently disappearing.

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::pack::{BIN_DIR, Pack, UTILS_DIR};

/// Prefix every strategy file starts with.
pub const STRATEGY_PREFIX: &str = "general";

/// The two ranges `service.bat` uses when the game filter is on for both protocols.
const GAME_FILTER_ALL: &str = "1024-65535";

/// What `winws.exe` runs when the game filter is disabled.
const GAME_FILTER_OFF: &str = "12";

/// One strategy script, parsed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Strategy {
    /// File stem, e.g. `general (ALT)`.
    pub name: String,
    /// File name, e.g. `general (ALT).bat`.
    pub file: String,
    /// Absolute path of `winws.exe`.
    pub program: PathBuf,
    /// Arguments after the program, with quotes and variables resolved.
    pub args: Vec<String>,
    /// Number of arguments after `winws.exe`, serialized for the dashboard.
    pub argument_count: usize,
}

impl Strategy {
    /// The full command line as argv, program first.
    pub fn argv(&self) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.args.len() + 1);
        argv.push(self.program.display().to_string());
        argv.extend(self.args.iter().cloned());
        argv
    }

    /// Number of arguments after `winws.exe`.
    pub fn argument_count(&self) -> usize {
        self.argument_count
    }
}

/// The parsed strategies of one pack, plus the scripts that could not be used.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Strategies {
    /// Usable strategies, sorted by file name.
    pub strategies: Vec<Strategy>,
    /// Scripts that exist but do not invoke `winws.exe`.
    pub skipped: Vec<SkippedStrategy>,
}

/// A strategy script that was deliberately left out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SkippedStrategy {
    /// File name.
    pub file: String,
    /// Why it was skipped.
    pub reason: String,
}

/// Why a script could not be parsed.
#[derive(Debug, thiserror::Error)]
pub enum StrategyError {
    /// The folder or one of its files could not be read.
    #[error("cannot read '{}': {source}", path.display())]
    Io {
        /// File or directory that failed.
        path: PathBuf,
        /// The underlying error.
        #[source]
        source: io::Error,
    },

    /// The script never invokes `winws.exe`.
    #[error("'{file}' does not invoke winws.exe")]
    NoWinws {
        /// File name of the script.
        file: String,
    },
}

/// Reads every `general*.bat` in `pack`.
///
/// A script that cannot be used is listed in [`Strategies::skipped`]; a directory that cannot be
/// read is an error, because the caller cannot tell an empty pack from an unreadable one.
///
/// # Errors
///
/// [`StrategyError::Io`] when the pack root cannot be read.
pub fn load(pack: &Pack) -> Result<Strategies, StrategyError> {
    let mut files = Vec::new();

    for entry in fs::read_dir(pack.root()).map_err(|source| StrategyError::Io {
        path: pack.root().to_path_buf(),
        source,
    })? {
        let entry = entry.map_err(|source| StrategyError::Io {
            path: pack.root().to_path_buf(),
            source,
        })?;
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();

        if path.is_file() && name.starts_with(STRATEGY_PREFIX) && name.ends_with(".bat") {
            files.push(path);
        }
    }

    files.sort_by_key(|path| {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        (name != "general.bat", name)
    });

    let mut strategies = Vec::new();
    let mut skipped = Vec::new();

    for path in files {
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();

        match fs::read(&path) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(strip_bom(&bytes));
                match parse_text(&file, &text, pack.root()) {
                    Ok(strategy) => strategies.push(strategy),
                    Err(error) => skipped.push(SkippedStrategy {
                        file,
                        reason: error.to_string(),
                    }),
                }
            }
            Err(source) => skipped.push(SkippedStrategy {
                file,
                reason: StrategyError::Io { path, source }.to_string(),
            }),
        }
    }

    Ok(Strategies {
        strategies,
        skipped,
    })
}

/// Parses one script. `root` is the pack folder `%~dp0` expands to.
///
/// # Errors
///
/// [`StrategyError::NoWinws`] when the script never invokes `winws.exe`.
pub fn parse_text(file: &str, text: &str, root: &Path) -> Result<Strategy, StrategyError> {
    let name = Path::new(file)
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .unwrap_or_else(|| file.to_owned());
    let root_text = with_separator(root);
    let bat = root.join(file);
    let mut vars = seed_variables(root);

    for line in logical_lines(text) {
        let line = line.trim();
        if line.is_empty() || is_comment(line) {
            continue;
        }

        if let Some((key, value)) = parse_set(line) {
            let value = expand(&value, &vars, &root_text, &bat, &name);
            vars.insert(key.to_ascii_uppercase(), value);
            continue;
        }

        if is_ignored_command(line) {
            continue;
        }

        let expanded = expand(line, &vars, &root_text, &bat, &name);
        let tokens = split_command_line(&expanded);
        let Some(index) = tokens.iter().position(|token| is_winws(token)) else {
            continue;
        };

        let args = tokens[index + 1..].to_vec();

        return Ok(Strategy {
            name,
            file: file.to_owned(),
            program: resolve_program(&tokens[index], root),
            argument_count: args.len(),
            args,
        });
    }

    Err(StrategyError::NoWinws {
        file: file.to_owned(),
    })
}

/// Joins physical lines that end in a caret and drops the caret.
fn logical_lines(text: &str) -> Vec<String> {
    let mut lines = Vec::new();
    let mut pending = String::new();
    let mut continued = false;

    for raw in text.lines() {
        let line = raw.trim_end();
        let carets = line
            .chars()
            .rev()
            .take_while(|character| *character == '^')
            .count();

        if continued {
            pending.push_str(line);
        } else {
            pending.clear();
            pending.push_str(line);
        }

        if carets % 2 == 1 {
            pending.pop();
            pending.push(' ');
            continued = true;
            continue;
        }

        lines.push(std::mem::take(&mut pending));
        continued = false;
    }

    if !pending.is_empty() {
        lines.push(pending);
    }

    lines
}

fn is_comment(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();

    lower.starts_with("::") || lower.starts_with(':') || lower == "rem" || lower.starts_with("rem ")
}

/// Whether the line is a cmd statement the module does not need to execute.
fn is_ignored_command(line: &str) -> bool {
    let line = line.trim_start_matches('@');
    let command = line
        .split(|character: char| character.is_whitespace() || character == '(')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase();

    matches!(
        command.as_str(),
        "echo"
            | "rem"
            | "chcp"
            | "cd"
            | "chdir"
            | "call"
            | "title"
            | "cls"
            | "pause"
            | "exit"
            | "if"
            | "for"
            | "goto"
            | "setlocal"
            | "endlocal"
            | "shift"
            | "del"
            | "copy"
            | "move"
            | "mkdir"
            | "md"
            | "rd"
            | "rmdir"
            | "net"
            | "sc"
            | "reg"
            | "taskkill"
    )
}

/// Parses `set "NAME=VALUE"` or `set NAME=VALUE`.
fn parse_set(line: &str) -> Option<(String, String)> {
    let lower = line.to_ascii_lowercase();
    if !lower.starts_with("set") {
        return None;
    }

    let rest = line.get(3..)?;
    if !rest.starts_with([' ', '\t']) {
        return None;
    }

    let rest = rest.trim_start();
    let (inner, quoted) = match rest.strip_prefix('"') {
        Some(inner) => (inner, true),
        None => (rest, false),
    };
    let inner = if quoted {
        inner.split_once('"').map_or(inner, |(inside, _)| inside)
    } else {
        inner
    };
    let (key, value) = inner.split_once('=')?;
    let key = key.trim();

    if key.is_empty() {
        return None;
    }

    Some((key.to_owned(), value.to_owned()))
}

/// Variables `service.bat load_game_filter` would have defined before the strategy line runs.
fn seed_variables(root: &Path) -> BTreeMap<String, String> {
    let mut tcp = GAME_FILTER_OFF.to_owned();
    let mut udp = GAME_FILTER_OFF.to_owned();

    if let Ok(text) = fs::read_to_string(root.join(UTILS_DIR).join("game_filter.enabled")) {
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let (key, value) = match line.split_once('=') {
                Some((key, value)) => (key.trim().to_ascii_lowercase(), value.trim().to_owned()),
                None => (line.to_ascii_lowercase(), String::new()),
            };

            match key.as_str() {
                "all" => {
                    tcp = GAME_FILTER_ALL.to_owned();
                    udp = GAME_FILTER_ALL.to_owned();
                }
                "tcp" if !value.is_empty() => tcp = value,
                "udp" if !value.is_empty() => udp = value,
                "mode" if value.eq_ignore_ascii_case("all") => {
                    tcp = GAME_FILTER_ALL.to_owned();
                    udp = GAME_FILTER_ALL.to_owned();
                }
                "mode" if value.eq_ignore_ascii_case("tcp") => tcp = GAME_FILTER_ALL.to_owned(),
                "mode" if value.eq_ignore_ascii_case("udp") => udp = GAME_FILTER_ALL.to_owned(),
                _ => {}
            }
        }
    }

    let mut vars = BTreeMap::new();
    vars.insert("GAMEFILTERTCP".to_owned(), tcp.clone());
    vars.insert("GAMEFILTERUDP".to_owned(), udp);
    vars.insert("GAMEFILTER".to_owned(), tcp);
    vars
}

/// Expands the percent forms the strategy scripts use.
fn expand(
    text: &str,
    vars: &BTreeMap<String, String>,
    root: &str,
    bat: &Path,
    name: &str,
) -> String {
    let bat = bat.display().to_string();
    let mut out = String::with_capacity(text.len());
    let mut rest = text;

    while let Some(position) = rest.find('%') {
        out.push_str(&rest[..position]);
        let after = &rest[position + 1..];

        if let Some(tail) = strip_builtin(after, "~dp0") {
            out.push_str(root);
            rest = tail;
            continue;
        }
        if let Some(tail) = strip_builtin(after, "~f0") {
            out.push_str(&bat);
            rest = tail;
            continue;
        }
        if let Some(tail) = strip_builtin(after, "~n0") {
            out.push_str(name);
            rest = tail;
            continue;
        }
        if let Some(tail) = after.strip_prefix('%') {
            out.push('%');
            rest = tail;
            continue;
        }

        let Some(end) = after.find('%') else {
            out.push('%');
            rest = after;
            continue;
        };
        let variable = &after[..end];

        if variable.is_empty() || variable.contains(['%', ' ', '\t']) {
            out.push('%');
            rest = after;
            continue;
        }

        if let Some(value) = vars.get(&variable.to_ascii_uppercase()) {
            out.push_str(value);
        } else if let Ok(value) = std::env::var(variable) {
            out.push_str(&value);
        }
        // An undefined variable expands to nothing, which is what cmd does.

        rest = &after[end + 1..];
    }

    out.push_str(rest);
    out
}

/// Matches a `%~xx0` form case-insensitively and returns the rest of the text.
fn strip_builtin<'a>(text: &'a str, token: &str) -> Option<&'a str> {
    let head = text.get(..token.len())?;

    if head.eq_ignore_ascii_case(token) {
        text.get(token.len()..)
    } else {
        None
    }
}

/// Splits a Windows command line into argv the way `CommandLineToArgvW` does.
pub fn split_command_line(line: &str) -> Vec<String> {
    let characters: Vec<char> = line.chars().collect();
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut index = 0;

    while index < characters.len() {
        let character = characters[index];

        if character == '\\' {
            let mut count = 0;
            while index < characters.len() && characters[index] == '\\' {
                count += 1;
                index += 1;
            }

            if index < characters.len() && characters[index] == '"' {
                for _ in 0..count / 2 {
                    current.push('\\');
                }
                if count % 2 == 0 {
                    quoted = !quoted;
                } else {
                    current.push('"');
                }
                index += 1;
            } else {
                for _ in 0..count {
                    current.push('\\');
                }
            }

            started = true;
            continue;
        }

        if character == '"' {
            quoted = !quoted;
            started = true;
            index += 1;
            continue;
        }

        if !quoted && (character == ' ' || character == '\t') {
            if started {
                tokens.push(std::mem::take(&mut current));
                started = false;
            }
            index += 1;
            continue;
        }

        current.push(character);
        started = true;
        index += 1;
    }

    if started {
        tokens.push(current);
    }

    tokens
}

fn is_winws(token: &str) -> bool {
    Path::new(token)
        .file_name()
        .is_some_and(|name| name.to_string_lossy().eq_ignore_ascii_case("winws.exe"))
}

fn resolve_program(token: &str, root: &Path) -> PathBuf {
    let path = Path::new(token);

    if path.is_absolute() {
        return path.to_path_buf();
    }

    if path.components().count() == 1 {
        return root.join(BIN_DIR).join(path);
    }

    root.join(path)
}

fn with_separator(path: &Path) -> String {
    let mut text = path.display().to_string();

    if !text.ends_with('\\') {
        text.push('\\');
    }

    text
}

fn strip_bom(bytes: &[u8]) -> &[u8] {
    bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    /// A script shaped like the pack's, with the parts that need parser support.
    const FIXTURE: &str = "@echo off\r\n\
chcp 65001 > nul\r\n\
:: 65001 - UTF-8\r\n\
rem a comment\r\n\
cd /d \"%~dp0\"\r\n\
call service.bat status_zapret\r\n\
echo:\r\n\
set \"BIN=%~dp0bin\\\"\r\n\
set \"LISTS=%~dp0lists\\\"\r\n\
start \"zapret: %~n0\" /min \"%BIN%winws.exe\" --wf-tcp=80,443,%GameFilterTCP% ^\r\n\
--filter-tcp=443 --hostlist=\"%LISTS%list-general.txt\" ^\r\n\
--dpi-desync=fake --dpi-desync-fake-tls=\"%BIN%tls_clienthello_www_google_com.bin\"\r\n";

    fn root_with_game_filter(flag: Option<&str>) -> (tempfile::TempDir, PathBuf) {
        let directory = tempdir().expect("temp dir");
        let root = directory.path().join("pack");
        fs::create_dir_all(root.join(UTILS_DIR)).expect("utils");
        if let Some(flag) = flag {
            fs::write(root.join(UTILS_DIR).join("game_filter.enabled"), flag).expect("flag");
        }

        (directory, root)
    }

    #[test]
    fn a_pack_strategy_becomes_a_program_and_an_argv() {
        let (_directory, root) = root_with_game_filter(Some("all\n"));
        let strategy = parse_text("general.bat", FIXTURE, &root).expect("parse");

        assert_eq!(strategy.name, "general");
        assert_eq!(strategy.file, "general.bat");
        assert_eq!(strategy.program, root.join(BIN_DIR).join("winws.exe"));
        assert_eq!(strategy.args[0], "--wf-tcp=80,443,1024-65535");
        assert_eq!(strategy.args[1], "--filter-tcp=443");
        assert_eq!(
            strategy.args[2],
            format!(r"--hostlist={}\lists\list-general.txt", root.display()),
        );
        assert_eq!(strategy.args.len(), 5);
        assert_eq!(strategy.argument_count(), 5);
        assert_eq!(strategy.argv().len(), 6);
        assert_eq!(strategy.argv()[0], strategy.program.display().to_string());
    }

    #[test]
    fn the_game_filter_defaults_to_twelve_when_the_flag_is_absent() {
        let (_directory, root) = root_with_game_filter(None);
        let strategy = parse_text("general.bat", FIXTURE, &root).expect("parse");

        assert_eq!(strategy.args[0], "--wf-tcp=80,443,12");
    }

    #[test]
    fn the_game_filter_flag_carries_one_protocol_range() {
        let (_directory, root) = root_with_game_filter(Some("tcp=5000-6000\n"));
        let strategy = parse_text("general.bat", FIXTURE, &root).expect("parse");

        assert_eq!(strategy.args[0], "--wf-tcp=80,443,5000-6000");
    }

    #[test]
    fn line_continuations_join_and_comments_disappear() {
        let (_directory, root) = root_with_game_filter(None);
        let strategy = parse_text("general.bat", FIXTURE, &root).expect("parse");

        for argument in &strategy.args {
            assert!(!argument.contains('^'), "{argument}");
            assert!(!argument.starts_with("::"), "{argument}");
            assert!(!argument.contains("chcp"), "{argument}");
        }
    }

    #[test]
    fn quoted_arguments_with_spaces_stay_one_argument() {
        let text = "start \"\" \"%~dp0bin\\winws.exe\" --hostlist=\"C:\\Program Files\\x list.txt\" --new\r\n";
        let (_directory, root) = root_with_game_filter(None);
        let strategy = parse_text("general (ALT).bat", text, &root).expect("parse");

        assert_eq!(strategy.name, "general (ALT)");
        assert_eq!(
            strategy.args,
            vec![
                r"--hostlist=C:\Program Files\x list.txt".to_owned(),
                "--new".to_owned()
            ]
        );
    }

    #[test]
    fn a_script_without_winws_is_an_error_the_caller_reports() {
        let (_directory, root) = root_with_game_filter(None);

        match parse_text("general.bat", "@echo off\r\necho nothing\r\n", &root) {
            Err(StrategyError::NoWinws { file }) => assert_eq!(file, "general.bat"),
            other => panic!("expected NoWinws, got {other:?}"),
        }
    }

    #[test]
    fn a_script_that_only_echoes_the_name_does_not_count_as_an_invocation() {
        let (_directory, root) = root_with_game_filter(None);
        let text = "@echo off\r\necho \"%BIN%winws.exe\"\r\n";

        assert!(matches!(
            parse_text("general.bat", text, &root),
            Err(StrategyError::NoWinws { .. })
        ));
    }

    #[test]
    fn an_unquoted_relative_winws_is_resolved_under_bin() {
        let (_directory, root) = root_with_game_filter(None);
        let strategy = parse_text("general.bat", "winws.exe --new\r\n", &root).expect("parse");

        assert_eq!(strategy.program, root.join(BIN_DIR).join("winws.exe"));
        assert_eq!(strategy.args, vec!["--new".to_owned()]);
    }

    #[test]
    fn a_set_line_may_omit_the_quotes() {
        let (_directory, root) = root_with_game_filter(None);
        let text = "set BIN=%~dp0bin\\\r\n\"%BIN%winws.exe\" --new\r\n";
        let strategy = parse_text("general.bat", text, &root).expect("parse");

        assert_eq!(strategy.program, root.join(BIN_DIR).join("winws.exe"));
    }

    #[test]
    fn the_tokenizer_follows_the_windows_backslash_and_quote_rules() {
        assert_eq!(split_command_line(r#"a "b c" d"#), vec!["a", "b c", "d"]);
        assert_eq!(
            split_command_line(r#""C:\a b\x.exe" --new"#),
            vec![r"C:\a b\x.exe", "--new"]
        );
        assert_eq!(
            split_command_line(r#""C:\path\\""#),
            vec![r"C:\path\"],
            "two backslashes before a quote become one literal backslash"
        );
        assert!(split_command_line("   ").is_empty());
    }

    #[test]
    fn loading_reads_only_general_bats_and_sorts_them() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path().join("pack");
        fs::create_dir_all(root.join(BIN_DIR)).expect("bin");
        fs::write(root.join(BIN_DIR).join("winws.exe"), b"exe").expect("winws");
        fs::write(root.join(BIN_DIR).join("WinDivert64.sys"), b"sys").expect("sys");
        fs::write(root.join("general (ALT).bat"), FIXTURE).expect("alt");
        fs::write(root.join("general.bat"), FIXTURE).expect("general");
        fs::write(root.join("notes.bat"), "--not a strategy").expect("notes");
        fs::write(root.join("general (BAD).bat"), "echo nothing").expect("bad");

        let pack = Pack::open(&root).expect("pack");
        let loaded = load(&pack).expect("load");

        let names: Vec<&str> = loaded
            .strategies
            .iter()
            .map(|strategy| strategy.name.as_str())
            .collect();
        assert_eq!(names, vec!["general", "general (ALT)"]);
        assert_eq!(loaded.skipped.len(), 1);
        assert_eq!(loaded.skipped[0].file, "general (BAD).bat");
        assert!(loaded.skipped[0].reason.contains("winws.exe"));
    }

    #[test]
    fn loading_an_unreadable_folder_is_an_error_not_an_empty_list() {
        let directory = tempdir().expect("temp dir");
        let root = directory.path().join("pack");
        fs::create_dir_all(root.join(BIN_DIR)).expect("bin");
        fs::write(root.join(BIN_DIR).join("winws.exe"), b"exe").expect("winws");
        fs::write(root.join(BIN_DIR).join("WinDivert64.sys"), b"sys").expect("sys");
        let pack = Pack::open(&root).expect("pack");

        fs::remove_dir_all(&root).expect("remove the pack");

        assert!(matches!(load(&pack), Err(StrategyError::Io { .. })));
    }
}
