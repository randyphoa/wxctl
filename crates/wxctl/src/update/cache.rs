//! News-acknowledgement state: `~/.wxctl/update-news-seen.json`.
//!
//! Tests pass an explicit path and never mutate `HOME`/CWD — process-global env
//! mutation races other unit tests
//! (see docs/troubleshoot/archive/env-var-test-race-fix.md).

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Ids of news items already shown to the user.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SeenState {
    #[serde(default)]
    pub shown_ids: Vec<String>,
}

/// The directory holding update-check state: `~/.wxctl`, or the
/// `WXCTL_UPDATE_CACHE_DIR` test-only override (mirrors WXCTL_UPDATE_ENDPOINT).
/// The override exists because the subprocess E2E isolates each child with a
/// temp `HOME`, which `dirs::home_dir` ignores on Windows (it asks the
/// known-folder API) — without it, parallel tests race on the real `~/.wxctl`.
fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("WXCTL_UPDATE_CACHE_DIR").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".wxctl")))
}

/// `~/.wxctl/update-news-seen.json`, or `None` if the home dir is undiscoverable.
pub fn news_seen_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("update-news-seen.json"))
}

/// Load seen state; any error (missing/corrupt) yields the empty default.
pub fn load_seen(path: &Path) -> SeenState {
    std::fs::read_to_string(path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_default()
}

/// Persist seen state (creates `~/.wxctl/` if needed).
/// Wired into `main.rs` (persist shown `info` ids after rendering the notice).
pub fn save_seen(path: &Path, state: &SeenState) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_string_pretty(state).unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path, body)
}

/// `~/.wxctl/update-last-check` — its mtime gates the 24h fetch interval. We run
/// `update-informer` with a ZERO interval (fetch whenever called) and gate here,
/// because update-informer's own file cache never fetches on the first run (it
/// primes the cache with the current version and returns `None` until its
/// interval elapses). `None` if the home dir is undiscoverable.
pub fn last_check_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("update-last-check"))
}

/// Time elapsed since the last recorded check, or `None` if it has never run or
/// the timestamp is unreadable (→ treated as "due for a check").
pub fn last_check_age(path: &Path) -> Option<Duration> {
    std::fs::metadata(path).ok()?.modified().ok()?.elapsed().ok()
}

/// Record that a live `/check` fetch just happened — sets the stamp's mtime to
/// now (creates `~/.wxctl/` if needed). Called only after a successful fetch.
pub fn record_check(path: &Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, b"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_seen_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-news-seen.json");

        // Missing file → empty default.
        assert!(load_seen(&path).shown_ids.is_empty());

        let state = SeenState { shown_ids: vec!["welcome-2026-06".to_string()] };
        save_seen(&path, &state).unwrap();

        let loaded = load_seen(&path);
        assert_eq!(loaded.shown_ids, vec!["welcome-2026-06".to_string()]);
    }

    #[test]
    fn corrupt_file_yields_default() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bad.json");
        std::fs::write(&path, "not json").unwrap();
        assert!(load_seen(&path).shown_ids.is_empty());
    }

    #[test]
    fn records_and_ages_check_stamp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("update-last-check");
        // No stamp yet → age is None (due for a check).
        assert!(last_check_age(&path).is_none());
        record_check(&path).unwrap();
        // Freshly recorded → age is Some and small.
        let age = last_check_age(&path).expect("age after record");
        assert!(age < std::time::Duration::from_secs(60), "fresh stamp age is small: {age:?}");
    }
}
