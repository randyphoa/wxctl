//! Final-phase subprocess E2E for the update-check + news feature.
//! Drives the freshly-built binary; a std TcpListener stub records hits.
mod common;

use common::{dead_endpoint, run_resources, start};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// A version guaranteed greater than the running binary (forces "update available").
const NEWER: &str = "99.0.0";

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// AC 1: update-available notice to stderr; stdout + exit code unchanged.
#[test]
fn ac1_update_available_on_stderr() {
    let h = home();
    let stub = start(format!(r#"{{"latest":"{NEWER}","news":[]}}"#), None);
    let out = run_resources(h.path(), &stub.url, true, &[]);
    assert!(out.status.success(), "exit code unchanged (0)");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(NEWER) && err.contains('\u{2192}'), "update line `current → latest` on stderr: {err}");
    // stdout matches a --no-update-check baseline (notice is stderr-only).
    let base = run_resources(home().path(), &stub.url, true, &["--no-update-check"]);
    assert_eq!(out.stdout, base.stdout, "stdout is unchanged by the notice");
    assert!(!out.stdout.contains(&0x1b), "no notice ANSI leaks onto stdout");
}

/// AC 2: latest == current → no update line.
#[test]
fn ac2_up_to_date_prints_nothing() {
    let h = home();
    let same = env!("CARGO_PKG_VERSION");
    let stub = start(format!(r#"{{"latest":"{same}","news":[]}}"#), None);
    let out = run_resources(h.path(), &stub.url, true, &[]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("available"), "no update line when up to date");
}

/// AC 3 (info dedup + persistence end-to-end) + AC 10 (cache honored):
/// shown once, persisted to seen.json, not reprinted on a within-interval re-run.
#[test]
fn ac3_info_shown_once_persisted_and_ac10_cache() {
    let h = home();
    let stub = start(r#"{"latest":null,"news":[{"id":"welcome-x","severity":"info","title":"Hello there"}]}"#.into(), None);
    let r1 = run_resources(h.path(), &stub.url, true, &[]);
    assert!(String::from_utf8_lossy(&r1.stderr).contains("Hello there"), "info shown on the first run");
    let seen = std::fs::read_to_string(h.path().join(".wxctl/update-news-seen.json")).expect("seen.json written");
    assert!(seen.contains("welcome-x"), "info id persisted: {seen}");
    // Second run within the 24h interval: cache honored → no /check, not reprinted.
    let r2 = run_resources(h.path(), &stub.url, true, &[]);
    assert!(!String::from_utf8_lossy(&r2.stderr).contains("Hello there"), "info not reprinted on the second run");
    assert_eq!(stub.hits(), 1, "AC 10: second run made no /check request (cache honored)");
}

/// One kill-switch case: (extra env vars to set, extra CLI args to pass).
type KillCase = (&'static [(&'static str, &'static str)], &'static [&'static str]);

/// AC 4: each kill switch → no /check request, no notice.
#[test]
fn ac4_kill_switches_suppress() {
    let cases: &[KillCase] = &[(&[("WXCTL_NO_UPDATE_CHECK", "1")], &[]), (&[("DO_NOT_TRACK", "1")], &[]), (&[], &["--no-update-check"])];
    for (envs, args) in cases {
        let h = home();
        let stub = start(format!(r#"{{"latest":"{NEWER}","news":[]}}"#), None);
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_wxctl"));
        cmd.arg("resources").args(*args).env("HOME", h.path()).env("WXCTL_UPDATE_CACHE_DIR", h.path().join(".wxctl")).env("WXCTL_UPDATE_ENDPOINT", &stub.url).env("WXCTL_UPDATE_FORCE_TTY", "1").env_remove("CI").stdin(Stdio::null());
        for (k, v) in *envs {
            cmd.env(k, v);
        }
        let out = cmd.output().unwrap();
        assert_eq!(stub.hits(), 0, "kill switch {envs:?}/{args:?} made no /check request");
        assert!(!String::from_utf8_lossy(&out.stderr).contains(NEWER), "kill switch {envs:?}/{args:?} printed no notice");
    }
}

/// AC 5: `wxctl mcp serve` issues no /check and emits nothing from this feature.
#[test]
fn ac5_mcp_serve_makes_no_check() {
    let h = home();
    let stub = start(format!(r#"{{"latest":"{NEWER}","news":[]}}"#), None);
    // Spawn, give the gate time to (not) fire, then kill — robust to the server blocking.
    let mut child = Command::new(env!("CARGO_BIN_EXE_wxctl"))
        .args(["mcp", "serve"])
        .env("HOME", h.path())
        .env("WXCTL_UPDATE_CACHE_DIR", h.path().join(".wxctl"))
        .env("WXCTL_UPDATE_ENDPOINT", &stub.url)
        .env("WXCTL_UPDATE_FORCE_TTY", "1")
        .env_remove("CI")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(800));
    let _ = child.kill();
    let _ = child.wait();
    assert_eq!(stub.hits(), 0, "mcp serve made no /check request (gate: is_mcp_command)");
}

/// AC 6: piped (non-TTY) stdout is byte-identical to a disabled baseline; no /check.
#[test]
fn ac6_piped_non_tty_byte_identical() {
    let stub = start(r#"{"latest":"99.0.0","news":[{"id":"n","severity":"info","title":"Hi"}]}"#.into(), None);
    // Feature enabled but piped (no force-tty) → real non-TTY gate suppresses it.
    let feat = run_resources(home().path(), &stub.url, false, &[]);
    // Baseline: feature explicitly disabled.
    let base = run_resources(home().path(), &stub.url, false, &["--no-update-check"]);
    assert_eq!(stub.hits(), 0, "non-tty stdout made no /check request");
    assert_eq!(feat.stdout, base.stdout, "piped stdout is byte-identical to the disabled baseline");
    assert!(!feat.stdout.contains(&0x1b), "no ANSI on piped stdout");
    assert!(feat.stderr.is_empty() || !String::from_utf8_lossy(&feat.stderr).contains("available"), "no notice on stderr when suppressed");
}

/// AC 7 (changelog spec 2026-06-28): a `changelog` in `/check` renders a "What's new" block on
/// STDERR; the upgrade hint reads "Run `wxctl update`" (not the bare URL); stdout + exit 0
/// unchanged.
#[test]
fn ac7_changelog_notice_on_stderr() {
    let h = home();
    let body = format!(r#"{{"latest":"{NEWER}","news":[],"changelog":[{{"version":"{NEWER}","notes":["Brand new feature","Another improvement"]}}]}}"#);
    let stub = start(body, None);
    let out = run_resources(h.path(), &stub.url, true, &[]);
    assert!(out.status.success(), "exit 0 unchanged");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains(&format!("What's new in v{NEWER}")), "What's new block on stderr: {err}");
    assert!(err.contains("Brand new feature"), "changelog note rendered: {err}");
    assert!(err.contains("Run `wxctl update`"), "upgrade hint flipped to `Run wxctl update`: {err}");
    assert!(!err.contains("/releases"), "bare releases URL is not used as the hint: {err}");
    let base = run_resources(home().path(), &stub.url, true, &["--no-update-check"]);
    assert_eq!(out.stdout, base.stdout, "stdout unchanged by the notice");
}

/// AC 7 (prior spec): unreachable or slow /check → unchanged exit/output, bounded wall-clock.
#[test]
fn ac7_unreachable_and_slow_are_bounded_and_silent() {
    // Connection refused → fast fail-silent.
    let t = Instant::now();
    let out = run_resources(home().path(), &dead_endpoint(), true, &[]);
    assert!(out.status.success(), "refused check does not fail the command");
    assert!(t.elapsed() < Duration::from_secs(10), "refused check returns promptly");
    // Slow endpoint sleeps past the ~3s timeout → still bounded, exit 0, no notice.
    let stub = start(format!(r#"{{"latest":"{NEWER}","news":[]}}"#), Some(Duration::from_secs(6)));
    let t2 = Instant::now();
    let out2 = run_resources(home().path(), &stub.url, true, &[]);
    assert!(out2.status.success(), "slow check does not fail the command");
    assert!(t2.elapsed() < Duration::from_secs(8), "added wall-clock bounded by the ~3s timeout");
    assert!(!String::from_utf8_lossy(&out2.stderr).contains(NEWER), "no notice when the check times out");
}
