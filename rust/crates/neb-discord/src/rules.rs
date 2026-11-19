//! Foreground-to-preset rules.
//!
//! The rules are plain data from `config.json`; no hooks, no injected DLL. Every
//! [`POLL_INTERVAL`] the service samples the foreground window and compares it with the rules in
//! order. The first enabled match wins, and a preset pinned with `discord.setPreset` wins over every
//! rule until it is cleared, which is what makes a manual choice stick.
//!
//! A pattern may name the field explicitly (`exe:`, `class:`, `title:`) or apply to all three. `*`
//! is a wildcard and comparisons are case-insensitive; without a wildcard a pattern is a substring
//! match. `exe:` also matches the executable's base name, so `exe:game.exe` works with any path.

use neb_core::config::DiscordRule;

/// How often the foreground window is sampled.
pub const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

/// The parts of the foreground window a rule can see.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Foreground {
    /// Full executable path, e.g. `C:\Games\game.exe`.
    pub exe: String,
    /// Window class, e.g. `Chrome_WidgetWin_1`.
    pub class: String,
    /// Window title as the user sees it.
    pub title: String,
}

impl Foreground {
    /// A sample for tests and for the diagnostic output.
    pub fn new(exe: impl Into<String>, class: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            exe: exe.into(),
            class: class.into(),
            title: title.into(),
        }
    }

    /// Whether every field is empty, which is what a locked or switching desktop produces.
    pub fn is_empty(&self) -> bool {
        self.exe.is_empty() && self.class.is_empty() && self.title.is_empty()
    }
}

/// Which preset the service should show, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    /// Preset name.
    pub preset: String,
    /// `manual` or `rule`.
    pub source: &'static str,
    /// Index of the rule that won, when `source` is `rule`.
    pub rule: Option<usize>,
}

/// Chooses the preset for `foreground`.
///
/// A non-empty `active` preset is returned first with source `manual`. Otherwise the first enabled
/// rule whose pattern matches wins. `None` means no rule matched, which the service renders as "no
/// activity" rather than leaving a stale one on screen.
pub fn select(
    rules: &[DiscordRule],
    foreground: Option<&Foreground>,
    active: Option<&str>,
) -> Option<Selection> {
    if let Some(active) = active.map(str::trim).filter(|name| !name.is_empty()) {
        return Some(Selection {
            preset: active.to_owned(),
            source: "manual",
            rule: None,
        });
    }

    let foreground = foreground.filter(|foreground| !foreground.is_empty())?;

    for (index, rule) in rules.iter().enumerate() {
        if !rule.enabled {
            continue;
        }
        let preset = rule.preset.trim();
        if preset.is_empty() {
            continue;
        }

        if matches(rule, foreground) {
            return Some(Selection {
                preset: preset.to_owned(),
                source: "rule",
                rule: Some(index),
            });
        }
    }

    None
}

/// Whether one rule matches one sample.
pub fn matches(rule: &DiscordRule, foreground: &Foreground) -> bool {
    let (target, pattern) = split_prefix(&rule.pattern);
    let pattern = pattern.trim().to_lowercase();
    if pattern.is_empty() {
        return false;
    }

    match target {
        Target::Exe => exe_matches(&pattern, &foreground.exe),
        Target::Class => glob(&pattern, &foreground.class.to_lowercase()),
        Target::Title => glob(&pattern, &foreground.title.to_lowercase()),
        Target::Any => {
            exe_matches(&pattern, &foreground.exe)
                || glob(&pattern, &foreground.class.to_lowercase())
                || glob(&pattern, &foreground.title.to_lowercase())
        }
    }
}

/// Which field a pattern is aimed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Any,
    Exe,
    Class,
    Title,
}

/// Reads an optional `exe:`, `class:` or `title:` prefix, case-insensitively.
fn split_prefix(pattern: &str) -> (Target, &str) {
    let trimmed = pattern.trim();
    for (prefix, target) in [
        ("exe:", Target::Exe),
        ("class:", Target::Class),
        ("title:", Target::Title),
    ] {
        if let Some(head) = trimmed.get(..prefix.len()) {
            if head.eq_ignore_ascii_case(prefix) {
                return (target, &trimmed[prefix.len()..]);
            }
        }
    }

    (Target::Any, trimmed)
}

/// Matches an executable against a path or its base name.
fn exe_matches(pattern: &str, exe: &str) -> bool {
    let exe = exe.to_lowercase();
    let base = exe
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(exe.as_str())
        .to_owned();

    glob(pattern, &exe) || glob(pattern, &base)
}

/// Case-insensitive glob: `*` matches any run of characters, every other character is literal.
fn glob(pattern: &str, text: &str) -> bool {
    if !pattern.contains('*') {
        return text.contains(pattern);
    }

    let mut position = 0;
    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');

    for (index, part) in pattern.split('*').enumerate() {
        if part.is_empty() {
            continue;
        }

        if index == 0 && anchored_start {
            if !text[position..].starts_with(part) {
                return false;
            }
            position += part.len();
            continue;
        }

        match text[position..].find(part) {
            Some(offset) => position += offset + part.len(),
            None => return false,
        }
    }

    if anchored_end {
        let tail = pattern.rsplit('*').next().unwrap_or_default();
        if !tail.is_empty() && !text.ends_with(tail) {
            return false;
        }
    }

    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str, preset: &str, enabled: bool) -> DiscordRule {
        DiscordRule {
            pattern: pattern.to_owned(),
            preset: preset.to_owned(),
            enabled,
        }
    }

    fn game() -> Foreground {
        Foreground::new(
            r"C:\Games\Nightfall\nightfall.exe",
            "UnrealWindow",
            "Nightfall - Main Menu",
        )
    }

    #[test]
    fn an_exe_pattern_matches_the_full_path_or_the_base_name() {
        assert!(matches(&rule("exe:nightfall.exe", "G", true), &game()));
        assert!(matches(
            &rule(r"exe:c:\games\nightfall\nightfall.exe", "G", true),
            &game()
        ));
        assert!(!matches(&rule("exe:other.exe", "G", true), &game()));
    }

    #[test]
    fn class_and_title_patterns_are_case_insensitive() {
        assert!(matches(&rule("class:unrealwindow", "G", true), &game()));
        assert!(matches(&rule("title:main menu", "G", true), &game()));
        assert!(matches(&rule("Title:NIGHTFALL*", "G", true), &game()));
        assert!(!matches(&rule("title:settings", "G", true), &game()));
    }

    #[test]
    fn a_bare_pattern_checks_all_three_fields() {
        assert!(matches(&rule("nightfall", "G", true), &game()));
        assert!(matches(&rule("unreal", "G", true), &game()));
        assert!(matches(&rule("main", "G", true), &game()));
        assert!(!matches(&rule("discord", "G", true), &game()));
    }

    #[test]
    fn the_first_enabled_matching_rule_wins() {
        let rules = vec![
            rule("exe:nightfall.exe", "Disabled", false),
            rule("title:nightfall", "First", true),
            rule("unrealwindow", "Second", true),
        ];

        let selection = select(&rules, Some(&game()), None).expect("a match");

        assert_eq!(selection.preset, "First");
        assert_eq!(selection.source, "rule");
        assert_eq!(selection.rule, Some(1));
    }

    #[test]
    fn a_disabled_rule_is_skipped_and_the_next_one_wins() {
        let rules = vec![
            rule("exe:nightfall.exe", "Off", false),
            rule("exe:nightfall.exe", "On", true),
        ];

        assert_eq!(
            select(&rules, Some(&game()), None).map(|selection| selection.preset),
            Some("On".to_owned())
        );
    }

    #[test]
    fn a_manual_preset_wins_over_every_rule() {
        let rules = vec![rule("exe:nightfall.exe", "Rule", true)];

        let selection = select(&rules, Some(&game()), Some("Pinned")).expect("manual");

        assert_eq!(selection.preset, "Pinned");
        assert_eq!(selection.source, "manual");
        assert_eq!(selection.rule, None);
    }

    #[test]
    fn no_match_or_no_foreground_is_none() {
        let rules = vec![rule("exe:nightfall.exe", "G", true)];

        assert_eq!(select(&rules, None, None), None);
        assert_eq!(select(&rules, Some(&Foreground::default()), None), None);
        assert_eq!(
            select(
                &rules,
                Some(&Foreground::new("other.exe", "Other", "Other")),
                None
            ),
            None
        );
        assert_eq!(select(&[], Some(&game()), None), None);
    }

    #[test]
    fn wildcards_anchor_where_they_are_written() {
        assert!(glob("night*.exe", "nightfall.exe"));
        assert!(glob("*.exe", "nightfall.exe"));
        assert!(glob("night*", "nightfall"));
        assert!(glob("*fall*", "nightfall"));
        assert!(!glob("night*.exe", "midnight.exe"));
        assert!(!glob("*.txt", "nightfall.exe"));
        assert!(
            glob("night", "midnight"),
            "a pattern without * is a substring, not an anchored prefix"
        );
    }

    #[test]
    fn an_empty_or_blank_rule_is_ignored() {
        let rules = vec![rule("   ", "G", true), rule("exe:", "G", true)];

        assert_eq!(select(&rules, Some(&game()), None), None);
    }
}
