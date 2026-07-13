//! Custom `update_informer::Registry` that hits only the `wxctl-updates`
//! Worker. Parses `{ latest, news }`, stashes `news` for the engine to dedup,
//! and returns `latest` for update-informer's semver comparison.

use std::sync::OnceLock;

use parking_lot::Mutex;
use serde::Deserialize;
use update_informer::{
    Package, Registry, Result,
    http_client::{GenericHttpClient, HttpClient},
};

use crate::update::{ChangelogEntry, NewsItem};

/// Public production endpoint (carve-out invariant I1: hardcoded, no private host).
pub const DEFAULT_ENDPOINT: &str = "https://wxctl-updates.randyphoa.workers.dev/check";

/// News stashed by the most recent fetch, drained by the engine via `take_news`.
static NEWS_SLOT: Mutex<Option<Vec<NewsItem>>> = Mutex::new(None);

/// Changelog stashed by the most recent fetch, drained via `take_changelog`.
static CHANGELOG_SLOT: Mutex<Option<Vec<ChangelogEntry>>> = Mutex::new(None);

/// Worker `/check` response shape. `latest` is omitted when GitHub is
/// unreachable; missing `latest` means "no update".
#[derive(Debug, Deserialize)]
struct CheckResponse {
    #[serde(default)]
    latest: Option<String>,
    #[serde(default)]
    news: Vec<NewsItem>,
    #[serde(default)]
    changelog: Vec<ChangelogEntry>,
}

/// Resolve the endpoint: `WXCTL_UPDATE_ENDPOINT` override (dev/test only) else
/// the public const.
fn endpoint() -> String {
    std::env::var("WXCTL_UPDATE_ENDPOINT").unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string())
}

/// `wxctl/<version> (<os>; <arch>)`, plus a `; npm` suffix for npm installs,
/// built once into a `&'static str` so it satisfies `add_header`'s
/// client-lifetime bound on the header refs.
fn user_agent() -> &'static str {
    static UA: OnceLock<String> = OnceLock::new();
    UA.get_or_init(|| user_agent_string(crate::update::installed_via_npm())).as_str()
}

/// Build the `/check` User-Agent. Appends `; npm` for npm installs (P2) so a
/// later Worker analytics change can split npm vs binary adoption; the Worker
/// reads only parts 0-1 today, so the extra `;`-part is ignored harmlessly.
/// Pure for deterministic unit-testing.
fn user_agent_string(via_npm: bool) -> String {
    let suffix = if via_npm { "; npm" } else { "" };
    format!("wxctl/{} ({}; {}{})", env!("CARGO_PKG_VERSION"), std::env::consts::OS, std::env::consts::ARCH, suffix)
}

/// Drain the news stashed by the last fetch (empty if no fetch happened, e.g. a
/// cached run where the registry was never invoked).
pub fn take_news() -> Vec<NewsItem> {
    NEWS_SLOT.lock().take().unwrap_or_default()
}

/// Drain the changelog stashed by the last fetch (empty on a cached run where
/// the registry was never invoked). Mirrors `take_news`.
pub fn take_changelog() -> Vec<ChangelogEntry> {
    CHANGELOG_SLOT.lock().take().unwrap_or_default()
}

/// Marker registry passed to `update_informer::new`.
pub struct WxctlUpdates;

impl Registry for WxctlUpdates {
    const NAME: &'static str = "wxctl";

    fn get_latest_version<T: HttpClient>(http_client: GenericHttpClient<T>, _pkg: &Package) -> Result<Option<String>> {
        let url = endpoint();
        let resp: CheckResponse = http_client.add_header("User-Agent", user_agent()).get(&url)?;
        *NEWS_SLOT.lock() = Some(resp.news);
        *CHANGELOG_SLOT.lock() = Some(resp.changelog);
        Ok(resp.latest)
    }
}

/// Full `/check` payload for the explicit `update` command. Mirrors the
/// background path's stash but is returned directly rather than parked in a
/// slot. The update command reads only `latest` + `changelog`; `news` is carried
/// for wire-contract completeness.
pub struct CheckResult {
    pub latest: Option<String>,
    #[allow(dead_code)] // wire-contract completeness; the update command reads latest + changelog
    pub news: Vec<NewsItem>,
    pub changelog: Vec<ChangelogEntry>,
}

/// One direct `/check` GET for `wxctl update`. Unlike the background path it
/// ignores the 24h interval and never writes the interval stamp — `update` is an
/// explicit user action. Reuses the same endpoint + User-Agent as the background
/// fetch.
pub fn fetch_check() -> anyhow::Result<CheckResult> {
    let url = endpoint();
    let mut resp = ureq::get(&url).header("User-Agent", user_agent()).call().map_err(|e| anyhow::anyhow!("update check request to {url} failed: {e}"))?;
    let body = resp.body_mut().read_to_string().map_err(|e| anyhow::anyhow!("reading /check response failed: {e}"))?;
    let parsed: CheckResponse = serde_json::from_str(&body).map_err(|e| anyhow::anyhow!("parsing /check response failed: {e}"))?;
    Ok(CheckResult { latest: parsed.latest, news: parsed.news, changelog: parsed.changelog })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Severity;

    #[test]
    fn parses_check_response_with_news() {
        let json = r#"{"latest":"0.2.0","news":[{"id":"welcome-2026-06","severity":"security","title":"Heads up"}]}"#;
        let resp: CheckResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.latest.as_deref(), Some("0.2.0"));
        assert_eq!(resp.news.len(), 1);
        assert_eq!(resp.news[0].id, "welcome-2026-06");
        assert_eq!(resp.news[0].severity, Severity::Security);
    }

    #[test]
    fn missing_latest_is_none() {
        let resp: CheckResponse = serde_json::from_str(r#"{"news":[]}"#).unwrap();
        assert!(resp.latest.is_none());
        assert!(resp.news.is_empty());
    }

    #[test]
    fn user_agent_appends_npm_suffix() {
        assert!(user_agent_string(true).ends_with("; npm)"), "npm UA: {}", user_agent_string(true));
        assert!(!user_agent_string(false).contains("npm"), "binary UA has no npm: {}", user_agent_string(false));
        assert!(user_agent_string(false).ends_with(')'), "binary UA is well-formed: {}", user_agent_string(false));
    }
}
