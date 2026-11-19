//! The Windhawk mod catalog: GitHub's listing of `mods/*.wh.cpp`, with a six-hour cache.
//!
//! The catalog is the 946-file `mods/` folder of `ramensoftware/windhawk-mods`. There is no
//! `mods.json` in that repository, so the GitHub contents API is the listing: one object per file
//! with `name` and `download_url`. The cache stores only the catalog, never the `installed` flag,
//! so a mod installed after the snapshot still shows as installed on the next read.
//!
//! A fetch failure is not fatal while a snapshot exists: a stale catalog is better than an empty
//! install page, and [`Catalog::stale`] says the user is looking at one.

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::plan::sanitize_id;

/// Where the GitHub contents listing lives.
pub const CATALOG_URL: &str =
    "https://api.github.com/repos/ramensoftware/windhawk-mods/contents/mods";

/// Raw source base; the install URL is this plus `<id>.wh.cpp`.
pub const RAW_BASE: &str =
    "https://raw.githubusercontent.com/ramensoftware/windhawk-mods/main/mods";

/// How long a snapshot is served before the network is asked again.
pub const CACHE_TTL: Duration = Duration::from_secs(6 * 60 * 60);

/// One catalog row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogEntry {
    /// Mod id, the file stem.
    pub id: String,
    /// Human-readable name derived from the id.
    pub name: String,
    /// Raw `.wh.cpp` download URL.
    pub url: String,
    /// Whether a source file or a configuration key exists on this machine.
    pub installed: bool,
}

/// A catalog plus the age of the snapshot behind it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    /// Unix seconds at which the snapshot was fetched.
    pub fetched_at: u64,
    /// Where it came from, `github` or `cache`.
    pub source: String,
    /// Whether a network refresh failed and this is the previous snapshot.
    pub stale: bool,
    /// Rows, in the order GitHub returned them.
    pub entries: Vec<CatalogEntry>,
}

impl Catalog {
    /// The rows matching `query`, capped at `limit`. An empty query matches everything.
    pub fn search(&self, query: &str, limit: usize) -> Vec<CatalogEntry> {
        let query = query.trim().to_ascii_lowercase();
        self.entries
            .iter()
            .filter(|entry| {
                query.is_empty()
                    || entry.id.to_ascii_lowercase().contains(&query)
                    || entry.name.to_ascii_lowercase().contains(&query)
            })
            .take(limit)
            .cloned()
            .collect()
    }

    /// The number of rows.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the catalog is empty.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// The network one call needs, injectable so tests never touch it.
pub trait Fetcher: std::fmt::Debug {
    /// Fetches `url` as bytes.
    fn get(&self, url: &str) -> Result<Vec<u8>, String>;
}

/// Everything that can go wrong on the way to a catalog.
#[derive(Debug, thiserror::Error)]
pub enum CatalogError {
    /// The fetch failed and no snapshot was available.
    #[error("cannot download the Windhawk catalog: {0}")]
    Http(String),

    /// The answer was not the listing this module understands.
    #[error("cannot parse the Windhawk catalog: {0}")]
    Parse(String),

    /// The cache could not be read or written.
    #[error("cannot use the catalog cache '{}': {source}", path.display())]
    Io {
        /// Cache file.
        path: std::path::PathBuf,
        /// The underlying error.
        #[source]
        source: std::io::Error,
    },
}

/// A cache row; no `installed`, because that is a property of this machine, not the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Record {
    id: String,
    name: String,
    url: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    fetched_at: u64,
    source: String,
    entries: Vec<Record>,
}

/// Loads the catalog, from the cache when it is younger than [`CACHE_TTL`], else from GitHub.
///
/// `installed` is asked about each id as the catalog is materialised, so a cached snapshot still
/// answers this machine's state.
///
/// # Errors
///
/// [`CatalogError`] only when there is neither a fresh snapshot nor a usable stale one.
pub fn load(
    fetcher: &dyn Fetcher,
    cache_path: &Path,
    now: u64,
    installed: &dyn Fn(&str) -> bool,
) -> Result<Catalog, CatalogError> {
    let cached = read_snapshot(cache_path);

    if let Some(snapshot) = &cached {
        if now.saturating_sub(snapshot.fetched_at) < CACHE_TTL.as_secs() {
            return Ok(materialise(snapshot, false, installed));
        }
    }

    match fetch(fetcher, now) {
        Ok(snapshot) => {
            // A cache that cannot be written is not a reason to refuse a working catalog.
            let _ = write_snapshot(cache_path, &snapshot);
            Ok(materialise(&snapshot, false, installed))
        }
        Err(error) => match cached {
            Some(snapshot) => Ok(materialise(&snapshot, true, installed)),
            None => Err(error),
        },
    }
}

/// Fetches and parses the listing without consulting the cache.
///
/// # Errors
///
/// [`CatalogError::Http`] or [`CatalogError::Parse`].
fn fetch(fetcher: &dyn Fetcher, now: u64) -> Result<Snapshot, CatalogError> {
    let body = fetcher.get(CATALOG_URL).map_err(CatalogError::Http)?;
    let records = parse_github(&body)?;

    Ok(Snapshot {
        fetched_at: now,
        source: "github".to_owned(),
        entries: records,
    })
}

/// Parses the GitHub contents array.
///
/// # Errors
///
/// [`CatalogError::Parse`] when the body is not a JSON array of objects with a `name`.
fn parse_github(body: &[u8]) -> Result<Vec<Record>, CatalogError> {
    let value: serde_json::Value =
        serde_json::from_slice(body).map_err(|error| CatalogError::Parse(error.to_string()))?;
    let array = value
        .as_array()
        .ok_or_else(|| CatalogError::Parse("the listing is not an array".to_owned()))?;

    let mut records = Vec::with_capacity(array.len());
    let mut seen = BTreeSet::new();

    for entry in array {
        let Some(name) = entry.get("name").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let Some(stem) = name.strip_suffix(".wh.cpp") else {
            continue;
        };
        let Ok(id) = sanitize_id(stem) else {
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }

        let url = entry
            .get("download_url")
            .and_then(serde_json::Value::as_str)
            .filter(|url| !url.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| install_url(&id));

        records.push(Record {
            name: prettify(&id),
            id,
            url,
        });
    }

    if records.is_empty() {
        return Err(CatalogError::Parse(
            "the listing contains no .wh.cpp files".to_owned(),
        ));
    }

    Ok(records)
}

/// The raw download URL of one mod.
pub fn install_url(id: &str) -> String {
    format!("{RAW_BASE}/{id}.wh.cpp")
}

/// `accent-color-sync` → `Accent Color Sync`.
///
/// A handful of tokens have a conventional spelling (`macOS`, `Windows 11`, `UI`); everything else
/// is capitalised word by word. The id itself is never changed.
pub fn prettify(id: &str) -> String {
    let mut words = Vec::new();

    for word in id.split('-') {
        let prettified = match word {
            "macos" => "macOS".to_owned(),
            "w11" => "Windows 11".to_owned(),
            "w10" => "Windows 10".to_owned(),
            "ui" => "UI".to_owned(),
            "ide" => "IDE".to_owned(),
            "dwm" => "DWM".to_owned(),
            "uxtheme" => "UXTheme".to_owned(),
            other => {
                let mut characters = other.chars();
                match characters.next() {
                    Some(first) => first.to_uppercase().chain(characters).collect(),
                    None => String::new(),
                }
            }
        };
        words.push(prettified);
    }

    words.join(" ")
}

/// The current Unix time in seconds, or zero when the clock is before the epoch.
pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn read_snapshot(path: &Path) -> Option<Snapshot> {
    let bytes = fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn write_snapshot(path: &Path, snapshot: &Snapshot) -> Result<(), CatalogError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| CatalogError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }

    let bytes = serde_json::to_vec_pretty(snapshot)
        .map_err(|error| CatalogError::Parse(error.to_string()))?;
    let temporary = path.with_extension("json.tmp");

    fs::write(&temporary, bytes).map_err(|source| CatalogError::Io {
        path: temporary.clone(),
        source,
    })?;

    // Windows refuses to rename over an existing file; the snapshot is ours, so replacing it is
    // safe even if another read sees the moment in between.
    let _ = fs::remove_file(path);
    fs::rename(&temporary, path).map_err(|source| CatalogError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn materialise(snapshot: &Snapshot, stale: bool, installed: &dyn Fn(&str) -> bool) -> Catalog {
    Catalog {
        fetched_at: snapshot.fetched_at,
        source: snapshot.source.clone(),
        stale,
        entries: snapshot
            .entries
            .iter()
            .map(|record| CatalogEntry {
                id: record.id.clone(),
                name: record.name.clone(),
                url: record.url.clone(),
                installed: installed(&record.id),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    use std::path::PathBuf;
    use tempfile::tempdir;

    const FIXTURE: &str = r#"[
        {"name":"accent-color-sync.wh.cpp","download_url":"https://raw/accent-color-sync.wh.cpp"},
        {"name":"windows-11-taskbar-styler.wh.cpp","download_url":"https://raw/taskbar.wh.cpp"},
        {"name":"README.md","download_url":"https://raw/README.md"},
        {"name":"../evil.wh.cpp","download_url":"https://raw/evil.wh.cpp"},
        {"name":"no-url.wh.cpp","download_url":null}
    ]"#;

    #[derive(Debug, Default)]
    struct FakeFetcher {
        body: String,
        calls: Cell<usize>,
        fail: bool,
    }

    impl Fetcher for FakeFetcher {
        fn get(&self, _url: &str) -> Result<Vec<u8>, String> {
            self.calls.set(self.calls.get() + 1);
            if self.fail {
                return Err("offline".to_owned());
            }
            Ok(self.body.clone().into_bytes())
        }
    }

    fn nothing_installed(_: &str) -> bool {
        false
    }

    #[test]
    fn the_listing_keeps_only_wh_cpp_files_and_builds_the_missing_url() {
        let records = parse_github(FIXTURE.as_bytes()).expect("parse");

        assert_eq!(records.len(), 3, "{records:?}");
        assert_eq!(records[0].id, "accent-color-sync");
        assert_eq!(records[1].id, "windows-11-taskbar-styler");
        assert_eq!(records[2].id, "no-url");
        assert_eq!(records[2].url, install_url("no-url"));
    }

    #[test]
    fn names_are_prettified_from_the_file_name() {
        assert_eq!(prettify("accent-color-sync"), "Accent Color Sync");
        assert_eq!(
            prettify("windows-11-taskbar-styler"),
            "Windows 11 Taskbar Styler"
        );
        assert_eq!(
            prettify("macos-minimize-animation"),
            "macOS Minimize Animation"
        );
        assert_eq!(prettify("uxtheme-hook"), "UXTheme Hook");
    }

    #[test]
    fn a_listing_that_is_not_json_is_a_parse_error() {
        assert!(matches!(
            parse_github(b"<html>rate limited</html>"),
            Err(CatalogError::Parse(_))
        ));
        assert!(matches!(
            parse_github(br#"{"message":"rate limited"}"#),
            Err(CatalogError::Parse(_))
        ));
        assert!(matches!(
            parse_github(br#"[]"#),
            Err(CatalogError::Parse(_))
        ));
    }

    #[test]
    fn search_matches_id_and_name_case_insensitively() {
        let catalog = Catalog {
            fetched_at: 0,
            source: "test".to_owned(),
            stale: false,
            entries: vec![
                entry("accent-color-sync", "Accent Color Sync"),
                entry("windows-11-taskbar-styler", "Windows 11 Taskbar Styler"),
            ],
        };

        assert_eq!(catalog.search("taskbar", 10).len(), 1);
        assert_eq!(catalog.search("TASKBAR", 10).len(), 1);
        assert_eq!(catalog.search("accent", 10)[0].id, "accent-color-sync");
        assert_eq!(catalog.search("", 1).len(), 1);
        assert!(catalog.search("nothing", 10).is_empty());
    }

    fn entry(id: &str, name: &str) -> CatalogEntry {
        CatalogEntry {
            id: id.to_owned(),
            name: name.to_owned(),
            url: install_url(id),
            installed: false,
        }
    }

    #[test]
    fn a_fresh_snapshot_is_served_without_a_network_call() {
        let directory = tempdir().expect("temp dir");
        let cache = directory.path().join("cache").join("catalog.json");
        let fetcher = FakeFetcher {
            body: FIXTURE.to_owned(),
            ..FakeFetcher::default()
        };

        let first = load(&fetcher, &cache, 1_000, &nothing_installed).expect("first");
        assert_eq!(first.len(), 3);
        assert_eq!(first.source, "github");
        assert!(!first.stale);

        let second = load(&fetcher, &cache, 1_000 + 60, &nothing_installed).expect("second");
        assert_eq!(second.len(), 3);
        assert_eq!(fetcher.calls.get(), 1, "the cache answered the second read");
        assert!(cache.is_file());
    }

    #[test]
    fn the_cache_expires_after_six_hours() {
        let directory = tempdir().expect("temp dir");
        let cache = directory.path().join("catalog.json");
        let fetcher = FakeFetcher {
            body: FIXTURE.to_owned(),
            ..FakeFetcher::default()
        };

        load(&fetcher, &cache, 1_000, &nothing_installed).expect("first");
        load(
            &fetcher,
            &cache,
            1_000 + CACHE_TTL.as_secs(),
            &nothing_installed,
        )
        .expect("expired");

        assert_eq!(fetcher.calls.get(), 2);
    }

    #[test]
    fn a_failed_refresh_serves_the_stale_snapshot() {
        let directory = tempdir().expect("temp dir");
        let cache = directory.path().join("catalog.json");
        let fetcher = FakeFetcher {
            body: FIXTURE.to_owned(),
            ..FakeFetcher::default()
        };
        load(&fetcher, &cache, 1_000, &nothing_installed).expect("first");

        let offline = FakeFetcher {
            body: String::new(),
            fail: true,
            ..FakeFetcher::default()
        };
        let stale = load(
            &offline,
            &cache,
            1_000 + CACHE_TTL.as_secs() + 1,
            &nothing_installed,
        )
        .expect("stale");

        assert!(stale.stale);
        assert_eq!(stale.len(), 3);
    }

    #[test]
    fn without_a_cache_a_failed_fetch_is_an_error() {
        let directory = tempdir().expect("temp dir");
        let cache = directory.path().join("catalog.json");
        let offline = FakeFetcher {
            fail: true,
            ..FakeFetcher::default()
        };

        assert!(matches!(
            load(&offline, &cache, 1_000, &nothing_installed),
            Err(CatalogError::Http(_))
        ));
    }

    #[test]
    fn installed_is_computed_when_the_catalog_is_materialised() {
        let directory = tempdir().expect("temp dir");
        let cache = directory.path().join("catalog.json");
        let fetcher = FakeFetcher {
            body: FIXTURE.to_owned(),
            ..FakeFetcher::default()
        };

        let catalog =
            load(&fetcher, &cache, 1_000, &|id| id == "accent-color-sync").expect("catalog");
        assert!(catalog.entries[0].installed);

        // The snapshot on disk has no `installed` field; the next read recomputes it.
        let text = fs::read_to_string(&cache).expect("cache");
        assert!(!text.contains("installed"), "{text}");

        let catalog = load(&fetcher, &cache, 1_001, &|id| id == "no-url").expect("cached");
        assert!(!catalog.entries[0].installed);
        assert!(catalog.entries[2].installed);
    }

    #[test]
    fn the_cache_lives_where_the_layout_says() {
        let directory = tempdir().expect("temp dir");
        let paths = crate::paths::Paths::at(directory.path());

        assert_eq!(
            paths.catalog_file,
            PathBuf::from(directory.path())
                .join("cache")
                .join("windhawk-catalog.json")
        );
    }
}
