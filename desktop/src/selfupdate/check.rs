//! The once-a-day check, with the TUI's rules (SPECS §30): at most one network
//! request per day, the last answer remembered on disk so a restart shows
//! yesterday's finding at once, and every failure silent.
//!
//! The desktop keeps its own cache file, separate from the TUI's
//! `update-check.json`, because the two products have separate release lines
//! (`desktop-v*` against `v*`) and either may be newer than the other.

use std::path::{Path, PathBuf};

use flightdeck::contracts::FileSystem;
use serde_json::{json, Value};

use super::kind::Os;
use super::release::Version;
use super::source::ReleaseSource;

/// Minimum gap between network checks.
pub const CHECK_INTERVAL_SECS: u64 = 24 * 60 * 60;

/// The last completed check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckCache {
    pub last_check_unix: u64,
    /// The newest version seen then (may equal the running one).
    pub latest_version: String,
}

/// Read a cache file's text; anything unreadable is "no cache".
pub fn parse_cache(text: &str) -> Option<CheckCache> {
    let value: Value = serde_json::from_str(text).ok()?;
    Some(CheckCache {
        last_check_unix: value["last_check_unix"].as_u64()?,
        latest_version: value["latest_version"].as_str()?.to_string(),
    })
}

/// The cache file's text.
pub fn render_cache(cache: &CheckCache) -> String {
    json!({
        "last_check_unix": cache.last_check_unix,
        "latest_version": cache.latest_version,
    })
    .to_string()
}

/// Whether a fresh network check is due, and the notice the cache alone
/// already justifies (a newer version seen on the last check that the running
/// one has not caught up to). Pure.
pub fn evaluate(
    cache: Option<&CheckCache>,
    now_unix: u64,
    current: Version,
) -> (bool, Option<Version>) {
    let due =
        cache.is_none_or(|c| now_unix.saturating_sub(c.last_check_unix) >= CHECK_INTERVAL_SECS);
    let cached = cache
        .and_then(|c| Version::parse(&c.latest_version))
        .filter(|latest| *latest > current);
    (due, cached)
}

/// Where the cache lives, from an environment lookup (injected so each OS's
/// rule is testable anywhere).
pub fn cache_path(os: Os, env: &dyn Fn(&str) -> Option<PathBuf>) -> Option<PathBuf> {
    const FILE: &str = "desktop-update-check.json";
    let dir = match os {
        Os::MacOs => env("HOME")?.join("Library/Application Support"),
        Os::Linux => env("XDG_CACHE_HOME").or_else(|| env("HOME").map(|h| h.join(".cache")))?,
        Os::Windows => env("LOCALAPPDATA")
            .or_else(|| env("USERPROFILE").map(|p| p.join("AppData").join("Local")))?,
    };
    Some(dir.join("flightdeck").join(FILE))
}

/// The version a notice should announce, if any: the cached finding when a
/// check is not due, otherwise a fresh query (whose answer is cached). Any
/// failure is `None`: an update notice must never get in the way.
pub fn newer_version(
    fs: &dyn FileSystem,
    source: &dyn ReleaseSource,
    cache_file: &Path,
    now_unix: u64,
    current: Version,
) -> Option<Version> {
    let cache = fs
        .read_to_string(cache_file)
        .ok()
        .and_then(|text| parse_cache(&text));
    let (due, cached) = evaluate(cache.as_ref(), now_unix, current);
    if !due {
        return cached;
    }
    let latest = source.latest().ok()??.version;
    if let Some(dir) = cache_file.parent() {
        let _ = fs.create_dir_all(dir);
    }
    let _ = fs.write(
        cache_file,
        &render_cache(&CheckCache {
            last_check_unix: now_unix,
            latest_version: latest.to_string(),
        }),
    );
    (latest > current).then_some(latest)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use flightdeck::testing::FakeFs;

    use super::super::release::ReleaseInfo;
    use super::super::source::UpdateError;
    use super::*;

    fn v(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    /// A release source that answers with a fixed version and counts asks.
    struct Fixed {
        latest: Result<Option<&'static str>, ()>,
        asked: Cell<u32>,
    }

    impl ReleaseSource for Fixed {
        fn latest(&self) -> Result<Option<ReleaseInfo>, UpdateError> {
            self.asked.set(self.asked.get() + 1);
            match self.latest {
                Ok(version) => Ok(version.map(|version| ReleaseInfo {
                    version: v(version),
                    page_url: String::new(),
                    assets: vec![],
                })),
                Err(()) => Err(UpdateError::Network("offline".into())),
            }
        }

        fn download(&self, _: &str, _: &Path) -> Result<(), UpdateError> {
            unreachable!("a check never downloads")
        }
    }

    fn fixed(latest: Result<Option<&'static str>, ()>) -> Fixed {
        Fixed {
            latest,
            asked: Cell::new(0),
        }
    }

    const CACHE: &str = "/cache/flightdeck/desktop-update-check.json";

    #[test]
    fn the_cache_round_trips_and_junk_is_no_cache() {
        let cache = CheckCache {
            last_check_unix: 42,
            latest_version: "1.2.3".into(),
        };
        assert_eq!(parse_cache(&render_cache(&cache)), Some(cache));
        for junk in [
            "",
            "{",
            "{}",
            r#"{"last_check_unix":"x","latest_version":"1"}"#,
        ] {
            assert_eq!(parse_cache(junk), None, "{junk:?}");
        }
    }

    #[test]
    fn a_check_is_due_after_a_full_day_and_the_cache_still_speaks_before() {
        let cache = CheckCache {
            last_check_unix: 1_000,
            latest_version: "1.5.0".into(),
        };
        let (due, notice) = evaluate(Some(&cache), 1_000 + CHECK_INTERVAL_SECS - 1, v("1.4.0"));
        assert!(!due);
        assert_eq!(notice, Some(v("1.5.0")));
        let (due, _) = evaluate(Some(&cache), 1_000 + CHECK_INTERVAL_SECS, v("1.4.0"));
        assert!(due);
        let (due, notice) = evaluate(None, 5, v("1.4.0"));
        assert!(due && notice.is_none());
        // Caught up: nothing to announce.
        assert_eq!(evaluate(Some(&cache), 1_001, v("1.5.0")).1, None);
    }

    #[test]
    fn a_fresh_check_queries_once_and_remembers_the_answer() {
        let fs = FakeFs::new();
        let source = fixed(Ok(Some("1.5.0")));
        let path = Path::new(CACHE);
        assert_eq!(
            newer_version(&fs, &source, path, 10_000, v("1.4.0")),
            Some(v("1.5.0"))
        );
        assert_eq!(source.asked.get(), 1);
        // An hour later: answered from the cache, no second request.
        assert_eq!(
            newer_version(&fs, &source, path, 13_600, v("1.4.0")),
            Some(v("1.5.0"))
        );
        assert_eq!(source.asked.get(), 1);
        // A day later: asked again.
        let later = 10_000 + CHECK_INTERVAL_SECS;
        newer_version(&fs, &source, path, later, v("1.4.0"));
        assert_eq!(source.asked.get(), 2);
    }

    #[test]
    fn failures_and_being_current_are_silent() {
        let fs = FakeFs::new();
        let path = Path::new(CACHE);
        assert_eq!(
            newer_version(&fs, &fixed(Err(())), path, 1, v("1.4.0")),
            None
        );
        assert_eq!(
            newer_version(&fs, &fixed(Ok(None)), path, 1, v("1.4.0")),
            None
        );
        assert_eq!(
            newer_version(&fs, &fixed(Ok(Some("1.4.0"))), path, 1, v("1.4.0")),
            None
        );
    }

    #[test]
    fn the_cache_lives_in_each_oss_own_place() {
        let env = |name: &str| -> Option<PathBuf> {
            match name {
                "HOME" => Some("/home/u".into()),
                "LOCALAPPDATA" => Some("C:/Users/u/AppData/Local".into()),
                _ => None,
            }
        };
        let tail = Path::new("flightdeck").join("desktop-update-check.json");
        assert_eq!(
            cache_path(Os::MacOs, &env),
            Some(Path::new("/home/u/Library/Application Support").join(&tail))
        );
        assert_eq!(
            cache_path(Os::Linux, &env),
            Some(Path::new("/home/u/.cache").join(&tail))
        );
        assert_eq!(
            cache_path(Os::Windows, &env),
            Some(Path::new("C:/Users/u/AppData/Local").join(&tail))
        );
        let xdg = |name: &str| (name == "XDG_CACHE_HOME").then(|| PathBuf::from("/x"));
        assert_eq!(
            cache_path(Os::Linux, &xdg),
            Some(Path::new("/x").join(&tail))
        );
        assert_eq!(cache_path(Os::MacOs, &|_: &str| None), None);
    }
}
