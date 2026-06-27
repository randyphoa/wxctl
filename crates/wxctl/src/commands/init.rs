use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, BufRead, Read, Write};
use std::path::PathBuf;

pub const VALID_AUTH_TYPES: &[&str] = &["none", "apikey", "basic", "cp4d", "icp4d"];

/// Execute the init command: set up a profile with service URLs and auth settings.
pub fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, template: bool) -> Result<()> {
    let services = determine_services(config_paths)?;

    if template {
        return write_template(&services, profile, profile_path);
    }

    let config_file = resolve_config_path(profile_path)?;
    let mut root = load_existing_config(&config_file)?;

    let profiles = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("Config file root must be a JSON object, not an array or primitive"))?.entry("profiles").or_insert_with(|| serde_json::json!({}));

    let profile_obj = profiles.as_object_mut().ok_or_else(|| anyhow::anyhow!("\"profiles\" must be an object in {}", config_file.display()))?.entry(profile).or_insert_with(|| serde_json::json!({}));

    let profile_map = profile_obj.as_object_mut().ok_or_else(|| anyhow::anyhow!("Profile \"{}\" must be an object", profile))?;

    let stdin = io::stdin();
    let mut reader = stdin.lock();

    println!("Configuring profile \x1b[1m{}\x1b[0m", profile);
    println!("Config will be written to {}\n", config_file.display());

    for service in &services {
        println!("\x1b[1m[{}]\x1b[0m", service);

        let existing = profile_map.get(service.as_str()).and_then(|v| v.as_object());
        let existing_url = existing.and_then(|o| o.get("url")).and_then(|v| v.as_str()).unwrap_or("");
        let existing_auth = existing.and_then(|o| o.get("auth_type")).and_then(|v| v.as_str()).unwrap_or("apikey");

        let url = prompt(&mut reader, "  URL", existing_url)?;
        if url.is_empty() {
            bail!("URL cannot be empty for service \"{}\"", service);
        }

        let auth_type = prompt(&mut reader, "  Auth type (none|apikey|basic|cp4d|icp4d)", existing_auth)?;
        if !VALID_AUTH_TYPES.contains(&auth_type.as_str()) {
            bail!("Invalid auth type \"{}\". Must be one of: {}", auth_type, VALID_AUTH_TYPES.join(", "));
        }

        let mut service_obj = serde_json::json!({
            "url": url,
            "auth_type": auth_type,
        });

        match auth_type.as_str() {
            "apikey" => {
                let existing_apikey = existing.and_then(|o| o.get("apikey")).and_then(|v| v.as_str()).unwrap_or("");
                let display_default = if existing_apikey.is_empty() { String::new() } else { mask_credential(existing_apikey) };
                let apikey = prompt(&mut reader, "  API key", &display_default)?;
                let apikey = if apikey == display_default { existing_apikey.to_string() } else { apikey };
                if apikey.is_empty() {
                    bail!("API key cannot be empty for service \"{}\"", service);
                }
                service_obj["apikey"] = serde_json::Value::String(apikey);
            }
            "basic" | "cp4d" | "icp4d" => {
                let existing_username = existing.and_then(|o| o.get("username")).and_then(|v| v.as_str()).unwrap_or("");
                let existing_password = existing.and_then(|o| o.get("password")).and_then(|v| v.as_str()).unwrap_or("");
                let username = prompt(&mut reader, "  Username", existing_username)?;
                if username.is_empty() {
                    bail!("Username cannot be empty for service \"{}\"", service);
                }
                let display_default = if existing_password.is_empty() { String::new() } else { mask_credential(existing_password) };
                let password = prompt(&mut reader, "  Password", &display_default)?;
                let password = if password == display_default { existing_password.to_string() } else { password };
                if password.is_empty() {
                    bail!("Password cannot be empty for service \"{}\"", service);
                }
                service_obj["username"] = serde_json::Value::String(username);
                service_obj["password"] = serde_json::Value::String(password);
            }
            _ => {} // "none" — no credentials needed
        }

        profile_map.insert(service.clone(), service_obj);

        println!();
    }

    let json = serde_json::to_string_pretty(&root)?;
    crate::config::write_credential_file(&config_file, &json)?;

    println!("\x1b[32mProfile \"{}\" saved to {}\x1b[0m", profile, config_file.display());
    println!("\nConfigured services:");
    for service in &services {
        println!("  - {}", service);
    }

    println!("\nCredentials stored in config file. Ready to use.");

    Ok(())
}

/// Write a profile template with placeholder values and print it (no interactive prompts).
fn write_template(services: &[String], profile: &str, profile_path: Option<&str>) -> Result<()> {
    let config_file = resolve_config_path(profile_path)?;
    let mut root = load_existing_config(&config_file)?;

    let profiles = root.as_object_mut().ok_or_else(|| anyhow::anyhow!("Config file root must be a JSON object, not an array or primitive"))?.entry("profiles").or_insert_with(|| serde_json::json!({}));

    let profile_obj = profiles.as_object_mut().ok_or_else(|| anyhow::anyhow!("\"profiles\" must be an object in {}", config_file.display()))?.entry(profile).or_insert_with(|| serde_json::json!({}));

    let profile_map = profile_obj.as_object_mut().ok_or_else(|| anyhow::anyhow!("Profile \"{}\" must be an object", profile))?;

    for service in services {
        let upper = service.to_uppercase();
        let mut svc = serde_json::Map::new();
        svc.insert("url".into(), serde_json::Value::String(format!("<{}_URL>", upper)));
        svc.insert("auth_type".into(), serde_json::Value::String(format!("<{}>", VALID_AUTH_TYPES.join("|"))));
        svc.insert("apikey".into(), serde_json::Value::String(format!("<{}_APIKEY>", upper)));
        profile_map.insert(service.clone(), serde_json::Value::Object(svc));
    }

    let json = serde_json::to_string_pretty(&root)?;
    crate::config::write_credential_file(&config_file, &json)?;
    println!("{}", json);

    Ok(())
}

/// Determine which services need configuration.
///
/// If config files are provided, parse YAML and extract resource kinds, then map to services.
/// If no config files, return all non-local services from the schema registry.
fn determine_services(config_paths: &[String]) -> Result<Vec<String>> {
    let schemas = wxctl_providers::load_all_schemas()?;

    if config_paths.is_empty() {
        // No files: collect all unique services, filter out "local"
        let mut services: BTreeSet<String> = BTreeSet::new();
        for schema in &schemas {
            let svc = &schema.resource.service;
            if svc != "local" {
                services.insert(svc.clone());
            }
        }
        return Ok(services.into_iter().collect());
    }

    // Build kind → service mapping
    let mut kind_to_service: BTreeMap<String, String> = BTreeMap::new();
    for schema in &schemas {
        kind_to_service.insert(schema.resource.kind.clone(), schema.resource.service.clone());
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
/// Uses --profile-path if given, otherwise defaults to ~/.wxctl/config.json.
pub fn resolve_config_path(profile_path: Option<&str>) -> Result<PathBuf> {
    if let Some(path) = profile_path {
        return Ok(PathBuf::from(path));
    }

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?;
    Ok(home.join(".wxctl").join("config.json"))
}

/// Load existing config file or return a fresh skeleton.
pub fn load_existing_config(path: &PathBuf) -> Result<serde_json::Value> {
    if path.exists() {
        let content = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", path.display(), e))?;
        let value: serde_json::Value = serde_json::from_str(&content).map_err(|e| anyhow::anyhow!("Failed to parse '{}': {}", path.display(), e))?;
        Ok(value)
    } else {
        Ok(serde_json::json!({"profiles": {}}))
    }
}

/// Prompt the user for input with a default value.
fn prompt(reader: &mut impl BufRead, label: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{}: ", label);
    } else {
        print!("{} [{}]: ", label, default);
    }
    io::stdout().flush()?;

    let mut input = String::new();
    reader.read_line(&mut input)?;
    let input = input.trim().to_string();

    if input.is_empty() { Ok(default.to_string()) } else { Ok(input) }
}

/// Mask a credential for display, showing only the last 4 characters.
fn mask_credential(value: &str) -> String {
    if value.len() <= 4 { "****".to_string() } else { format!("****{}", &value[value.len() - 4..]) }
}
