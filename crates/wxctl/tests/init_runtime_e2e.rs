//! Final-phase black-box E2E for the profiles.yaml-only loader and the
//! non-interactive init path. Covers AC 6 (no prompting / no hang) and
//! AC 7 (runtime reads only profiles.yaml; --profile-path resolves).
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};
use tempfile::TempDir;

fn base(home: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_wxctl"));
    // WXCTL_CONFIG_DIR redirects the profiles/active-profile lookup; HOME alone
    // does not, since dirs::home_dir ignores HOME on Windows (known-folder API).
    c.env("HOME", home).env("WXCTL_CONFIG_DIR", home.join(".wxctl")).env("WXCTL_UPDATE_CACHE_DIR", home.join(".wxctl")).env_remove("CI").env_remove("GITHUB_ACTIONS").env_remove("VISUAL").env_remove("EDITOR").stdin(Stdio::null());
    c
}

fn run(home: &Path, args: &[&str]) -> Output {
    base(home).args(args).arg("--no-update-check").output().expect("spawn wxctl")
}

/// AC 6: `wxctl init` does no prompting and no terminal probing. With stdin
/// closed on a non-TTY it exits 0 and returns quickly (regression guard for the
/// removed OSC/stdin deadlock). A bounded spawn+poll replaces `timeout` (macOS
/// has none); an interactive hang would blow the cap.
#[test]
fn ac6_non_tty_completes_promptly_exit0() {
    let h = TempDir::new().unwrap();
    let f = h.path().join("p.yaml");
    let start = Instant::now();
    let mut child = base(h.path()).args(["init", "--profile-path", f.to_str().unwrap(), "--no-update-check"]).spawn().expect("spawn");
    let cap = Duration::from_secs(30); // observed wall clock ~0.06s; cap only catches a hang.
    let status = loop {
        if let Some(s) = child.try_wait().expect("try_wait") {
            break s;
        }
        if start.elapsed() > cap {
            let _ = child.kill();
            panic!("init hung past {cap:?} with stdin closed (interactive-prompt regression)");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(status.success(), "init exits 0 on a non-TTY");
    assert!(f.exists(), "scaffold written");
    assert!(start.elapsed() < Duration::from_secs(10), "init returns promptly, no interactive wait");
}

/// AC 7: the runtime loads profiles only from profiles.yaml. A profile present
/// only in config.json is not found; the same name in profiles.yaml is found;
/// --profile-path to an arbitrary YAML file resolves.
#[test]
fn ac7_loader_only_reads_profiles_yaml() {
    let h = TempDir::new().unwrap();
    let wx = h.path().join(".wxctl");
    std::fs::create_dir_all(&wx).unwrap();
    let legacy = "{\"profiles\":{\"legacy\":{\"watsonx_ai\":{\"url\":\"https://x\",\"auth_type\":\"apikey\",\"apikey\":\"K\"}}}}";
    std::fs::write(wx.join("config.json"), legacy).unwrap();

    // Only config.json exists -> `legacy` is not found (config.json is ignored).
    let miss = run(h.path(), &["profile", "show", "legacy"]);
    assert!(!miss.status.success(), "legacy not found when only config.json exists");
    let err = String::from_utf8_lossy(&miss.stderr);
    assert!(err.contains("profiles.yaml") || err.to_lowercase().contains("not found"), "loader targets profiles.yaml: {err}");

    // Same profile in profiles.yaml -> found.
    std::fs::write(wx.join("profiles.yaml"), "profiles:\n  legacy:\n    deployment: saas\n    watsonx_ai:\n      url: \"https://x\"\n      auth_type: apikey\n      apikey: \"K\"\n").unwrap();
    let hit = run(h.path(), &["profile", "show", "legacy"]);
    assert!(hit.status.success(), "legacy found via profiles.yaml: {}", String::from_utf8_lossy(&hit.stderr));
    assert!(String::from_utf8_lossy(&hit.stdout).contains("legacy"), "show prints the profile");

    // --profile-path to an arbitrary YAML resolves.
    let alt = h.path().join("elsewhere.yaml");
    std::fs::write(&alt, "profiles:\n  alt:\n    deployment: saas\n    watsonx_ai:\n      url: \"https://y\"\n      auth_type: apikey\n      apikey: \"K2\"\n").unwrap();
    let byp = run(h.path(), &["profile", "show", "alt", "--profile-path", alt.to_str().unwrap()]);
    assert!(byp.status.success(), "--profile-path resolves: {}", String::from_utf8_lossy(&byp.stderr));
}
