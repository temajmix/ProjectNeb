//! The documents a release publishes: the GitHub notes, `latest.json`, and the changelog section.

use serde_json::{Map, Value, json};

use crate::commands::CliError;
use crate::commands::release::semver::Version;

/// Where the changelog lives once the manifest repository is pushed.
pub const CHANGELOG_URL: &str = "https://github.com/temajmix/ProjectNeb/blob/main/CHANGELOG.md";

/// One downloadable asset and the hash of its bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Asset {
    /// File name inside the release, e.g. `ProjectNeb-0.1.1-beta-portable.zip`.
    pub file: String,
    /// SHA-256 of the file, uppercase hexadecimal.
    pub sha256: String,
}

/// The URL an asset is served from for `version`.
pub fn asset_url(version: &Version, file: &str) -> String {
    format!("https://github.com/temajmix/ProjectNeb/releases/download/v{version}/{file}")
}

/// Builds the body of the GitHub release: the user's notes plus a link to the changelog.
pub fn release_notes(notes: &str) -> String {
    format!("{}\n\nChangelog: {CHANGELOG_URL}\n", notes.trim())
}

/// Builds the `latest.json` document the application polls.
///
/// `setup` is absent when the release was made with `--skip-installer`; the portable archive is
/// always present.
///
/// # Errors
///
/// [`CliError::Failed`] if the document cannot be serialised.
pub fn render_manifest(
    version: &Version,
    channel: &str,
    released: &str,
    notes: &str,
    portable: &Asset,
    setup: Option<&Asset>,
) -> Result<String, CliError> {
    let mut assets = Map::new();
    assets.insert("portable".to_owned(), asset_json(version, portable));

    if let Some(setup) = setup {
        assets.insert("setup".to_owned(), asset_json(version, setup));
    }

    let document = json!({
        "version": version.to_string(),
        "channel": channel,
        "released": released,
        "notes": notes.trim(),
        "changelogUrl": CHANGELOG_URL,
        "assets": assets,
    });

    let mut text = serde_json::to_string_pretty(&document)
        .map_err(|error| CliError::Failed(format!("cannot render latest.json: {error}")))?;
    text.push('\n');
    Ok(text)
}

/// Renders one asset entry.
fn asset_json(version: &Version, asset: &Asset) -> Value {
    json!({
        "url": asset_url(version, &asset.file),
        "sha256": asset.sha256,
    })
}

/// Builds the changelog heading for a release.
pub fn changelog_section(version: &Version, date: &str, notes: &str) -> String {
    format!("## [{version}] — {date}\n\n{}\n", notes.trim())
}

/// Inserts `section` before the newest existing release section.
///
/// The introductory text at the top of `CHANGELOG.md` stays where it is; with no section yet the new
/// one is simply appended.
pub fn prepend_changelog(existing: &str, section: &str) -> String {
    let section = section.trim_end();

    if existing.trim().is_empty() {
        return format!("{section}\n");
    }

    if let Some(index) = existing.find("\n## [") {
        let split = index + 1;
        format!(
            "{}\n\n{section}\n\n{}",
            existing[..split].trim_end(),
            &existing[split..]
        )
    } else if existing.starts_with("## [") {
        format!("{section}\n\n{existing}")
    } else {
        format!("{}\n\n{section}\n", existing.trim_end())
    }
}

/// Formats a Unix timestamp as a UTC `YYYY-MM-DD` date.
///
/// The civil-from-days conversion is Howard Hinnant's `civil_from_days`, which keeps the command free
/// of a date crate.
pub fn utc_date(unix_seconds: u64) -> String {
    let days = i64::try_from(unix_seconds / 86_400).unwrap_or(i64::MAX);
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    let year = if month <= 2 { year + 1 } else { year };

    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn version(text: &str) -> Version {
        Version::parse(text).expect("version")
    }

    fn asset(file: &str, hash: &str) -> Asset {
        Asset {
            file: file.to_owned(),
            sha256: hash.to_owned(),
        }
    }

    #[test]
    fn asset_urls_point_at_the_version_tag() {
        assert_eq!(
            asset_url(&version("0.1.1"), "ProjectNeb-0.1.1-beta-portable.zip"),
            "https://github.com/temajmix/ProjectNeb/releases/download/v0.1.1/ProjectNeb-0.1.1-beta-portable.zip"
        );
    }

    #[test]
    fn the_manifest_carries_both_assets_with_urls_and_hashes() {
        let text = render_manifest(
            &version("0.1.1"),
            "beta",
            "2026-11-17",
            "Коротко",
            &asset("ProjectNeb-0.1.1-beta-portable.zip", "AABB"),
            Some(&asset("ProjectNeb-0.1.1-beta-setup.exe", "CCDD")),
        )
        .expect("render");

        assert!(!text.contains("\\u"), "cyrillic notes must stay readable");
        let document: Value = serde_json::from_str(&text).expect("json");

        assert_eq!(document["version"], "0.1.1");
        assert_eq!(document["channel"], "beta");
        assert_eq!(document["released"], "2026-11-17");
        assert_eq!(document["notes"], "Коротко");
        assert_eq!(document["changelogUrl"], CHANGELOG_URL);
        assert_eq!(
            document["assets"]["portable"]["url"],
            "https://github.com/temajmix/ProjectNeb/releases/download/v0.1.1/ProjectNeb-0.1.1-beta-portable.zip"
        );
        assert_eq!(document["assets"]["portable"]["sha256"], "AABB");
        assert_eq!(
            document["assets"]["setup"]["url"],
            "https://github.com/temajmix/ProjectNeb/releases/download/v0.1.1/ProjectNeb-0.1.1-beta-setup.exe"
        );
        assert_eq!(document["assets"]["setup"]["sha256"], "CCDD");
        assert!(
            text.find("\"portable\"").unwrap() < text.find("\"setup\"").unwrap(),
            "the portable asset is listed first"
        );
        assert!(text.ends_with('\n'));
    }

    #[test]
    fn a_skipped_installer_leaves_the_setup_asset_out() {
        let text = render_manifest(
            &version("0.1.1"),
            "beta",
            "2026-11-17",
            "notes",
            &asset("ProjectNeb-0.1.1-beta-portable.zip", "AA"),
            None,
        )
        .expect("render");

        let document: Value = serde_json::from_str(&text).expect("json");
        assert!(document["assets"].get("setup").is_none());
        assert!(document["assets"].get("portable").is_some());
    }

    #[test]
    fn release_notes_append_the_changelog_link() {
        let notes = release_notes("  Новый док\n");

        assert!(notes.starts_with("Новый док\n\n"));
        assert!(notes.contains(CHANGELOG_URL));
        assert_eq!(notes.matches(CHANGELOG_URL).count(), 1);
    }

    #[test]
    fn the_changelog_section_has_the_documented_heading() {
        assert_eq!(
            changelog_section(&version("0.1.1"), "2026-11-17", "Коротко\n"),
            "## [0.1.1] — 2026-11-17\n\nКоротко\n"
        );
    }

    #[test]
    fn a_new_section_goes_above_the_previous_one_and_keeps_the_intro() {
        let existing = "# Changelog\n\nВсё о ProjectNeb.\n\n## [0.1.0] - 2026-11-17\n\nСтарое.\n";
        let updated = prepend_changelog(
            existing,
            &changelog_section(&version("0.1.1"), "2026-11-18", "Новое."),
        );

        assert_eq!(
            updated,
            "# Changelog\n\nВсё о ProjectNeb.\n\n## [0.1.1] — 2026-11-18\n\nНовое.\n\n## [0.1.0] - 2026-11-17\n\nСтарое.\n"
        );
    }

    #[test]
    fn a_changelog_without_sections_gains_one_at_the_end() {
        let updated = prepend_changelog(
            "# Changelog\n\nВсё о ProjectNeb.\n",
            &changelog_section(&version("0.1.0"), "2026-11-17", "Первое."),
        );

        assert!(updated.starts_with("# Changelog\n\nВсё о ProjectNeb.\n\n## [0.1.0]"));
        assert!(updated.ends_with("Первое.\n"));
    }

    #[test]
    fn an_empty_changelog_starts_with_the_section() {
        assert_eq!(
            prepend_changelog("", &changelog_section(&version("0.1.1"), "2026-11-17", "x")),
            "## [0.1.1] — 2026-11-17\n\nx\n"
        );
    }

    #[test]
    fn dates_come_out_as_utc_calendar_days() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(86_399), "1970-01-01");
        assert_eq!(utc_date(86_400), "1970-01-02");
        assert_eq!(utc_date(1_700_000_000), "2023-11-14");
        assert_eq!(utc_date(1_763_424_000), "2025-11-18");
    }
}
