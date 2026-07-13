//! Fail-silent background update check + curated news.
//!
//! The CLI's only outbound host for this feature is the `wxctl-updates`
//! Cloudflare Worker (the Worker proxies GitHub server-side). Any error on this
//! path is swallowed — logged at `debug` on target `wxctl::update`; exit code
//! and stdout are never affected.

use serde::Deserialize;
use std::ffi::OsStr;
use std::path::Path;

pub mod cache;
pub mod check;
pub mod download;
pub mod registry;

/// Severity of a curated news item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Security,
}

/// One curated news item served by the Worker's `/check` endpoint. The renderer
/// reads `severity`/`title`/`body`/`url`; `fixed_in`/`max_version` drive the
/// security re-show logic. `min_version` and `expires` are part of the wire
/// contract but applied **Worker-side** (the CLI never reads them), so they
/// carry a narrowed `#[allow(dead_code)]`.
#[derive(Debug, Clone, Deserialize)]
pub struct NewsItem {
    pub id: String,
    pub severity: Severity,
    pub title: String,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field; filtered Worker-side, not read by the CLI
    pub min_version: Option<String>,
    #[serde(default)]
    pub max_version: Option<String>,
    #[serde(default)]
    pub fixed_in: Option<String>,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field; filtered Worker-side, not read by the CLI
    pub expires: Option<String>,
}

/// One per-version release-notes entry served by the Worker's `/check`
/// endpoint. The Worker filters the array to versions in `(current, latest]`
/// and sorts newest-first, so `changelog[0]` is the newest gainable release.
/// `date` is part of the wire contract but not read by the CLI renderer.
#[derive(Debug, Clone, Deserialize)]
pub struct ChangelogEntry {
    pub version: String,
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    #[allow(dead_code)] // wire-contract field; not read by the notice renderer
    pub date: Option<String>,
}

/// Result of a completed update check, ready for rendering.
#[derive(Debug, Clone)]
pub struct UpdateNotice {
    /// The running binary's version (`env!("CARGO_PKG_VERSION")` at fetch time),
    /// so the renderer can show `current → latest` (AC 1) deterministically in
    /// snapshots without embedding the live crate version.
    pub current_version: String,
    /// `Some(version)` when the Worker reports a newer release than the running
    /// binary; `None` when up to date or `latest` was omitted by the Worker.
    pub new_version: Option<String>,
    /// News items selected for display after dedup / re-show filtering.
    pub news: Vec<NewsItem>,
    /// Per-version release notes for `(current, latest]`, newest-first, as
    /// served by the Worker. Drives the "What's new" block in the notice.
    pub changelog: Vec<ChangelogEntry>,
}

/// `true` when wxctl was installed via npm: the shim exports
/// `WXCTL_INSTALL_METHOD=npm`, or the running binary sits inside a
/// `node_modules` tree (a global `npm i -g` layout). Consumers: `wxctl update`
/// (refuse + redirect), the post-command notice (upgrade-command text), and the
/// `/check` User-Agent suffix.
pub fn installed_via_npm() -> bool {
    installed_via_npm_from(std::env::var_os("WXCTL_INSTALL_METHOD").as_deref(), std::env::current_exe().ok().as_deref())
}

/// Pure decision behind `installed_via_npm`, split out for deterministic tests
/// (no process-global env / `current_exe` mutation).
fn installed_via_npm_from(marker: Option<&OsStr>, exe: Option<&Path>) -> bool {
    if marker == Some(OsStr::new("npm")) {
        return true;
    }
    exe.map(|p| p.components().any(|c| c.as_os_str() == OsStr::new("node_modules"))).unwrap_or(false)
}

#[cfg(test)]
mod install_method_tests {
    use super::*;

    #[test]
    fn marker_npm_is_true() {
        assert!(installed_via_npm_from(Some(OsStr::new("npm")), None));
    }

    #[test]
    fn other_marker_is_false() {
        assert!(!installed_via_npm_from(Some(OsStr::new("curl")), None));
    }

    #[test]
    fn node_modules_path_is_true() {
        assert!(installed_via_npm_from(None, Some(Path::new("/usr/lib/node_modules/wxctl/bin/wxctl"))));
    }

    #[test]
    fn plain_path_is_false() {
        assert!(!installed_via_npm_from(None, Some(Path::new("/usr/local/bin/wxctl"))));
    }

    #[test]
    fn no_marker_no_exe_is_false() {
        assert!(!installed_via_npm_from(None, None));
    }
}
