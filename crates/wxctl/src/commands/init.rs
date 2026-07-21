use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, IsTerminal, Read};
use std::path::{Path, PathBuf};

use super::profile::{ServiceStatus, check_profile_services};
use crate::output::color::{Color, Theme};

/// Every auth type the client factory (`wxctl-core`) accepts. Kept in sync with the
/// `get_auth_token_with` match: a value here that the factory rejects — or vice versa —
/// makes `wxctl init` and `wxctl profile validate` disagree with what actually works.
pub const VALID_AUTH_TYPES: &[&str] = &["none", "apikey", "basic", "cp4d", "icp4d", "bearer", "c_api_key", "api_token", "hmac", "zenapikey", "pa_session", "vault_token"];

/// Per-service `(saas_auth, software_auth)`. Sync contract with `VALID_AUTH_TYPES` and
/// `factory.rs::get_auth_token_with` (Invariant I3): every value returned here must be in
/// `VALID_AUTH_TYPES` and be accepted by the factory with the fields `cred_fields` emits.
/// Unmapped / new services fall back to `(apikey, zenapikey)`.
fn auth_for(service: &str) -> (&'static str, &'static str) {
    match service {
        "watsonx_ai" | "watsonx_data" | "watsonx_orchestrate" | "factsheets" | "openscale" | "common_core" => ("apikey", "zenapikey"),
        "cloud_object_storage" => ("hmac", "hmac"),
        "planning_analytics" | "pa_workspace" => ("pa_session", "pa_session"),
        "instana" => ("api_token", "api_token"),
        "concert" | "concert_workflows" => ("c_api_key", "c_api_key"),
        "vault" => ("vault_token", "vault_token"),
        _ => ("apikey", "zenapikey"),
    }
}

/// Credential fields a given `auth_type` needs, in emit order. Mirrors the field reads in
/// `factory.rs::get_auth_token_with` (Invariant I3): apikey/bearer/c_api_key/api_token/
/// pa_session read `apikey`; zenapikey reads `username`+`apikey`; basic/cp4d/icp4d read
/// `username`+`password`; hmac reads `access_key`+`secret_key`; none reads nothing.
fn cred_fields(auth_type: &str) -> &'static [&'static str] {
    match auth_type {
        "apikey" | "bearer" | "c_api_key" | "api_token" | "pa_session" | "vault_token" => &["apikey"],
        "zenapikey" => &["username", "apikey"],
        "basic" | "cp4d" | "icp4d" => &["username", "password"],
        "hmac" => &["access_key", "secret_key"],
        _ => &[],
    }
}

/// SaaS endpoint format-hint (Open Question P2: hints, not resolved defaults). The variable
/// part is bracketed; `wxctl profile validate` catches a wrong host immediately.
fn saas_url_hint(service: &str) -> &'static str {
    match service {
        "watsonx_ai" => "https://<REGION>.ml.cloud.ibm.com",
        "watsonx_data" => "https://<REGION>.lakehouse.cloud.ibm.com",
        "watsonx_orchestrate" => "https://api.<REGION>.watson-orchestrate.cloud.ibm.com",
        "factsheets" | "openscale" | "common_core" => "https://api.dataplatform.cloud.ibm.com",
        "cloud_object_storage" => "https://s3.<REGION>.cloud-object-storage.appdomain.cloud",
        "instana" => "https://<TENANT>-<UNIT>.instana.io",
        "concert" | "concert_workflows" => "https://<CONCERT_HOST>",
        "planning_analytics" | "pa_workspace" => "https://<TENANT>.planning-analytics.cloud.ibm.com",
        "vault" => "https://<VAULT_HOST>:8200",
        _ => "https://<HOST>",
    }
}

/// Software (CP4D / on-prem) endpoint format-hint. watsonx services share the one CP4D
/// cluster host; standalone products render identically to their SaaS hint.
fn software_url_hint(service: &str) -> &'static str {
    match service {
        "watsonx_ai" | "watsonx_data" | "watsonx_orchestrate" | "factsheets" | "openscale" | "common_core" => "https://cpd-cpd.apps.<CLUSTER_DOMAIN>",
        _ => saas_url_hint(service),
    }
}

/// One service block as YAML lines indented under a profile. Emits `url` + `auth_type`, then
/// per credential field BOTH a commented `${env:...}` line and an active
/// `PASTE_YOUR_<FIELD>_HERE` placeholder. Values are double-quoted so a hint containing
/// `<...>` and any placeholder always parse as valid YAML strings.
fn render_service_block(service: &str, auth_type: &str, url: &str) -> Vec<String> {
    let svc_upper = service.to_uppercase();
    let mut lines = vec![format!("    {service}:"), format!("      url: \"{url}\""), format!("      auth_type: {auth_type}")];
    for field in cred_fields(auth_type) {
        let field_upper = field.to_uppercase();
        lines.push(format!("      # {field}: ${{env:WXCTL_{svc_upper}_{field_upper}}}"));
        lines.push(format!("      {field}: \"PASTE_YOUR_{field_upper}_HERE\""));
    }
    lines
}

/// The `#`-comment header block explaining the file, how to fill it, and the next step.
fn scaffold_header(profile_name: &str) -> String {
    let validate_hint = if profile_name == "default" { "wxctl profile validate".to_string() } else { format!("wxctl profile validate {profile_name}") };
    format!(
        "\
# wxctl profiles: credentials for the IBM services wxctl manages.
#
# This is NOT a resource config.yaml (which describes what to deploy). This file
# holds per-profile service endpoints and auth. Keep it private.
#
# Active-profile precedence: -p NAME  >  $WXCTL_PROFILE  >  ~/.wxctl/active_profile  >  \"default\".
#
# Fill each service block one of two ways:
#   1. Replace \"PASTE_YOUR_<FIELD>_HERE\" with the real value, or
#   2. Delete that PASTE line and uncomment the \"# <field>: ${{env:WXCTL_...}}\" line
#      above it to read the value from that environment variable at run time.
#
# This file is written with 0600 permissions (owner read/write only). Keep secrets out of git.
#
# Next step: {validate_hint}
#
"
    )
}

/// Render the commented `profiles.yaml` scaffold: an active `<profile_name>` SaaS profile
/// (real YAML) plus a fully commented `<profile_name>-software` profile (text only), both
/// enumerating `services`.
pub fn render_scaffold(services: &[String], profile_name: &str) -> String {
    let mut out = scaffold_header(profile_name);
    out.push_str("profiles:\n");

    // Active SaaS profile (real YAML).
    out.push_str(&format!("  {profile_name}:\n"));
    out.push_str("    deployment: saas\n");
    for svc in services {
        let (saas_auth, _software_auth) = auth_for(svc);
        for line in render_service_block(svc, saas_auth, saas_url_hint(svc)) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    out.push('\n');

    // Software profile (commented text only; not part of the parsed document).
    out.push_str("# --- Software (CP4D / watsonx on-prem) alternative: uncomment and edit to use ---\n");
    let mut sw = vec![format!("  {profile_name}-software:"), "    deployment: software".to_string()];
    for svc in services {
        let (_saas_auth, software_auth) = auth_for(svc);
        sw.extend(render_service_block(svc, software_auth, software_url_hint(svc)));
    }
    for line in sw {
        out.push_str("# ");
        out.push_str(&line);
        out.push('\n');
    }

    out
}

/// Execute the init command: scaffold a commented `profiles.yaml` (no prompting), then
/// either print the validate next-step or, with `--edit`, open `$EDITOR` and run the
/// advisory per-service live check.
pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, force: bool, edit: bool) -> Result<()> {
    let services = determine_services(config_paths)?;
    let config_file = resolve_config_path(profile_path)?;
    let scaffold = render_scaffold(&services, profile);

    let existing = if config_file.exists() { std::fs::read_to_string(&config_file).map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", config_file.display(), e))? } else { String::new() };

    // Fresh (missing or empty) file: write the full commented scaffold verbatim so the
    // header, both profile variants, and every `${env:}`/PASTE line are preserved.
    if existing.trim().is_empty() {
        crate::config::write_credential_file(&config_file, &scaffold)?;
        return finish(&config_file, profile, edit).await;
    }

    // Existing non-empty file: parse it, honor the overwrite guard.
    let mut root: serde_json::Value = serde_norway::from_str(&existing).map_err(|e| anyhow::anyhow!("Failed to parse '{}': {}", config_file.display(), e))?;
    let already_present = root.get("profiles").and_then(|p| p.as_object()).map(|m| m.contains_key(profile)).unwrap_or(false);
    if already_present && !force {
        println!("Profile \"{}\" already exists in {}. Not overwriting.", profile, config_file.display());
        println!("Edit the file directly, or re-run with --force to replace it with a fresh scaffold.");
        return Ok(());
    }

    // Lift the active SaaS profile out of the scaffold text (the software variant is a
    // comment, so it does not appear in the parse) and insert it, preserving other
    // profiles and `preferences` already in the file.
    let scaffold_doc: serde_json::Value = serde_norway::from_str(&scaffold).map_err(|e| anyhow::anyhow!("internal: scaffold did not parse as YAML: {}", e))?;
    let scaffolded_profile = scaffold_doc.get("profiles").and_then(|p| p.get(profile)).cloned().ok_or_else(|| anyhow::anyhow!("internal: scaffold missing profile '{}'", profile))?;

    let root_obj = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("'{}' root must be a YAML mapping", config_file.display()))?;
    let profiles = root_obj.entry("profiles").or_insert_with(|| serde_json::json!({}));
    let profiles_obj = profiles.as_object_mut().ok_or_else(|| anyhow::anyhow!("\"profiles\" must be a mapping in {}", config_file.display()))?;
    profiles_obj.insert(profile.to_string(), scaffolded_profile);

    let yaml = serde_norway::to_string(&root).map_err(|e| anyhow::anyhow!("Failed to serialize profiles: {}", e))?;
    crate::config::write_credential_file(&config_file, &yaml)?;
    finish(&config_file, profile, edit).await
}

/// Print the written path and the exact next command (`wxctl profile validate [name]`).
fn print_next_step(config_file: &Path, profile: &str) {
    println!("Wrote profile \"{}\" scaffold to {}", profile, config_file.display());
    let suffix = if profile == "default" { String::new() } else { format!(" {profile}") };
    println!("Next: fill in credentials, then run `wxctl profile validate{suffix}`");
}

/// After a successful scaffold write: with `--edit`, open `$VISUAL`/`$EDITOR` and run the
/// advisory per-service live check; otherwise print the path + `wxctl profile validate` step.
async fn finish(config_file: &Path, profile: &str, edit: bool) -> Result<()> {
    if !edit {
        print_next_step(config_file, profile);
        return Ok(());
    }

    // Skip cleanly (print path + hint, exit 0) if no editor is set or stdin is not a TTY.
    // Non-locking TTY check — no stdin lock, no OSC/terminal probe (regression guard for the
    // removed interactive deadlock). Editor spawn inherits the real TTY.
    let Some(editor) = resolve_editor() else {
        print_next_step(config_file, profile);
        return Ok(());
    };
    if !std::io::stdin().is_terminal() {
        print_next_step(config_file, profile);
        return Ok(());
    }

    let theme = Theme::resolve(None);
    let status = spawn_editor(&editor, config_file)?;
    if !status.success() {
        eprintln!("{}", theme.paint(Color::Yellow, &format!("Editor exited with {status} — skipping validation.")));
        print_next_step(config_file, profile);
        return Ok(());
    }

    // Re-read the just-edited profile and run the same live checks as `wxctl profile validate`.
    let root = load_existing_config(&config_file.to_path_buf())?;
    match root.get("profiles").and_then(|p| p.get(profile)) {
        Some(profile_val) => {
            let results = check_profile_services(profile_val).await?;
            print_validation_table(&theme, profile, &results);
        }
        None => {
            eprintln!("{}", theme.paint(Color::Yellow, &format!("Profile \"{profile}\" not found after edit — skipping validation.")));
        }
    }

    // Validation is advisory: init --edit exits 0 even on ✗ (scaffolding succeeded).
    let suffix = if profile == "default" { String::new() } else { format!(" {profile}") };
    println!();
    println!("{}", theme.paint(Color::Dim, &format!("Validation is advisory. Re-run `wxctl profile validate{suffix}` any time.")));
    Ok(())
}

/// `$VISUAL`, then `$EDITOR`; `None` if neither is set to a non-blank value.
fn resolve_editor() -> Option<String> {
    std::env::var("VISUAL").ok().filter(|s| !s.trim().is_empty()).or_else(|| std::env::var("EDITOR").ok().filter(|s| !s.trim().is_empty()))
}

/// Launch the editor on `file`, inheriting the real TTY (default `Command` stdio is inherit —
/// no stdin capture, no probe). Splits the editor command on whitespace so `code --wait` /
/// `emacsclient -c` work.
fn spawn_editor(editor: &str, file: &Path) -> Result<std::process::ExitStatus> {
    let mut parts = editor.split_whitespace();
    let program = parts.next().ok_or_else(|| anyhow::anyhow!("empty editor command"))?;
    let mut cmd = std::process::Command::new(program);
    cmd.args(parts).arg(file);
    cmd.status().map_err(|e| anyhow::anyhow!("Failed to launch editor '{editor}': {e}"))
}

/// Print a compact ✓/✗ line per service from the shared `check_service` statuses.
fn print_validation_table(theme: &Theme, profile: &str, results: &[(String, ServiceStatus)]) {
    println!("\nValidating profile \"{}\":\n", theme.paint(Color::Blue, profile));
    let width = results.iter().map(|(n, _)| n.len()).max().unwrap_or(0);
    for (name, status) in results {
        let (mark, detail) = match status {
            ServiceStatus::Authenticated => (theme.paint(Color::Green, "\u{2713}"), theme.paint(Color::Green, "authenticated")),
            ServiceStatus::Reachable(code) if (200..400).contains(code) => (theme.paint(Color::Green, "\u{2713}"), theme.paint(Color::Green, &format!("reachable (HTTP {code})"))),
            ServiceStatus::Reachable(code) => (theme.paint(Color::Yellow, "\u{2717}"), theme.paint(Color::Yellow, &format!("unexpected status (HTTP {code})"))),
            ServiceStatus::AuthFailed(msg) => (theme.paint(Color::Red, "\u{2717}"), theme.paint(Color::Red, &format!("auth failed ({msg})"))),
            ServiceStatus::Unreachable(msg) => (theme.paint(Color::Red, "\u{2717}"), theme.paint(Color::Red, &format!("unreachable ({msg})"))),
            ServiceStatus::Skipped(note) => (theme.paint(Color::Dim, "-"), theme.paint(Color::Dim, note)),
        };
        println!("  {mark} {name:<width$}  {detail}");
    }
}

/// Determine which services need configuration.
///
/// If config files are provided, parse YAML and extract resource kinds, then map to services.
/// If no config files, return all non-local services from the schema registry.
fn determine_services(config_paths: &[String]) -> Result<Vec<String>> {
    let schemas: Vec<&'static wxctl_schema::ir::SchemaIr> = wxctl_schema::ir::RESOURCE_IR.values().copied().collect();

    if config_paths.is_empty() {
        // No files: collect all unique services, filter out "local"
        let mut services: BTreeSet<String> = BTreeSet::new();
        for schema in &schemas {
            let svc = schema.resource.service;
            if svc != "local" {
                services.insert(svc.to_string());
            }
        }
        return Ok(services.into_iter().collect());
    }

    // Build kind → service mapping
    let mut kind_to_service: BTreeMap<String, String> = BTreeMap::new();
    for schema in &schemas {
        kind_to_service.insert(schema.resource.kind.to_string(), schema.resource.service.to_string());
    }

    // Parse config sources and extract kinds
    let mut needed: BTreeSet<String> = BTreeSet::new();
    for path in config_paths {
        let content = if path == "-" {
            let mut buf = String::new();
            io::stdin().read_to_string(&mut buf).map_err(|e| anyhow::anyhow!("Failed to read from stdin: {}", e))?;
            buf
        } else {
            std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path, e))?
        };

        for doc in content.split("\n---") {
            let doc = doc.trim();
            if doc.is_empty() {
                continue;
            }
            let value: serde_norway::Value = serde_norway::from_str(doc).map_err(|e| anyhow::anyhow!("Failed to parse YAML in '{}': {}", path, e))?;

            if let Some(kind) = value.get("kind").and_then(|v| v.as_str())
                && let Some(service) = kind_to_service.get(kind)
                && service != "local"
            {
                needed.insert(service.clone());
            }
        }
    }

    if needed.is_empty() {
        bail!("No remote services found in the provided config files");
    }

    Ok(needed.into_iter().collect())
}

/// Resolve the config file path.
/// Uses --profile-path if given, otherwise defaults to `<config-dir>/profiles.yaml`.
pub fn resolve_config_path(profile_path: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = profile_path {
        return Ok(PathBuf::from(path));
    }

    Ok(crate::config::wxctl_config_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?.join("profiles.yaml"))
}

/// Load existing config file or return a fresh skeleton.
pub fn load_existing_config(path: &PathBuf) -> Result<serde_json::Value> {
    if path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path.display(), e))?;
        let value: serde_json::Value = serde_norway::from_str(&content).map_err(|e| anyhow::anyhow!("Failed to parse '{}': {}", path.display(), e))?;
        Ok(value)
    } else {
        Ok(serde_json::json!({"profiles": {}}))
    }
}
