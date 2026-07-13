use crate::cli::ProfileCommands;
use crate::config::{active_profile_path, resolve_active_profile};
use crate::output::color::{Color, Theme};
use anyhow::{Result, bail};
use std::collections::BTreeSet;
use wxctl_core::client::TokenManager;

use super::init::{VALID_AUTH_TYPES, load_existing_config, resolve_config_path};

/// Dispatch profile subcommands.
pub async fn execute(command: ProfileCommands, cli_profile: Option<&str>, profile_path: Option<&str>) -> Result<()> {
    match command {
        ProfileCommands::List => list(cli_profile, profile_path),
        ProfileCommands::Show { name } => show(name.as_deref(), cli_profile, profile_path),
        ProfileCommands::Use { name } => use_profile(&name, profile_path),
        ProfileCommands::Validate { name, no_connect } => validate(name.as_deref(), cli_profile, profile_path, no_connect).await,
    }
}

/// Load all profiles as a map from the config file.
fn load_profiles(profile_path: Option<&str>) -> Result<(std::path::PathBuf, serde_json::Map<String, serde_json::Value>)> {
    let config_file = resolve_config_path(profile_path)?;
    if !config_file.exists() {
        let theme = Theme::resolve(None);
        bail!("Config file not found: {}\nRun {} to create one.", config_file.display(), theme.paint(Color::Blue, "wxctl init"));
    }
    let root = load_existing_config(&config_file)?;
    let profiles = root.get("profiles").and_then(|v| v.as_object()).ok_or_else(|| anyhow::anyhow!("No \"profiles\" key in {}", config_file.display()))?.clone();
    Ok((config_file, profiles))
}

// ── list ─────────────────────────────────────────────────────────

fn list(cli_profile: Option<&str>, profile_path: Option<&str>) -> Result<()> {
    let (config_file, profiles) = load_profiles(profile_path)?;
    let active = resolve_active_profile(cli_profile);
    let theme = Theme::resolve(None);

    if profiles.is_empty() {
        println!("No profiles configured in {}", config_file.display());
        println!("Run {} to create one.", theme.paint(Color::Blue, "wxctl init"));
        return Ok(());
    }

    let mut names: Vec<&String> = profiles.keys().collect();
    names.sort();

    let max_name = names.iter().map(|n| n.len()).max().unwrap_or(4).max(4);

    println!("\n  {:<width$}  {:>8}  AUTH TYPES", "NAME", "SERVICES", width = max_name);

    for name in &names {
        let marker = if **name == active { "*" } else { " " };
        let profile_val = &profiles[*name];

        let (service_count, auth_types) = if let Some(obj) = profile_val.as_object() {
            let count = obj.len();
            let types: BTreeSet<&str> = obj.values().filter_map(|v| v.get("auth_type").and_then(|a| a.as_str())).collect();
            let types_str: Vec<&str> = types.into_iter().collect();
            (count, types_str.join(", "))
        } else {
            (0, String::new())
        };

        let display_name = if **name == active { theme.paint(Color::Green, name) } else { name.to_string() };

        // Pad the raw name, then replace with colored version for display
        let padded = format!("{:<width$}", name, width = max_name);
        let display_padded = if **name == active { padded.replace(name.as_str(), &display_name) } else { padded };

        println!("{} {}  {:>8}  {}", marker, display_padded, service_count, auth_types);
    }

    println!();
    Ok(())
}

// ── show ─────────────────────────────────────────────────────────

fn show(name: Option<&str>, cli_profile: Option<&str>, profile_path: Option<&str>) -> Result<()> {
    let (_config_file, profiles) = load_profiles(profile_path)?;
    let active = resolve_active_profile(cli_profile);
    let target = name.unwrap_or(&active);
    let theme = Theme::resolve(None);

    let profile_val = profiles.get(target).ok_or_else(|| anyhow::anyhow!("Profile '{}' not found", target))?;

    let services = profile_val.as_object().ok_or_else(|| anyhow::anyhow!("Profile '{}' is not a valid object", target))?;

    println!("\nProfile: {}\n", theme.paint(Color::Green, target));

    let max_svc = services.keys().map(|k| k.len()).max().unwrap_or(7).max(7);
    let max_url = services.values().filter_map(|v| v.get("url").and_then(|u| u.as_str())).map(|u| u.len()).max().unwrap_or(3).max(3);

    println!("  {:<svc_w$}  {:<url_w$}  {:>9}  CREDENTIALS", "SERVICE", "URL", "AUTH TYPE", svc_w = max_svc, url_w = max_url);

    let mut sorted: Vec<_> = services.iter().collect();
    sorted.sort_by_key(|(k, _)| k.to_string());

    for (svc_name, svc_val) in &sorted {
        let url = svc_val.get("url").and_then(|v| v.as_str()).unwrap_or("-");
        let auth_type = svc_val.get("auth_type").and_then(|v| v.as_str()).unwrap_or("-");
        let creds = check_credentials(&theme, svc_name, auth_type, svc_val);

        println!("  {:<svc_w$}  {:<url_w$}  {:>9}  {}", svc_name, url, auth_type, creds, svc_w = max_svc, url_w = max_url);
    }

    println!();
    Ok(())
}

/// Check if credentials are available in the profile config or environment variables.
fn check_credentials(theme: &Theme, service: &str, auth_type: &str, svc_val: &serde_json::Value) -> String {
    // An unresolved `${env:VAR}` literal (lenient interpolation leaves it in place) is not a credential.
    let has_field = |field: &str| -> bool { svc_val.get(field).and_then(|v| v.as_str()).map(|s| !s.is_empty() && !s.contains("${env:")).unwrap_or(false) };

    // Report missing config fields as a comma-joined `"<a>, <b> missing"` string.
    let missing = |fields: &[&str]| -> String {
        let miss: Vec<&str> = fields.iter().copied().filter(|f| !has_field(f)).collect();
        format!("{} missing", miss.join(", "))
    };

    match auth_type {
        "none" => theme.paint(Color::Dim, "n/a"),
        // Single-token schemes: the factory reads all of these from `apikey`.
        "apikey" | "bearer" | "c_api_key" | "api_token" => {
            if has_field("apikey") {
                theme.paint(Color::Green, "apikey set (config)")
            } else {
                let env_var = format!("{}_APIKEY", service.to_uppercase());
                if std::env::var(&env_var).is_ok() { theme.paint(Color::Green, &format!("{} set", env_var)) } else { theme.paint(Color::Yellow, &format!("{} missing", env_var)) }
            }
        }
        "zenapikey" => {
            if has_field("username") && has_field("apikey") {
                theme.paint(Color::Green, "username, apikey set (config)")
            } else {
                theme.paint(Color::Yellow, &missing(&["username", "apikey"]))
            }
        }
        "hmac" => {
            if has_field("access_key") && has_field("secret_key") {
                theme.paint(Color::Green, "access_key, secret_key set (config)")
            } else {
                theme.paint(Color::Yellow, &missing(&["access_key", "secret_key"]))
            }
        }
        "pa_session" => {
            if has_field("apikey") {
                theme.paint(Color::Green, "apikey (paSession) set (config)")
            } else if has_field("username") && has_field("password") {
                theme.paint(Color::Green, "username, password set (config)")
            } else {
                theme.paint(Color::Yellow, "apikey or username+password missing")
            }
        }
        "basic" | "cp4d" | "icp4d" => {
            let config_user = has_field("username");
            let config_pass = has_field("password");
            if config_user && config_pass {
                theme.paint(Color::Green, "username, password set (config)")
            } else {
                let user_var = format!("{}_USERNAME", service.to_uppercase());
                let pass_var = format!("{}_PASSWORD", service.to_uppercase());
                let user_ok = config_user || std::env::var(&user_var).is_ok();
                let pass_ok = config_pass || std::env::var(&pass_var).is_ok();
                if user_ok && pass_ok {
                    theme.paint(Color::Green, &format!("{}, {} set", user_var, pass_var))
                } else {
                    let mut missing = vec![];
                    if !user_ok {
                        missing.push(user_var);
                    }
                    if !pass_ok {
                        missing.push(pass_var);
                    }
                    theme.paint(Color::Yellow, &format!("{} missing", missing.join(", ")))
                }
            }
        }
        _ => theme.paint(Color::Yellow, "unknown auth type"),
    }
}

// ── use ──────────────────────────────────────────────────────────

fn use_profile(name: &str, profile_path: Option<&str>) -> Result<()> {
    let (config_file, profiles) = load_profiles(profile_path)?;
    if !profiles.contains_key(name) {
        let mut available: Vec<&String> = profiles.keys().collect();
        available.sort();
        bail!("Profile '{}' not found in {}.\nAvailable profiles: {}", name, config_file.display(), available.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "));
    }

    let active_file = active_profile_path()?;
    crate::config::write_credential_file(&active_file, name)?;

    let theme = Theme::resolve(None);
    println!("Active profile set to {}", theme.paint(Color::Green, name));
    println!("{}", theme.paint(Color::Dim, &format!("Saved to {}", active_file.display())));

    Ok(())
}

// ── validate ─────────────────────────────────────────────────────

async fn validate(name: Option<&str>, cli_profile: Option<&str>, profile_path: Option<&str>, no_connect: bool) -> Result<()> {
    let (_config_file, profiles) = load_profiles(profile_path)?;
    let active = resolve_active_profile(cli_profile);
    let target = name.unwrap_or(&active);
    let theme = Theme::resolve(None);

    let profile_val = profiles.get(target).ok_or_else(|| anyhow::anyhow!("Profile '{}' not found", target))?;

    // Resolve `${env:VAR}` the same way the client factory does before its checks —
    // dev profiles are fully env-wired, so validating the raw file only ever sees
    // unparseable literals. Unset vars are collected and reported per field instead
    // of failing the whole command (the literal is left in place as the marker).
    let unresolved = LenientEnv::default();
    let mut yaml_val = serde_norway::to_value(profile_val)?;
    wxctl_core::interpolation::interpolate(&mut yaml_val, &unresolved)?;
    let resolved: serde_json::Value = serde_norway::from_value(yaml_val)?;

    let services = resolved.as_object().ok_or_else(|| anyhow::anyhow!("Profile '{}' is not a valid object", target))?;

    println!("\nValidating profile: {}\n", theme.paint(Color::Blue, target));

    let mut errors: Vec<String> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();

    let mut sorted: Vec<_> = services.iter().collect();
    sorted.sort_by_key(|(k, _)| k.to_string());

    for (svc_name, svc_val) in &sorted {
        // Profile-level scalars (e.g. `deployment: software-5.3.0`) are settings, not
        // service blocks — validating them as services fabricates missing-URL errors.
        if !svc_val.is_object() {
            println!("  {}", theme.paint(Color::Dim, &format!("[{}] profile setting — not a service, skipped", svc_name)));
            println!();
            continue;
        }
        println!("  {}", theme.paint(Color::Blue, &format!("[{}]", svc_name)));

        // Check URL
        let url = svc_val.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            errors.push(format!("{}: URL is missing", svc_name));
            println!("    URL:  {}", theme.paint(Color::Red, "missing"));
        } else if url.contains("${env:") {
            errors.push(format!("{}: URL references unset env var(s): {}", svc_name, url));
            println!("    URL:  {}", theme.paint(Color::Red, &format!("{} (unset env var)", url)));
        } else {
            match url::Url::parse(url) {
                Ok(parsed) => {
                    if parsed.scheme() != "https" && parsed.scheme() != "http" {
                        errors.push(format!("{}: URL has unsupported scheme '{}'", svc_name, parsed.scheme()));
                        println!("    URL:  {} (unsupported scheme)", theme.paint(Color::Red, url));
                    } else {
                        println!("    URL:  {}", theme.paint(Color::Green, url));
                    }
                }
                Err(e) => {
                    errors.push(format!("{}: malformed URL: {}", svc_name, e));
                    println!("    URL:  {} ({})", theme.paint(Color::Red, url), e);
                }
            }
        }

        // Check auth_type
        let auth_type = svc_val.get("auth_type").and_then(|v| v.as_str()).unwrap_or("");
        if auth_type.is_empty() {
            errors.push(format!("{}: auth_type is missing", svc_name));
            println!("    Auth: {}", theme.paint(Color::Red, "missing"));
        } else if !VALID_AUTH_TYPES.contains(&auth_type) {
            errors.push(format!("{}: invalid auth_type '{}' (valid: {})", svc_name, auth_type, VALID_AUTH_TYPES.join(", ")));
            println!("    Auth: {} (invalid)", theme.paint(Color::Red, auth_type));
        } else {
            println!("    Auth: {}", theme.paint(Color::Green, auth_type));
        }

        // Check credentials
        if auth_type != "none" && !auth_type.is_empty() {
            let has_config_field = |field: &str| -> bool { svc_val.get(field).and_then(|v| v.as_str()).map(|s| !s.is_empty() && !s.contains("${env:")).unwrap_or(false) };
            let has_creds = match auth_type {
                "apikey" | "bearer" | "c_api_key" | "api_token" => has_config_field("apikey") || std::env::var(format!("{}_APIKEY", svc_name.to_uppercase())).is_ok(),
                "zenapikey" => has_config_field("username") && has_config_field("apikey"),
                "hmac" => has_config_field("access_key") && has_config_field("secret_key"),
                "pa_session" => has_config_field("apikey") || (has_config_field("username") && has_config_field("password")),
                "basic" | "cp4d" | "icp4d" => (has_config_field("username") || std::env::var(format!("{}_USERNAME", svc_name.to_uppercase())).is_ok()) && (has_config_field("password") || std::env::var(format!("{}_PASSWORD", svc_name.to_uppercase())).is_ok()),
                _ => false,
            };
            let cred_status = check_credentials(&theme, svc_name, auth_type, svc_val);
            if !has_creds {
                warnings.push(format!("{}: credentials not found", svc_name));
            }
            println!("    Cred: {}", cred_status);
        } else if auth_type == "none" {
            println!("    Cred: {}", theme.paint(Color::Dim, "n/a"));
        }

        // Connectivity + authentication check
        if !no_connect && !url.is_empty() && !url.contains("${env:") && url::Url::parse(url).is_ok() {
            match check_service(url, auth_type, svc_val).await {
                ServiceStatus::Authenticated => {
                    println!("    Conn: {}", theme.paint(Color::Green, "authenticated"),);
                }
                ServiceStatus::Reachable(code) => {
                    if (200..400).contains(&code) {
                        println!("    Conn: {} (HTTP {})", theme.paint(Color::Green, "reachable"), code,);
                    } else {
                        warnings.push(format!("{}: unexpected HTTP status {}", svc_name, code,));
                        println!("    Conn: {} (HTTP {})", theme.paint(Color::Yellow, "unexpected status"), code,);
                    }
                }
                ServiceStatus::AuthFailed(msg) => {
                    warnings.push(format!("{}: authentication failed: {}", svc_name, msg,));
                    println!("    Conn: {}", theme.paint(Color::Red, &format!("auth failed ({})", msg)),);
                }
                ServiceStatus::Unreachable(msg) => {
                    errors.push(format!("{}: connection failed: {}", svc_name, msg));
                    println!("    Conn: {}", theme.paint(Color::Red, &format!("unreachable ({})", msg)),);
                }
                ServiceStatus::Skipped(note) => {
                    println!("    Conn: {}", theme.paint(Color::Dim, &note));
                }
            }
        } else if no_connect {
            println!("    Conn: {}", theme.paint(Color::Dim, "skipped"));
        }

        println!();
    }

    let missing = unresolved.missing.into_inner();
    if !missing.is_empty() {
        warnings.push(format!("unset env var(s) referenced by this profile: {} (export them or source your profile env first)", missing.iter().cloned().collect::<Vec<_>>().join(", ")));
    }

    // Summary
    if errors.is_empty() && warnings.is_empty() {
        println!("{}", theme.paint(Color::Green, &format!("Profile '{}' is valid ({} services configured)", target, services.values().filter(|v| v.is_object()).count())));
    } else {
        if !warnings.is_empty() {
            println!("{}", theme.paint(Color::Yellow, "Warnings:"));
            for w in &warnings {
                println!("  - {}", w);
            }
        }
        if !errors.is_empty() {
            println!("{}", theme.paint(Color::Red, "Errors:"));
            for e in &errors {
                println!("  - {}", e);
            }
            bail!("Profile '{}' validation failed with {} error(s)", target, errors.len());
        }
    }

    Ok(())
}

/// EnvReader that never fails: present vars resolve normally; unset/empty vars are
/// recorded and the `${env:VAR}` literal is left in place so the per-field checks
/// can flag exactly which values are unusable.
#[derive(Default)]
struct LenientEnv {
    missing: std::cell::RefCell<BTreeSet<String>>,
}

impl wxctl_core::interpolation::EnvReader for LenientEnv {
    fn get(&self, var: &str) -> Option<String> {
        match std::env::var(var).ok().filter(|v| !v.is_empty()) {
            Some(v) => Some(v),
            None => {
                self.missing.borrow_mut().insert(var.to_string());
                Some(format!("${{env:{var}}}"))
            }
        }
    }
}

/// Outcome of a connectivity + credential check.
pub(crate) enum ServiceStatus {
    /// Token exchange succeeded (apikey, cp4d, icp4d).
    Authenticated,
    /// Service responded to a direct HTTP request (basic, none).
    Reachable(u16),
    /// Credentials were rejected.
    AuthFailed(String),
    /// Could not reach the service or auth endpoint at all.
    Unreachable(String),
    /// Connectivity check intentionally skipped with an explanatory note (e.g. hmac,
    /// whose SigV4 request signing is exercised at apply time, not by a bearer GET).
    Skipped(String),
}

/// Live-check every service block in a profile value, mirroring `validate`'s connectivity
/// pass: resolve `${env:}` leniently (unset vars are left as literals), guard the URL, then
/// run `check_service` per service. Non-object entries (profile-level settings like
/// `deployment`) are skipped. Returns `(service_name, status)` sorted by name. Shared with
/// `wxctl init --edit`'s advisory post-edit check.
pub(crate) async fn check_profile_services(profile_val: &serde_json::Value) -> Result<Vec<(String, ServiceStatus)>> {
    let unresolved = LenientEnv::default();
    let mut yaml_val = serde_norway::to_value(profile_val)?;
    wxctl_core::interpolation::interpolate(&mut yaml_val, &unresolved)?;
    let resolved: serde_json::Value = serde_norway::from_value(yaml_val)?;
    let services = resolved.as_object().ok_or_else(|| anyhow::anyhow!("profile is not a mapping"))?;

    let mut sorted: Vec<_> = services.iter().collect();
    sorted.sort_by_key(|(k, _)| k.to_string());

    let mut out = Vec::new();
    for (svc_name, svc_val) in sorted {
        // Profile-level scalars (e.g. `deployment: saas`) are settings, not service blocks.
        if !svc_val.is_object() {
            continue;
        }
        let url = svc_val.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let auth_type = svc_val.get("auth_type").and_then(|v| v.as_str()).unwrap_or("");
        // No reachable URL (empty, unresolved `${env:}`, or an unedited `<...>` placeholder)
        // → skip the network call, same guard `validate` applies before `check_service`.
        let status = if url.is_empty() || url.contains("${env:") || url::Url::parse(url).is_err() { ServiceStatus::Skipped("no reachable URL (fill in the scaffold placeholder)".into()) } else { check_service(url, auth_type, svc_val).await };
        out.push((svc_name.clone(), status));
    }
    Ok(out)
}

/// Verify the service using the same auth flow as plan/apply.
async fn check_service(url: &str, auth_type: &str, svc_val: &serde_json::Value) -> ServiceStatus {
    let client = match reqwest::Client::builder().timeout(std::time::Duration::from_secs(10)).build() {
        Ok(c) => c,
        Err(e) => return ServiceStatus::Unreachable(e.to_string()),
    };

    let get_field = |field: &str| -> Option<String> { svc_val.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty() && !s.contains("${env:")).map(|s| s.to_string()) };

    match auth_type {
        "apikey" => {
            let Some(apikey) = get_field("apikey") else {
                return ServiceStatus::AuthFailed("apikey not found in config".into());
            };
            let tm = TokenManager::new(apikey, "apikey".into());
            match tm.get_token(&client).await {
                Ok(_) => ServiceStatus::Authenticated,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("authentication failed") { ServiceStatus::AuthFailed(msg) } else { ServiceStatus::Unreachable(msg) }
                }
            }
        }
        "cp4d" | "icp4d" => {
            let Some(username) = get_field("username") else {
                return ServiceStatus::AuthFailed("username not found in config".into());
            };
            let Some(password) = get_field("password") else {
                return ServiceStatus::AuthFailed("password not found in config".into());
            };
            let auth_token = format!("{}:{}", username, password);
            let tm = TokenManager::with_base_url(auth_token, auth_type.into(), url.into());
            match tm.get_token(&client).await {
                Ok(_) => ServiceStatus::Authenticated,
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("authentication failed") { ServiceStatus::AuthFailed(msg) } else { ServiceStatus::Unreachable(msg) }
                }
            }
        }
        "basic" => {
            let Some(username) = get_field("username") else {
                return ServiceStatus::AuthFailed("username not found in config".into());
            };
            let Some(password) = get_field("password") else {
                return ServiceStatus::AuthFailed("password not found in config".into());
            };
            match client.get(url).basic_auth(&username, Some(&password)).send().await {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if code == 401 { ServiceStatus::AuthFailed(format!("HTTP {}", code)) } else { ServiceStatus::Reachable(code) }
                }
                Err(e) => ServiceStatus::Unreachable(e.to_string()),
            }
        }
        "zenapikey" => {
            // ZenApiKey token construction is offline: base64(username:apikey), no token
            // exchange (see TokenManager's zenapikey arm). Build it, then send a real GET
            // with the `Authorization: ZenApiKey <token>` header the client would use.
            let Some(username) = get_field("username") else {
                return ServiceStatus::AuthFailed("username not found in config".into());
            };
            let Some(apikey) = get_field("apikey") else {
                return ServiceStatus::AuthFailed("apikey not found in config".into());
            };
            let tm = TokenManager::new(format!("{}:{}", username, apikey), "zenapikey".into());
            let token = match tm.get_token(&client).await {
                Ok(t) => t,
                Err(e) => return ServiceStatus::Unreachable(e.to_string()),
            };
            match client.get(url).header("Authorization", format!("ZenApiKey {}", token)).send().await {
                Ok(resp) => {
                    let code = resp.status().as_u16();
                    if code == 401 { ServiceStatus::AuthFailed(format!("HTTP {}", code)) } else { ServiceStatus::Reachable(code) }
                }
                Err(e) => ServiceStatus::Unreachable(e.to_string()),
            }
        }
        "hmac" => {
            // COS/S3 SigV4: the CosClient signs each request itself; there is no bearer
            // token or auth endpoint to probe, so a generic GET would send a nonsense
            // header. Skip with a note — the signing path is exercised at apply time.
            ServiceStatus::Skipped("hmac endpoints verified at apply time".into())
        }
        _ => {
            // "none" or unknown – bare GET
            match client.get(url).send().await {
                Ok(resp) => ServiceStatus::Reachable(resp.status().as_u16()),
                Err(e) => ServiceStatus::Unreachable(e.to_string()),
            }
        }
    }
}
