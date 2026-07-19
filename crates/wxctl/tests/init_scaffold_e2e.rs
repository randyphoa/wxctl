//! Final-phase black-box E2E for `wxctl init` scaffold contents.
//! Drives the freshly-built binary; asserts on the written profiles.yaml text
//! and on stdout. No YAML dep: the CLI (`profile show`) is the parse oracle.
//! Covers AC 1, 2, 3, 4, 5, 9, 11 and the I3 auth-type-subset check.
use std::path::Path;
use std::process::{Command, Output, Stdio};
use tempfile::TempDir;

/// Run `wxctl <args>` with a temp HOME, no update check, stdin closed.
fn wxctl(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wxctl"))
        .args(args)
        .arg("--no-update-check")
        .env("HOME", home)
        .env("WXCTL_CONFIG_DIR", home.join(".wxctl"))
        .env("WXCTL_UPDATE_CACHE_DIR", home.join(".wxctl"))
        .env_remove("CI")
        .env_remove("GITHUB_ACTIONS")
        .env_remove("VISUAL")
        .env_remove("EDITOR")
        .stdin(Stdio::null())
        .output()
        .expect("spawn wxctl")
}

fn write(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
}

/// The current non-local service catalog the no-`-f` scaffold enumerates.
/// If a new remote service is added, update this list (and the init auth map).
const EXPECTED_SERVICES: &[&str] = &["cloud_object_storage", "common_core", "concert", "concert_workflows", "factsheets", "instana", "openscale", "pa_workspace", "planning_analytics", "vault", "watsonx_ai", "watsonx_data", "watsonx_orchestrate"];

/// AC 1: no `-f` -> valid YAML, active `default` (deployment: saas) enumerating
/// every non-local service in SaaS shape; `default-software` present only as
/// commented text (absent from the parsed document).
#[test]
fn ac1_full_scaffold_default_saas_software_commented() {
    let h = TempDir::new().unwrap();
    let f = h.path().join("p.yaml");
    let out = wxctl(h.path(), &["init", "--profile-path", f.to_str().unwrap()]);
    assert!(out.status.success(), "init exit 0: {}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&f).unwrap();

    assert!(text.contains("profiles:") && text.contains("  default:") && text.contains("    deployment: saas"), "active default saas profile");

    // Slice at the software marker: head = active profile, tail = commented software.
    // Use `find` + slice, not `split_once` (which drops the delimiter and would leave
    // the marker line's trailing text at the head of `tail` without its leading `#`).
    let idx = text.find("# --- Software").expect("software marker present");
    let (head, tail) = (&text[..idx], &text[idx..]);
    for svc in EXPECTED_SERVICES {
        assert!(head.contains(&format!("    {svc}:")), "active default enumerates {svc}");
    }
    // Exactly the catalog's services in the active block (one auth_type per service).
    let active_blocks = head.matches("\n      auth_type:").count();
    assert_eq!(active_blocks, EXPECTED_SERVICES.len(), "active profile has one block per non-local service");
    // Software profile is comment-only: every non-blank tail line is a comment.
    for line in tail.lines().filter(|l| !l.trim().is_empty()) {
        assert!(line.trim_start().starts_with('#'), "software line is commented: {line:?}");
    }
    assert!(tail.contains("default-software"), "software profile name appears (commented)");

    // Parse oracle: default resolves, default-software does not (it is a comment).
    let show_default = wxctl(h.path(), &["profile", "show", "default", "--profile-path", f.to_str().unwrap()]);
    assert!(show_default.status.success(), "profile show default: parses + found");
    let show_sw = wxctl(h.path(), &["profile", "show", "default-software", "--profile-path", f.to_str().unwrap()]);
    assert!(!show_sw.status.success(), "default-software not in the parsed document");
}

/// AC 2 + AC 3 (instana=api_token, cos=hmac): `-f` narrows to exactly the config's
/// two services, in both the active and commented-software variants.
#[test]
fn ac2_ac3_dash_f_narrows_to_referenced_services() {
    let h = TempDir::new().unwrap();
    let cfg = h.path().join("cfg.yaml");
    write(&cfg, "kind: instana_alerting_channel\nref_name: a\nname: a\n---\nkind: s3_bucket\nref_name: b\nname: b\n");
    let f = h.path().join("p.yaml");
    let out = wxctl(h.path(), &["init", "-f", cfg.to_str().unwrap(), "--profile-path", f.to_str().unwrap()]);
    assert!(out.status.success(), "init -f exit 0: {}", String::from_utf8_lossy(&out.stderr));
    let text = std::fs::read_to_string(&f).unwrap();
    let (head, _tail) = text.split_once("# --- Software").unwrap();

    assert!(head.contains("    instana:"), "instana block present");
    assert!(head.contains("    cloud_object_storage:"), "cos block present");
    // Only these two services -> only two auth_type lines in the active profile.
    assert_eq!(head.matches("\n      auth_type:").count(), 2, "exactly two service blocks");
    for absent in ["watsonx_ai:", "watsonx_orchestrate:", "concert:"] {
        assert!(!head.contains(absent), "{absent} not scaffolded");
    }
    // AC 3: per-service auth_type from the auth map.
    assert!(head.contains("    instana:\n      url:") && head.contains("      auth_type: api_token"), "instana -> api_token");
    assert!(head.contains("    cloud_object_storage:\n      url:") && head.contains("      auth_type: hmac"), "cloud_object_storage -> hmac");
}

/// AC 3 (watsonx apikey/zenapikey) + AC 4 (dual env + PASTE lines) + I3 (every
/// emitted auth_type is a known valid type): a watsonx + cos + concert scaffold.
#[test]
fn ac3_ac4_i3_auth_types_and_dual_cred_lines() {
    let h = TempDir::new().unwrap();
    let cfg = h.path().join("cfg.yaml");
    write(&cfg, "kind: agent\nref_name: a\nname: a\n---\nkind: s3_bucket\nref_name: b\nname: b\n---\nkind: concert_application\nref_name: c\nname: c\n");
    let f = h.path().join("p.yaml");
    let out = wxctl(h.path(), &["init", "-f", cfg.to_str().unwrap(), "--profile-path", f.to_str().unwrap()]);
    assert!(out.status.success());
    let text = std::fs::read_to_string(&f).unwrap();
    let (head, tail) = text.split_once("# --- Software").unwrap();

    // AC 3: watsonx_orchestrate is apikey in the active (saas) profile, zenapikey in software.
    assert!(head.contains("    watsonx_orchestrate:") && head.contains("      auth_type: apikey"), "wxo saas -> apikey");
    assert!(tail.contains("#       auth_type: zenapikey"), "wxo software -> zenapikey (commented)");
    // concert -> c_api_key.
    assert!(head.contains("    concert:") && head.contains("      auth_type: c_api_key"), "concert -> c_api_key");

    // AC 4: every credential field has BOTH a commented ${env:...} line and an active PASTE line.
    assert!(head.contains("      # apikey: ${env:WXCTL_WATSONX_ORCHESTRATE_APIKEY}"), "wxo env line");
    assert!(head.contains("      apikey: \"PASTE_YOUR_APIKEY_HERE\""), "wxo PASTE line");
    assert!(head.contains("      # access_key: ${env:WXCTL_CLOUD_OBJECT_STORAGE_ACCESS_KEY}"), "cos access_key env line");
    assert!(head.contains("      access_key: \"PASTE_YOUR_ACCESS_KEY_HERE\""), "cos access_key PASTE line");
    assert!(head.contains("      secret_key: \"PASTE_YOUR_SECRET_KEY_HERE\""), "cos secret_key PASTE line");

    // I3 subset: every auth_type the scaffold emits (active + commented) is a known valid type.
    const EMITTED_OK: &[&str] = &["apikey", "zenapikey", "hmac", "pa_session", "api_token", "c_api_key", "basic", "cp4d", "icp4d", "bearer", "none"];
    for line in text.lines() {
        if let Some((_, v)) = line.split_once("auth_type:") {
            let v = v.trim();
            assert!(EMITTED_OK.contains(&v), "auth_type {v:?} is a known valid type");
        }
    }
}

/// AC 5 (Unix): scaffolded file is 0600, freshly created ~/.wxctl is 0700.
#[cfg(unix)]
#[test]
fn ac5_permissions_0600_file_0700_dir() {
    use std::os::unix::fs::PermissionsExt;
    let h = TempDir::new().unwrap();
    // No --profile-path -> writes <HOME>/.wxctl/profiles.yaml, creating .wxctl.
    let out = wxctl(h.path(), &["init"]);
    assert!(out.status.success(), "init exit 0: {}", String::from_utf8_lossy(&out.stderr));
    let dir = h.path().join(".wxctl");
    let file = dir.join("profiles.yaml");
    assert!(file.exists(), "profiles.yaml written under HOME/.wxctl");
    assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600, "file 0600");
    assert_eq!(std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777, 0o700, "dir 0700");
}

/// AC 9: without --edit, init prints the written path and the exact next command,
/// with the profile name only when it is not `default`.
#[test]
fn ac9_next_step_output() {
    let h = TempDir::new().unwrap();
    let f = h.path().join("p.yaml");
    let out = wxctl(h.path(), &["init", "--profile-path", f.to_str().unwrap()]);
    let so = String::from_utf8_lossy(&out.stdout);
    assert!(so.contains("Wrote profile \"default\" scaffold to"), "path line: {so}");
    assert!(so.contains("wxctl profile validate") && !so.contains("wxctl profile validate default"), "next step names no profile for default: {so}");

    let f2 = h.path().join("p2.yaml");
    let out2 = wxctl(h.path(), &["init", "-p", "staging", "--profile-path", f2.to_str().unwrap()]);
    let so2 = String::from_utf8_lossy(&out2.stdout);
    assert!(so2.contains("Wrote profile \"staging\" scaffold to"), "named path line: {so2}");
    assert!(so2.contains("wxctl profile validate staging"), "next step names staging: {so2}");
}

/// AC 11: a second init against an existing profile does not overwrite (exit 0 +
/// notice); --force re-scaffolds that profile to placeholders and preserves
/// other profiles and `preferences`.
#[test]
fn ac11_overwrite_guard_and_force() {
    let h = TempDir::new().unwrap();
    let f = h.path().join("p.yaml");
    // Seed a file with a real value in `default`, plus a `keep` profile and `preferences`.
    write(
        &f,
        "preferences:\n  color: always\nprofiles:\n  default:\n    deployment: saas\n    watsonx_ai:\n      url: \"https://real.example.com\"\n      auth_type: apikey\n      apikey: \"REALSECRET\"\n  keep:\n    deployment: saas\n    instana:\n      url: \"https://x.instana.io\"\n      auth_type: api_token\n      apikey: \"KEEPME\"\n",
    );
    let before = std::fs::read_to_string(&f).unwrap();

    // Guard: no --force -> notice, exit 0, file byte-identical.
    let guarded = wxctl(h.path(), &["init", "--profile-path", f.to_str().unwrap()]);
    assert!(guarded.status.success(), "guard exits 0");
    let so = String::from_utf8_lossy(&guarded.stdout);
    assert!(so.contains("already exists") && so.contains("--force"), "guard notice: {so}");
    assert_eq!(std::fs::read_to_string(&f).unwrap(), before, "file untouched without --force");

    // --force: default re-scaffolded to placeholders; keep + preferences preserved.
    let forced = wxctl(h.path(), &["init", "--force", "--profile-path", f.to_str().unwrap()]);
    assert!(forced.status.success(), "force exits 0: {}", String::from_utf8_lossy(&forced.stderr));
    let after = std::fs::read_to_string(&f).unwrap();
    assert!(after.contains("PASTE_YOUR"), "default re-scaffolded to placeholders");
    assert!(!after.contains("REALSECRET"), "old default secret replaced");
    assert!(after.contains("KEEPME") && after.contains("keep"), "other profile preserved");
    assert!(after.contains("preferences") && after.contains("color"), "preferences preserved");
    // `keep` still resolvable through the loader.
    assert!(wxctl(h.path(), &["profile", "show", "keep", "--profile-path", f.to_str().unwrap()]).status.success(), "keep profile still valid");
}
