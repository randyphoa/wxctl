//! Phase-3 download/verify/install E2E for `wxctl update`. Exercises the host
//! OS's archive format; the install is always routed through
//! WXCTL_UPDATE_INSTALL_PATH to a temp file so the test binary is never replaced
//! (invariant I3). All child env is set per-Command (env-var race:
//! docs/troubleshoot/archive/env-var-test-race-fix.md).
mod common;

use common::{build_release_archive, host_archive_ext, host_binary_member, host_target, run_update, sha256_hex, start_update_server};
use std::collections::HashMap;
use std::path::Path;

/// Strictly greater than the running version (forces "update available").
const NEWER: &str = "99.0.0";
const CUR: &str = env!("CARGO_PKG_VERSION");
const PAYLOAD: &[u8] = b"FAKE-WXCTL-BINARY-PAYLOAD-phase3";

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// `(archive_name, archive_bytes)` for `NEWER` on the host OS.
fn host_archive() -> (String, Vec<u8>) {
    let name = format!("wxctl-v{NEWER}-{}.{}", host_target(), host_archive_ext());
    (name, build_release_archive(host_binary_member(), PAYLOAD))
}

/// `(archive_name, archive_bytes)` for `version` on the host OS.
fn host_archive_for(version: &str) -> (String, Vec<u8>) {
    let name = format!("wxctl-v{version}-{}.{}", host_target(), host_archive_ext());
    (name, build_release_archive(host_binary_member(), PAYLOAD))
}

/// Asset map: the archive + a `SHA256SUMS` whose only line uses `sums_hash`.
fn assets(sums_hash: &str, archive_name: &str, archive: Vec<u8>) -> HashMap<String, Vec<u8>> {
    let mut m = HashMap::new();
    m.insert(archive_name.to_string(), archive);
    m.insert("SHA256SUMS".to_string(), format!("{sums_hash}  {archive_name}\n").into_bytes());
    m
}

fn check_json(latest: &str) -> String {
    format!(r#"{{"latest":"{latest}","news":[],"changelog":[{{"version":"{NEWER}","notes":["Self-update from the CLI","Second note"]}}]}}"#)
}

/// AC 1 + I3 + I4 + AC 9: download -> verify -> extract -> install to the seam path.
#[test]
fn ac1_download_verify_install() {
    let h = home();
    let (name, archive) = host_archive();
    let server = start_update_server(check_json(NEWER), assets(&sha256_hex(&archive), &name, archive));
    let dir = home();
    let install = dir.path().join(host_binary_member());
    let bin = Path::new(env!("CARGO_BIN_EXE_wxctl"));
    let before = std::fs::metadata(bin).unwrap().len();
    let out = run_update(h.path(), &server.check_url(), server.base_url(), &install, &["--yes"]);
    assert!(out.status.success(), "exit 0: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("updated {CUR}")) && stdout.contains(NEWER) && stdout.contains('\u{2192}'), "success line `updated <cur> -> <latest>`: {stdout}");
    assert_eq!(std::fs::read(&install).unwrap(), PAYLOAD, "install path bytes == stub binary (AC 1)");
    assert_eq!(std::fs::metadata(bin).unwrap().len(), before, "the test binary was never replaced (I3)");
    // AC 9: only /check + release-base paths were hit; never a github host.
    for p in server.paths() {
        assert!(p.starts_with("/check") || p.starts_with(&format!("/v{NEWER}/")), "unexpected outbound path: {p}");
        assert!(!p.contains("github"), "no github host contacted: {p}");
    }
}

/// AC 2 + I4: a SHA256SUMS hash that doesn't match the archive aborts before any write.
#[test]
fn ac2_checksum_mismatch_aborts() {
    let h = home();
    let (name, archive) = host_archive();
    let wrong = "0".repeat(64);
    let server = start_update_server(check_json(NEWER), assets(&wrong, &name, archive));
    let dir = home();
    let install = dir.path().join(host_binary_member());
    let out = run_update(h.path(), &server.check_url(), server.base_url(), &install, &["--yes"]);
    assert!(!out.status.success(), "non-zero exit on checksum mismatch");
    assert!(String::from_utf8_lossy(&out.stderr).contains("checksum mismatch"), "checksum-mismatch error");
    assert!(!install.exists(), "nothing written to the install path on abort (I4)");
}

/// AC 3: latest == current -> "already up to date", no release-asset request.
#[test]
fn ac3_already_up_to_date() {
    let h = home();
    let (name, archive) = host_archive();
    let server = start_update_server(check_json(CUR), assets(&sha256_hex(&archive), &name, archive));
    let dir = home();
    let install = dir.path().join(host_binary_member());
    let out = run_update(h.path(), &server.check_url(), server.base_url(), &install, &["--yes"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("already up to date"), "up-to-date message");
    assert_eq!(server.asset_hits(), 0, "AC 3: no release-asset request");
    assert!(!install.exists());
}

/// --force: latest == current still downloads + verifies + reinstalls the
/// current version, and reports a reinstall (not "already up to date").
#[test]
fn force_reinstalls_current_version() {
    let h = home();
    let (name, archive) = host_archive_for(CUR);
    let server = start_update_server(check_json(CUR), assets(&sha256_hex(&archive), &name, archive));
    let dir = home();
    let install = dir.path().join(host_binary_member());
    let out = run_update(h.path(), &server.check_url(), server.base_url(), &install, &["--yes", "--force"]);
    assert!(out.status.success(), "exit 0: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&format!("reinstalled wxctl {CUR}")), "reinstall confirmation: {stdout}");
    assert!(!stdout.contains("already up to date"), "must not short-circuit under --force: {stdout}");
    assert_eq!(std::fs::read(&install).unwrap(), PAYLOAD, "install path bytes == stub binary");
    assert!(server.asset_hits() > 0, "--force downloaded the current-version asset");
}

/// AC 4: --notes prints the changelog notes and makes no download.
#[test]
fn ac4_notes_no_download() {
    let h = home();
    let (name, archive) = host_archive();
    let server = start_update_server(check_json(NEWER), assets(&sha256_hex(&archive), &name, archive));
    let dir = home();
    let install = dir.path().join(host_binary_member());
    let out = run_update(h.path(), &server.check_url(), server.base_url(), &install, &["--notes"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).contains("Self-update from the CLI"), "changelog note printed");
    assert_eq!(server.asset_hits(), 0, "AC 4: --notes makes no download");
    assert!(!install.exists());
}

/// AC 5: WXCTL_DISABLE_UPDATES -> refuse, no /check and no asset request.
#[test]
fn ac5_disabled_no_network() {
    use std::process::{Command, Stdio};
    let h = home();
    let (name, archive) = host_archive();
    let server = start_update_server(check_json(NEWER), assets(&sha256_hex(&archive), &name, archive));
    let dir = home();
    let install = dir.path().join(host_binary_member());
    let out = Command::new(env!("CARGO_BIN_EXE_wxctl"))
        .arg("update")
        .arg("--yes")
        .env("HOME", h.path())
        .env("WXCTL_UPDATE_CACHE_DIR", h.path().join(".wxctl"))
        .env("WXCTL_UPDATE_ENDPOINT", server.check_url())
        .env("WXCTL_RELEASE_BASE_URL", server.base_url())
        .env("WXCTL_UPDATE_INSTALL_PATH", &install)
        .env("WXCTL_DISABLE_UPDATES", "1")
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success(), "non-zero exit when disabled");
    assert!(String::from_utf8_lossy(&out.stderr).contains("disabled"), "disabled message");
    assert_eq!(server.check_hits(), 0, "AC 5: no /check request");
    assert_eq!(server.asset_hits(), 0, "AC 5: no asset request");
}

/// AC 6 (mechanical; message wording is [human]): a read-only install dir ->
/// guidance + non-zero exit WITHOUT downloading.
/// Caveat: under a root UID a 0o555 dir is still writable, so this asserts
/// behavior for the standard non-root CI/dev user.
#[cfg(unix)]
#[test]
fn ac6_non_writable_no_download() {
    use std::os::unix::fs::PermissionsExt;
    let h = home();
    let (name, archive) = host_archive();
    let server = start_update_server(check_json(NEWER), assets(&sha256_hex(&archive), &name, archive));
    let ro = home();
    std::fs::set_permissions(ro.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let install = ro.path().join(host_binary_member());
    let out = run_update(h.path(), &server.check_url(), server.base_url(), &install, &["--yes"]);
    std::fs::set_permissions(ro.path(), std::fs::Permissions::from_mode(0o755)).unwrap(); // re-enable cleanup
    assert!(!out.status.success(), "non-zero exit on a non-writable install dir");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("re-run") || err.contains("sudo") || err.contains("elevated"), "OS-appropriate guidance: {err}");
    assert_eq!(server.asset_hits(), 0, "AC 6: no download attempted");
    assert!(!install.exists(), "no partial binary written (I4)");
}
