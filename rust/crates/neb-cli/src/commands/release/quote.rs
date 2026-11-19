//! Rendering command lines for the plan `release` prints.
//!
//! The runner hands arguments to [`std::process::Command`] as separate `OsString`s, so the process
//! boundary is already safe. This module only reproduces the line the way Windows' own
//! `CommandLineToArgvW` would split it back, which is what makes a dry-run plan copy-pastable.

/// Renders one argument, quoting it only when Windows would otherwise split it or lose it.
pub fn argument(text: &str) -> String {
    let needs_quotes = text.is_empty()
        || text
            .chars()
            .any(|character| matches!(character, ' ' | '\t' | '\n' | '\r' | '"'));

    if !needs_quotes {
        return text.to_owned();
    }

    let mut quoted = String::with_capacity(text.len() + 2);
    quoted.push('"');

    // Backslashes are literal unless they sit before a quote: then each one doubles, and the quote
    // itself gains one more. Trailing backslashes before the closing quote double too.
    let mut backslashes = 0usize;

    for character in text.chars() {
        match character {
            '\\' => backslashes += 1,
            '"' => {
                push(&mut quoted, '\\', backslashes * 2 + 1);
                quoted.push('"');
                backslashes = 0;
            }
            other => {
                push(&mut quoted, '\\', backslashes);
                quoted.push(other);
                backslashes = 0;
            }
        }
    }

    push(&mut quoted, '\\', backslashes * 2);
    quoted.push('"');
    quoted
}

/// Renders a program followed by its arguments.
pub fn command(program: &str, arguments: &[String]) -> String {
    let mut line = argument(program);

    for item in arguments {
        line.push(' ');
        line.push_str(&argument(item));
    }

    line
}

/// Appends `count` copies of `character`.
fn push(text: &mut String, character: char, count: usize) {
    for _ in 0..count {
        text.push(character);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_arguments_stay_plain() {
        assert_eq!(argument("cargo"), "cargo");
        assert_eq!(argument("--workspace"), "--workspace");
        assert_eq!(argument(r"A:\ProjectNeb\dist"), r"A:\ProjectNeb\dist");
    }

    #[test]
    fn empty_arguments_become_a_quoted_empty_string() {
        assert_eq!(argument(""), "\"\"");
    }

    #[test]
    fn spaces_and_tabs_are_quoted() {
        assert_eq!(
            argument("C:\\Program Files\\Inno"),
            "\"C:\\Program Files\\Inno\""
        );
        assert_eq!(argument("a\tb"), "\"a\tb\"");
        assert_eq!(argument("two words"), "\"two words\"");
    }

    #[test]
    fn quotes_are_escaped() {
        assert_eq!(argument(r#"say "hi""#), r#""say \"hi\"""#);
    }

    #[test]
    fn trailing_backslashes_are_doubled_before_the_closing_quote() {
        assert_eq!(argument("C:\\a b\\"), "\"C:\\a b\\\\\"");
    }

    #[test]
    fn a_full_line_quotes_only_what_needs_it() {
        let line = command(
            "gh",
            &[
                "release".to_owned(),
                "create".to_owned(),
                "v0.1.1".to_owned(),
                "C:\\Users\\me\\dist\\ProjectNeb setup.exe".to_owned(),
            ],
        );

        assert_eq!(
            line,
            "gh release create v0.1.1 \"C:\\Users\\me\\dist\\ProjectNeb setup.exe\""
        );
    }
}
