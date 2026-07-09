//! Fail-silent update-check engine: kill switches → background fetch → dedup.

use std::io::IsTerminal;
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use update_informer::Check;

use crate::config::env_bool;
use crate::update::cache;
use crate::update::registry::{self, WxctlUpdates};
use crate::update::{NewsItem, Severity, UpdateNotice};

/// 24h between live fetches; cached runs add ~0 latency (no `/check` request).
const CHECK_INTERVAL: Duration = Duration::from_secs(60 * 60 * 24);
/// Worst-case added wall-clock on a fetching run; also the bounded-join cap.
const CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// All inputs to the kill-switch gate, captured explicitly so the gate is a
/// pure function — unit-testable without mutating process-global env / TTY state.
pub struct GateInputs {
    pub no_update_check_flag: bool,
    pub is_mcp_command: bool,
    pub stdout_is_terminal: bool,
    pub env_no_update_check: bool,
    pub env_do_not_track: bool,
    pub env_ci: bool,
}

impl GateInputs {
    /// Capture the live environment + CLI state.
    pub fn from_env(no_update_check_flag: bool, is_mcp_command: bool) -> Self {
        // `WXCTL_UPDATE_FORCE_TTY` is a documented test-only override (mirrors
        // WXCTL_UPDATE_ENDPOINT): it lets the E2E exercise the notice path under
        // captured (piped) stdout. Never set in production — the real non-TTY
        // gate (AC 6) is tested by omitting it.
        let stdout_is_terminal = std::io::stdout().is_terminal() || env_bool("WXCTL_UPDATE_FORCE_TTY");
        Self { no_update_check_flag, is_mcp_command, stdout_is_terminal, env_no_update_check: env_bool("WXCTL_NO_UPDATE_CHECK"), env_do_not_track: env_bool("DO_NOT_TRACK"), env_ci: std::env::var_os("CI").is_some() }
    }
}

/// `Some(reason)` when the update check must be suppressed (no `/check` request).
pub fn check_suppressed(g: &GateInputs) -> Option<&'static str> {
    if g.no_update_check_flag {
        return Some("--no-update-check");
    }
    if g.env_no_update_check {
        return Some("WXCTL_NO_UPDATE_CHECK");
    }
    if g.env_do_not_track {
        return Some("DO_NOT_TRACK");
    }
    if g.is_mcp_command {
        return Some("mcp serve");
    }
    if !g.stdout_is_terminal {
        return Some("non-tty");
    }
    if g.env_ci {
        return Some("CI");
    }
    None
}

/// Handle to the background check; bounded-joined just before process exit.
pub struct UpdateCheck {
    rx: mpsc::Receiver<Option<UpdateNotice>>,
}

impl UpdateCheck {
    /// Wait up to `CHECK_TIMEOUT` for the result; a slow/hung check yields `None`
    /// (the detached thread is abandoned at process exit). Suppressed checks
    /// return immediately.
    pub fn join_timeout(self) -> Option<UpdateNotice> {
        self.rx.recv_timeout(CHECK_TIMEOUT).ok().flatten()
    }
}

/// Spawn the fail-silent background check. Honors every kill switch *before* any
/// network request; suppressed → no thread, no `/check`, immediate `None`.
pub fn spawn_background_check(current: &str, flags: GateInputs) -> UpdateCheck {
    let (tx, rx) = mpsc::channel();
    if let Some(reason) = check_suppressed(&flags) {
        tracing::debug!(target: "wxctl::update", reason, "update check suppressed");
        let _ = tx.send(None);
        return UpdateCheck { rx };
    }
    let current = current.to_string();
    // Dedicated std::thread, isolated from the tokio runtime (ureq is blocking).
    let spawned = thread::Builder::new().name("wxctl-update-check".into()).spawn(move || {
        let _ = tx.send(run_check(&current));
    });
    if spawned.is_err() {
        tracing::debug!(target: "wxctl::update", "could not spawn update-check thread");
        // tx was moved into the failed closure and dropped; rx disconnects → None.
    }
    UpdateCheck { rx }
}

/// Blocking fetch + dedup. Runs on the dedicated thread; never touches tokio.
fn run_check(current: &str) -> Option<UpdateNotice> {
    // Our own 24h interval gate. update-informer's file cache never fetches on
    // the first run (it primes the cache with `current` and returns `None` until
    // its interval elapses), so we run the informer with a ZERO interval (=
    // fetch whenever called) and decide here whether to call it at all. A run
    // with no/stale stamp fetches; a run within CHECK_INTERVAL of the last
    // successful fetch skips — no `/check` request, ~0 added latency.
    let stamp = cache::last_check_path();
    if let Some(path) = stamp.as_ref()
        && let Some(age) = cache::last_check_age(path)
        && age < CHECK_INTERVAL
    {
        return None;
    }

    let informer = update_informer::new(WxctlUpdates, "wxctl", current).interval(Duration::ZERO).timeout(CHECK_TIMEOUT);
    let new_version = match informer.check_version() {
        Ok(opt) => {
            // Successful fetch — record the timestamp so the next run within
            // CHECK_INTERVAL skips the network. A failed fetch is NOT recorded,
            // so a transient error doesn't suppress retries for 24h.
            if let Some(path) = stamp.as_ref() {
                let _ = cache::record_check(path);
            }
            opt.map(|v| v.to_string())
        }
        Err(e) => {
            tracing::debug!(target: "wxctl::update", error = %e, "update check failed");
            None
        }
    };
    // The registry stashes news only on an actual fetch; cached runs drain empty.
    let news = registry::take_news();
    let changelog = registry::take_changelog();
    let seen = cache::news_seen_path().map(|p| cache::load_seen(&p)).unwrap_or_default();
    let news = dedup_news(news, current, &seen);
    if new_version.is_none() && news.is_empty() && changelog.is_empty() {
        return None;
    }
    Some(UpdateNotice { current_version: current.to_string(), new_version, news, changelog })
}

/// Select news for display:
/// - `info` items dedup on id — dropped once acknowledged in `seen`.
/// - `security` items ignore `seen` and re-show on every fetch until `current`
///   satisfies their `fixed_in`/`max_version` (then they stop). An unparseable
///   `current` fails safe → still shown.
fn dedup_news(news: Vec<NewsItem>, current: &str, seen: &cache::SeenState) -> Vec<NewsItem> {
    let cur = semver::Version::parse(current).ok();
    news.into_iter()
        .filter(|n| match n.severity {
            Severity::Info => !seen.shown_ids.contains(&n.id),
            Severity::Security => security_still_affected(cur.as_ref(), n),
        })
        .collect()
}

/// `true` while a security item still applies to `current` (so it re-shows).
fn security_still_affected(cur: Option<&semver::Version>, n: &NewsItem) -> bool {
    let Some(cur) = cur else { return true };
    if let Some(fixed) = n.fixed_in.as_deref().and_then(|s| semver::Version::parse(s).ok())
        && *cur >= fixed
    {
        return false;
    }
    if let Some(max) = n.max_version.as_deref().and_then(|s| semver::Version::parse(s).ok())
        && *cur > max
    {
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::update::Severity;
    use crate::update::cache::SeenState;

    fn inputs() -> GateInputs {
        // All-clear baseline: terminal attached, nothing set.
        GateInputs { no_update_check_flag: false, is_mcp_command: false, stdout_is_terminal: true, env_no_update_check: false, env_do_not_track: false, env_ci: false }
    }

    #[test]
    fn all_clear_runs_check() {
        assert!(check_suppressed(&inputs()).is_none());
    }

    #[test]
    fn each_kill_switch_suppresses() {
        assert_eq!(check_suppressed(&GateInputs { no_update_check_flag: true, ..inputs() }), Some("--no-update-check"));
        assert_eq!(check_suppressed(&GateInputs { env_no_update_check: true, ..inputs() }), Some("WXCTL_NO_UPDATE_CHECK"));
        assert_eq!(check_suppressed(&GateInputs { env_do_not_track: true, ..inputs() }), Some("DO_NOT_TRACK"));
        assert_eq!(check_suppressed(&GateInputs { is_mcp_command: true, ..inputs() }), Some("mcp serve"));
        assert_eq!(check_suppressed(&GateInputs { stdout_is_terminal: false, ..inputs() }), Some("non-tty"));
        assert_eq!(check_suppressed(&GateInputs { env_ci: true, ..inputs() }), Some("CI"));
    }

    fn item(id: &str, sev: Severity) -> NewsItem {
        NewsItem { id: id.into(), severity: sev, title: "t".into(), body: None, url: None, min_version: None, max_version: None, fixed_in: None, expires: None }
    }

    #[test]
    fn info_dedup_drops_seen_ids() {
        let news = vec![item("a", Severity::Info), item("b", Severity::Info)];
        let seen = SeenState { shown_ids: vec!["a".into()] };
        let kept = dedup_news(news, "0.1.0", &seen);
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].id, "b");
    }

    #[test]
    fn security_reshows_until_fixed() {
        // Security item already "seen" must STILL show while current < fixed_in.
        let seen = SeenState { shown_ids: vec!["sec".into()] };
        let mut sec = item("sec", Severity::Security);
        sec.fixed_in = Some("0.2.0".into());
        assert_eq!(dedup_news(vec![sec.clone()], "0.1.0", &seen).len(), 1, "re-shows when current < fixed_in");
        // Once current satisfies fixed_in, it stops — even though id ∈ seen.
        assert_eq!(dedup_news(vec![sec.clone()], "0.2.0", &seen).len(), 0, "stops when current >= fixed_in");
        // max_version bound: stops once current > max_version.
        let mut sec2 = item("sec2", Severity::Security);
        sec2.max_version = Some("0.1.5".into());
        assert_eq!(dedup_news(vec![sec2.clone()], "0.1.5", &seen).len(), 1, "shows at the boundary");
        assert_eq!(dedup_news(vec![sec2], "0.1.6", &seen).len(), 0, "stops past max_version");
    }
}
