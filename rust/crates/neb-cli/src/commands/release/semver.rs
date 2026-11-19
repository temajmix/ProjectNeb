//! The one version shape a release may use: `MAJOR.MINOR.PATCH`.
//!
//! Prerelease and build metadata are deliberately out of scope: the update manifest compares plain
//! versions and the release tag is `v<version>`, so accepting `0.2.0-rc.1` here would only move the
//! ambiguity into the manifest. Leading zeros are refused, as SemVer 2.0.0 refuses them.

use std::fmt;

use crate::commands::CliError;

/// A semantic version, ordered by major, then minor, then patch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    major: u64,
    minor: u64,
    patch: u64,
}

impl Version {
    /// Parses `MAJOR.MINOR.PATCH`.
    ///
    /// # Errors
    ///
    /// [`CliError::Failed`] when the text has another shape or a component is not a canonical
    /// decimal number.
    pub fn parse(text: &str) -> Result<Self, CliError> {
        let text = text.trim();
        let mut parts = text.split('.');
        let (Some(major), Some(minor), Some(patch), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(invalid(text));
        };

        Ok(Self {
            major: number(major).ok_or_else(|| invalid(text))?,
            minor: number(minor).ok_or_else(|| invalid(text))?,
            patch: number(patch).ok_or_else(|| invalid(text))?,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Builds the shared "this is not a version" error.
fn invalid(text: &str) -> CliError {
    CliError::Failed(format!("version '{text}' is not MAJOR.MINOR.PATCH"))
}

/// Reads one component: ASCII digits only, and no leading zero unless the component is `0`.
fn number(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }

    if text.len() > 1 && text.starts_with('0') {
        return None;
    }

    text.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_version_round_trips() {
        let version = Version::parse("0.1.1").expect("parse");

        assert_eq!(version.to_string(), "0.1.1");
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(
            Version::parse("  1.20.300  ").expect("parse").to_string(),
            "1.20.300"
        );
    }

    #[test]
    fn malformed_versions_are_refused() {
        for text in [
            "",
            "1",
            "1.2",
            "1.2.3.4",
            "v1.2.3",
            "1.2.x",
            "1..3",
            "01.2.3",
            "1.02.3",
            "+1.2.3",
            "1.2.3-rc.1",
            " 1.2.3 extra",
        ] {
            assert!(
                Version::parse(text).is_err(),
                "'{text}' must not parse as a version"
            );
        }
    }

    #[test]
    fn comparison_is_numeric_by_component() {
        assert!(Version::parse("0.1.1").unwrap() > Version::parse("0.1.0").unwrap());
        assert!(Version::parse("0.2.0").unwrap() > Version::parse("0.1.9").unwrap());
        assert!(Version::parse("1.0.0").unwrap() > Version::parse("0.9.9").unwrap());
        assert!(Version::parse("10.0.0").unwrap() > Version::parse("9.9.9").unwrap());
        assert_eq!(
            Version::parse("1.2.3").unwrap(),
            Version::parse("1.2.3").unwrap()
        );
    }
}
