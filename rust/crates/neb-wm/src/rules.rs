//! Per-application rules: compile the configuration's `windowManager.rules` once, then answer a
//! decision per window in configuration order.
//!
//! `match` is a regular expression tested against the executable path, the window class and the
//! title, case-insensitively. A window matches when the expression matches any one of the three,
//! which is what makes `"match": "steam"` cover `steam.exe`, the `SDL_app` class and a window
//! titled after a game. The first rule that matches decides; later rules are not consulted, so a
//! specific rule must be written before a broader one.
//!
//! A rule whose expression does not compile is skipped with a warning instead of making the whole
//! configuration unusable — a typo in one rule must not stop the manager from tiling.

use neb_core::config::WindowRule;
use regex::{Regex, RegexBuilder};

/// What the manager knows about a window when it decides whether to manage it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowFacts {
    /// Full executable path, or the process name when the path could not be read.
    pub exe: String,
    /// Window class.
    pub class: String,
    /// Window title.
    pub title: String,
}

impl WindowFacts {
    /// Builds facts from the three strings the rule language tests.
    pub fn new(exe: impl Into<String>, class: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            exe: exe.into(),
            class: class.into(),
            title: title.into(),
        }
    }

    /// The executable's file name, which is what rules usually name (`notepad.exe`).
    pub fn exe_name(&self) -> &str {
        self.exe
            .rsplit(['\\', '/'])
            .next()
            .filter(|name| !name.is_empty())
            .unwrap_or(&self.exe)
    }
}

/// The overrides one matching rule asks for.
///
/// Every field but [`ignore`](Self::ignore) is optional: `None` means "keep the manager default",
/// which is what lets a rule change only the workspace without also deciding the float state.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RuleDecision {
    /// Leave the window completely alone.
    pub ignore: bool,
    /// Tile it or float it.
    pub float: Option<bool>,
    /// Workspace the window belongs to.
    pub workspace: Option<usize>,
    /// Keep the system decorations instead of the manager's accent border.
    pub no_border: Option<bool>,
}

impl RuleDecision {
    /// The decision a rule carries, with defaults where the rule said nothing.
    fn from_rule(rule: &WindowRule) -> Self {
        Self {
            ignore: rule.ignore.unwrap_or(false),
            float: rule.float,
            workspace: rule.workspace,
            no_border: rule.no_border,
        }
    }
}

/// One rule that compiled, ready to be asked about a window.
#[derive(Debug, Clone)]
struct CompiledRule {
    pattern: Regex,
    decision: RuleDecision,
}

/// A rule that could not be compiled, reported once at startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleWarning {
    /// Position in `windowManager.rules`, zero-based.
    pub index: usize,
    /// The expression as written.
    pub pattern: String,
    /// Why it was skipped.
    pub message: String,
}

impl std::fmt::Display for RuleWarning {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "windowManager.rules[{}] '{}' skipped: {}",
            self.index, self.pattern, self.message
        )
    }
}

/// The compiled rule list.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    rules: Vec<CompiledRule>,
}

impl RuleSet {
    /// Compiles `rules`, returning the usable set and one warning per skipped rule.
    ///
    /// An empty expression is a catch-all: `Regex::new("")` matches every subject, which is the
    /// natural reading of a rule with `"match": ""` and is still bounded by the first-match rule.
    pub fn compile(rules: &[WindowRule]) -> (Self, Vec<RuleWarning>) {
        let mut compiled = Vec::with_capacity(rules.len());
        let mut warnings = Vec::new();

        for (index, rule) in rules.iter().enumerate() {
            let pattern = rule.pattern.as_deref().unwrap_or("");
            match RegexBuilder::new(pattern).case_insensitive(true).build() {
                Ok(regex) => compiled.push(CompiledRule {
                    pattern: regex,
                    decision: RuleDecision::from_rule(rule),
                }),
                Err(error) => warnings.push(RuleWarning {
                    index,
                    pattern: pattern.to_owned(),
                    message: error.to_string(),
                }),
            }
        }

        (Self { rules: compiled }, warnings)
    }

    /// Whether any rule was compiled.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// How many usable rules the set holds.
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    /// The decision of the first rule that matches `facts`, or `None` when nothing matches.
    pub fn decide(&self, facts: &WindowFacts) -> Option<RuleDecision> {
        self.rule_index(facts)
            .map(|index| self.rules[index].decision)
    }

    /// The index of the first matching rule, kept separate because the manager logs it.
    pub fn rule_index(&self, facts: &WindowFacts) -> Option<usize> {
        self.rules.iter().position(|rule| {
            rule.pattern.is_match(&facts.exe)
                || rule.pattern.is_match(facts.exe_name())
                || rule.pattern.is_match(&facts.class)
                || rule.pattern.is_match(&facts.title)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(pattern: &str) -> WindowRule {
        WindowRule {
            pattern: Some(pattern.to_owned()),
            ..WindowRule::default()
        }
    }

    fn facts(exe: &str, class: &str, title: &str) -> WindowFacts {
        WindowFacts::new(exe, class, title)
    }

    #[test]
    fn an_executable_path_matches_case_insensitively() {
        let (rules, warnings) = RuleSet::compile(&[WindowRule {
            float: Some(true),
            ..rule(r"notepad\.exe")
        }]);

        assert!(warnings.is_empty());
        assert_eq!(
            rules.decide(&facts(r"C:\Windows\System32\NOTEPAD.EXE", "Notepad", "x")),
            Some(RuleDecision {
                float: Some(true),
                ..RuleDecision::default()
            })
        );
        assert_eq!(
            rules.decide(&facts(
                r"C:\Windows\System32\mspaint.exe",
                "MSPaintApp",
                "x"
            )),
            None
        );
    }

    #[test]
    fn a_class_or_a_title_can_match_as_well_as_the_executable() {
        let (rules, _) = RuleSet::compile(&[WindowRule {
            ignore: Some(true),
            ..rule("Progman")
        }]);

        assert_eq!(
            rules.decide(&facts("explorer.exe", "Progman", "Program Manager")),
            Some(RuleDecision {
                ignore: true,
                ..RuleDecision::default()
            })
        );

        let (by_title, _) = RuleSet::compile(&[WindowRule {
            workspace: Some(3),
            ..rule("^Steam$")
        }]);
        assert_eq!(
            by_title
                .decide(&facts("steam.exe", "SDL_app", "Steam"))
                .and_then(|decision| decision.workspace),
            Some(3)
        );
    }

    #[test]
    fn the_first_matching_rule_wins() {
        let (rules, _) = RuleSet::compile(&[
            WindowRule {
                float: Some(true),
                workspace: Some(1),
                ..rule("notepad")
            },
            WindowRule {
                float: Some(false),
                workspace: Some(2),
                ..rule(".*")
            },
        ]);

        let decision = rules
            .decide(&facts("notepad.exe", "Notepad", "Untitled - Notepad"))
            .expect("the first rule matches");

        assert_eq!(decision.float, Some(true));
        assert_eq!(decision.workspace, Some(1));
        assert_eq!(
            rules.rule_index(&facts("notepad.exe", "Notepad", "")),
            Some(0)
        );
    }

    #[test]
    fn a_later_rule_answers_when_the_first_does_not_match() {
        let (rules, _) = RuleSet::compile(&[
            WindowRule {
                float: Some(true),
                ..rule("steam")
            },
            WindowRule {
                workspace: Some(2),
                ..rule("discord")
            },
        ]);

        assert_eq!(
            rules.decide(&facts("discord.exe", "Chrome_WidgetWin_1", "Discord")),
            Some(RuleDecision {
                workspace: Some(2),
                ..RuleDecision::default()
            })
        );
    }

    #[test]
    fn a_rule_without_a_match_key_is_a_catch_all() {
        let (rules, _) = RuleSet::compile(&[WindowRule {
            float: Some(true),
            ..WindowRule::default()
        }]);

        assert_eq!(
            rules.decide(&facts("anything.exe", "AnyClass", "Any title")),
            Some(RuleDecision {
                float: Some(true),
                ..RuleDecision::default()
            })
        );
    }

    #[test]
    fn a_rule_that_changes_one_thing_leaves_the_rest_to_the_manager() {
        let (rules, _) = RuleSet::compile(&[WindowRule {
            workspace: Some(2),
            ..rule("notepad")
        }]);

        let decision = rules
            .decide(&facts("notepad.exe", "Notepad", ""))
            .expect("matches");

        assert_eq!(decision.workspace, Some(2));
        assert_eq!(decision.float, None);
        assert_eq!(decision.no_border, None);
        assert!(!decision.ignore);
    }

    #[test]
    fn an_invalid_expression_is_skipped_with_a_warning() {
        let (rules, warnings) = RuleSet::compile(&[
            rule("(unclosed"),
            WindowRule {
                float: Some(true),
                ..rule("notepad")
            },
        ]);

        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].index, 0);
        assert_eq!(warnings[0].pattern, "(unclosed");
        assert_eq!(rules.len(), 1, "the valid rule still works");
        assert_eq!(
            rules
                .decide(&facts("notepad.exe", "Notepad", ""))
                .and_then(|decision| decision.float),
            Some(true)
        );
    }

    #[test]
    fn an_empty_rule_list_never_matches_and_is_empty() {
        let (rules, warnings) = RuleSet::compile(&[]);

        assert!(warnings.is_empty());
        assert!(rules.is_empty());
        assert_eq!(rules.len(), 0);
        assert_eq!(rules.decide(&facts("x.exe", "X", "X")), None);
    }

    #[test]
    fn the_executable_name_is_the_last_path_component() {
        assert_eq!(
            facts(r"C:\Program Files\App\app.exe", "", "").exe_name(),
            "app.exe"
        );
        assert_eq!(facts("app.exe", "", "").exe_name(), "app.exe");
        assert_eq!(facts("", "", "").exe_name(), "");
    }
}
