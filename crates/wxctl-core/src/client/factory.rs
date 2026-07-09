use super::HttpClient;
use super::token::TokenManager;
use crate::concurrency::{CapacityManager, ConcurrencyConfig};
use crate::types::{Deployment, Flavor, Profile, ServiceConfig};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

pub struct ClientFactory {
    profile_name: String,
    profile: Profile,
    capacity: Arc<CapacityManager>,
    request_timeout_secs: u64,
    /// Shared token managers per service — avoids redundant IAM token requests
    /// when multiple pipeline operations create clients for the same service.
    token_managers: Mutex<HashMap<String, Arc<TokenManager>>>,
}

impl ClientFactory {
    pub fn new(profile_name: &str, profile_path: Option<&str>, concurrency_config: &ConcurrencyConfig) -> Result<Self> {
        let profile = load_profile(profile_name, profile_path)?;
        let capacity = Arc::new(CapacityManager::new(concurrency_config));
        Ok(Self { profile_name: profile_name.to_string(), profile, capacity, request_timeout_secs: concurrency_config.request_timeout_secs, token_managers: Mutex::new(HashMap::new()) })
    }

    pub fn profile_name(&self) -> &str {
        &self.profile_name
    }

    /// Validate that all required services are configured in the profile.
    ///
    /// Takes an iterator of `(service_name, resource_kind)` pairs and checks
    /// that every service exists in the profile. Returns a single error listing
    /// all missing services and which resource kinds need them.
    pub fn validate_services<'a>(&self, requirements: impl Iterator<Item = (&'a str, &'a str)>) -> Result<()> {
        use std::collections::BTreeMap;

        let mut missing: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
        for (service, kind) in requirements {
            if service != "local" && !self.profile.services.contains_key(service) {
                missing.entry(service).or_default().push(kind);
            }
        }

        if missing.is_empty() {
            return Ok(());
        }

        let details: Vec<String> = missing.iter().map(|(service, kinds)| format!("  - '{}' (required by: {})", service, kinds.join(", "))).collect();

        let configured: Vec<&str> = self.profile.services.keys().map(|s| s.as_str()).collect();
        let configured_list = if configured.is_empty() { "  (none)".to_string() } else { configured.iter().map(|s| format!("  - {}", s)).collect::<Vec<_>>().join("\n") };

        anyhow::bail!(
            "Profile '{}' is missing {} service{}:\n{}\n\nConfigured services:\n{}\n\nRun 'wxctl init' to add the missing services, or configure them manually in your profile.",
            self.profile_name,
            missing.len(),
            if missing.len() == 1 { "" } else { "s" },
            details.join("\n"),
            configured_list,
        );
    }

    /// Public accessor for the active deployment of a named service.
    ///
    /// Applies the same precedence as `create_client`: per-service `deployment`
    /// field first, then the profile-level `deployment`, then `Deployment::Saas`
    /// as the implicit default. Returns an error only when a deployment string is
    /// present but unparseable.
    ///
    /// The reconciliation pipeline calls this to obtain the deployment for
    /// overlay resolution and `unsupported_on` checking without constructing
    /// a full HTTP client.
    pub fn deployment_for_service(&self, service: &str) -> Result<Deployment> {
        match self.profile.services.get(service) {
            Some(sc) => self.resolve_deployment(sc),
            None => {
                // Service not in profile: fall back to the profile-level default.
                let raw = self.profile.deployment.as_deref();
                match raw {
                    None => Ok(Deployment::Saas),
                    Some(s) => Deployment::from_str(s).with_context(|| format!("invalid deployment in profile '{}'", self.profile_name)),
                }
            }
        }
    }

    /// Resolve the effective deployment for a service: per-service override
    /// first, then the profile-level default, then SaaS as the implicit default.
    /// Errors if any provided string is unparseable.
    fn resolve_deployment(&self, service_config: &ServiceConfig) -> Result<Deployment> {
        let raw = service_config.deployment.as_deref().or(self.profile.deployment.as_deref());
        match raw {
            None => Ok(Deployment::Saas),
            Some(s) => Deployment::from_str(s).with_context(|| format!("invalid deployment for service in profile '{}'", self.profile_name)),
        }
    }

    pub fn create_client(&self, service: &str) -> Result<HttpClient> {
        let service_config = self.profile.services.get(service).ok_or_else(|| {
            let configured: Vec<&str> = self.profile.services.keys().map(|s| s.as_str()).collect();
            anyhow::anyhow!("Service '{}' is not configured in profile '{}'. Configured services: [{}]. Run 'wxctl init' to add it.", service, self.profile_name, configured.join(", "),)
        })?;

        let url = service_config.url.clone().ok_or_else(|| anyhow::anyhow!("Service '{}' in profile '{}' is missing a `url`. Non-HTTP auxiliary entries (e.g. `cos`, `db2`) are tolerated in profiles but cannot be used as a client.", service, self.profile_name))?;

        let deployment = self.resolve_deployment(service_config)?;
        let auth_type = match service_config.auth_type.clone() {
            Some(t) => t,
            None => match deployment.flavor() {
                Flavor::Saas => "apikey".to_string(),
                Flavor::Software => anyhow::bail!("service '{}' on deployment '{}' has no auth_type configured; set 'cp4d' for username/password or 'zenapikey' for ZenApiKey auth", service, deployment),
            },
        };

        if auth_type == "apikey" && deployment.flavor() == Flavor::Software {
            anyhow::bail!("deployment '{}' does not support auth_type 'apikey'; use 'cp4d' or 'zenapikey' for Software", deployment);
        }

        if auth_type == "zenapikey" && deployment.flavor() == Flavor::Saas {
            anyhow::bail!("deployment '{}' does not support auth_type 'zenapikey'; ZenApiKey is a Software-Hub-only construct — use 'apikey' for SaaS", deployment);
        }

        // The optional per-service `path_prefix` is inserted between the profile's
        // service URL and the schema `base_path` (`<url><path_prefix><base_path>`,
        // see `http::join_url`). Absent → no prefix; the profile URL covers the base.
        let path_prefix = service_config.path_prefix.clone().unwrap_or_default();

        tracing::debug!(
            target: "wxctl::substage::client",
            service = %service,
            deployment = %deployment,
            auth_type = %auth_type,
            path_prefix = %path_prefix,
            "resolved client config",
        );

        let auth_token = get_auth_token_with(service_config, service, &auth_type)?;

        // Reuse token manager for the same service to avoid redundant IAM requests
        let token_manager = {
            let mut managers = self.token_managers.lock().expect("token_managers lock poisoned");
            managers.entry(service.to_string()).or_insert_with(|| Arc::new(TokenManager::with_base_url(auth_token.clone(), auth_type.clone(), url.clone()))).clone()
        };

        HttpClient::with_token_manager(url, service.to_string(), auth_type, self.capacity.clone(), token_manager, self.request_timeout_secs, service_config.instance_id.clone(), path_prefix, deployment.clone())
    }

    /// Get available global capacity for observability
    pub fn available_capacity(&self) -> usize {
        self.capacity.available()
    }
}

/// A profile object's keys in source order, **preserving duplicates**. A plain
/// `HashMap` / `serde_json::Value` silently collapses a repeated key (last
/// wins), so a profile that declares the same service block twice would route
/// every resource of that service to one endpoint with no error. We stream
/// every key via a custom visitor instead, so the repeat survives to be caught.
struct ProfileKeyList(Vec<String>);

impl<'de> serde::Deserialize<'de> for ProfileKeyList {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        struct KeyVisitor;
        impl<'de> serde::de::Visitor<'de> for KeyVisitor {
            type Value = Vec<String>;
            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a profile object")
            }
            fn visit_map<M: serde::de::MapAccess<'de>>(self, mut map: M) -> std::result::Result<Self::Value, M::Error> {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    keys.push(key);
                    map.next_value::<serde::de::IgnoredAny>()?; // discard the block body — we only need its key
                }
                Ok(keys)
            }
        }
        deserializer.deserialize_map(KeyVisitor).map(ProfileKeyList)
    }
}

#[derive(serde::Deserialize)]
struct ProfileKeyConfig {
    #[serde(default)]
    profiles: HashMap<String, ProfileKeyList>,
}

/// Reject a profile that declares the same service block (or any key) more than
/// once. Runs on the raw text **before** the `serde_json::Value` parse, which
/// would otherwise collapse the duplicate to last-wins. By design there is
/// exactly one block per service; two accounts for the same service belong in
/// two separate profiles.
fn reject_duplicate_service_keys(content: &str, name: &str) -> Result<()> {
    let parsed: ProfileKeyConfig = serde_norway::from_str(content).context("Failed to parse YAML config")?;
    let Some(ProfileKeyList(keys)) = parsed.profiles.get(name) else { return Ok(()) };
    let mut seen = std::collections::HashSet::new();
    for key in keys {
        if !seen.insert(key.as_str()) {
            anyhow::bail!("[{}] Profile '{}' declares the block '{}' more than once. Each service may appear at most once per profile — remove the duplicate. To target two accounts for the same service, use two separate profiles selected with `-p`.", crate::logging::error_codes::C001, name, key);
        }
    }
    Ok(())
}

/// The wxctl config directory: `WXCTL_CONFIG_DIR` if set, else `~/.wxctl`.
///
/// The override exists because `dirs::home_dir` ignores `HOME` on Windows (it
/// asks the known-folder API), so a temp-`HOME` sandbox cannot redirect the
/// profiles lookup without it. Mirrors `WXCTL_UPDATE_CACHE_DIR` (update state)
/// and `WXCTL_RUNS_DIR` (run records).
fn wxctl_config_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("WXCTL_CONFIG_DIR").map(std::path::PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".wxctl")))
}

fn load_profile(name: &str, profile_path: Option<&str>) -> Result<Profile> {
    let config_path = if let Some(path) = profile_path { std::path::PathBuf::from(path) } else { wxctl_config_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?.join("profiles.yaml") };

    let content = std::fs::read_to_string(&config_path).with_context(|| format!("Failed to read config file: {}", config_path.display()))?;

    // Catch a profile that repeats a service block before the Value parse below
    // silently collapses it to last-wins.
    reject_duplicate_service_keys(&content, name)?;

    let config: serde_json::Value = serde_norway::from_str(&content).context("Failed to parse YAML config")?;

    let profiles = config.get("profiles").and_then(|p| p.as_object()).ok_or_else(|| anyhow::anyhow!("No profiles found"))?;

    let profile_data = profiles.get(name).ok_or_else(|| anyhow::anyhow!("Profile '{}' not found", name))?;

    // Expand `${env:VAR}` placeholders in profile values (URLs, apikeys, etc.)
    // before deserialising into `Profile`. Profiles intentionally keep secrets
    // and per-environment endpoints in env vars; without this pass, literals
    // like "${env:WXAI_URL}" would be handed to the HTTP client as base URLs.
    let mut yaml_value: serde_norway::Value = serde_norway::to_value(profile_data).context("Failed to convert profile data for env interpolation")?;
    crate::interpolation::interpolate(&mut yaml_value, &crate::interpolation::ProcessEnv).with_context(|| format!("Failed to expand env vars in profile '{}'", name))?;
    serde_norway::from_value(yaml_value).context("Failed to parse profile")
}

/// Read color_theme from preferences in the wxctl config file.
///
/// Looks in the given profile_path, or falls back to ~/.wxctl/profiles.yaml.
/// Returns None if the file doesn't exist, has no preferences, or color_theme is unset.
pub fn load_color_preference(profile_path: Option<&str>) -> Option<String> {
    let config_path = if let Some(path) = profile_path {
        std::path::PathBuf::from(path)
    } else {
        let config_file = wxctl_config_dir()?.join("profiles.yaml");
        if config_file.exists() { config_file } else { return None }
    };

    let content = std::fs::read_to_string(&config_path).ok()?;
    let config: serde_json::Value = serde_norway::from_str(&content).ok()?;

    config.get("preferences").and_then(|p| p.get("color_theme")).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Resolve the auth token from the profile's `ServiceConfig`. The caller passes
/// the effective `auth_type` so it can be derived from `Deployment` when the
/// profile leaves it unset.
///
/// hmac: IBM COS / S3 SigV4 — `HttpClient::apply_auth` is never invoked
/// because `CosClient` signs requests itself, but the factory still needs
/// something in the token slot. Encode `access_key:secret_key` the same way
/// `basic` encodes `username:password` so `CosClient` can unpack them via
/// `get_auth_credential`.
fn get_auth_token_with(config: &ServiceConfig, service: &str, auth_type: &str) -> Result<String> {
    match auth_type {
        "none" => Ok(String::new()),
        "apikey" => {
            let api_key = config.apikey.as_ref().ok_or_else(|| anyhow::anyhow!("No 'apikey' configured for service '{}'. Run 'wxctl init' or add it to your config file.", service))?;
            Ok(api_key.clone())
        }
        "basic" | "cp4d" | "icp4d" => {
            let username = config.username.as_ref().ok_or_else(|| anyhow::anyhow!("No 'username' configured for service '{}'.", service))?;
            let password = config.password.as_ref().ok_or_else(|| anyhow::anyhow!("No 'password' configured for service '{}'.", service))?;
            Ok(format!("{}:{}", username, password))
        }
        "bearer" | "c_api_key" | "api_token" => {
            let token = config.apikey.as_ref().ok_or_else(|| anyhow::anyhow!("No 'apikey' (bearer/API-key token) configured for service '{}'.", service))?;
            Ok(token.clone())
        }
        "hmac" => {
            let access_key = config.access_key.as_ref().ok_or_else(|| anyhow::anyhow!("No 'access_key' configured for service '{}' (auth_type: hmac).", service))?;
            let secret_key = config.secret_key.as_ref().ok_or_else(|| anyhow::anyhow!("No 'secret_key' configured for service '{}' (auth_type: hmac).", service))?;
            Ok(format!("{}:{}", access_key, secret_key))
        }
        "zenapikey" => {
            let username = config.username.as_ref().ok_or_else(|| anyhow::anyhow!("No 'username' configured for service '{}' (auth_type: zenapikey).", service))?;
            let apikey = config.apikey.as_ref().ok_or_else(|| anyhow::anyhow!("No 'apikey' configured for service '{}' (auth_type: zenapikey).", service))?;
            Ok(format!("{}:{}", username, apikey))
        }
        "pa_session" => {
            // Planning Analytics TM1 REST. Static-cookie mode: the pre-acquired paSession
            // value rides in `apikey` (typically ${env:PA_SESSION}). Login mode: pack
            // username:password like cp4d so the token manager can drive the Phase-4 login.
            if let Some(session) = config.apikey.as_ref() {
                Ok(session.clone())
            } else if let (Some(u), Some(p)) = (config.username.as_ref(), config.password.as_ref()) {
                Ok(format!("{}:{}", u, p))
            } else {
                Err(anyhow::anyhow!("No credentials for service '{}' (auth_type: pa_session): set 'apikey' to a pre-acquired paSession value (static-cookie mode) or 'username'+'password' (login mode).", service))
            }
        }
        _ => Err(anyhow::anyhow!("Unsupported auth type: {}", auth_type)),
    }
}

#[cfg(test)]
mod zenapikey_factory_tests {
    use super::*;
    use crate::concurrency::ConcurrencyConfig;

    fn cfg(value: serde_json::Value) -> ServiceConfig {
        serde_json::from_value(value).expect("parse ServiceConfig")
    }

    #[test]
    fn zenapikey_packs_username_colon_apikey_and_errors_on_missing_parts() {
        // Both present → packed as "username:apikey". Each missing part errors with
        // a field-specific message naming the service and the zenapikey auth_type.
        let full = cfg(serde_json::json!({"url": "https://example.com", "auth_type": "zenapikey", "username": "alice", "apikey": "KEY-123"}));
        assert_eq!(get_auth_token_with(&full, "common_core", "zenapikey").expect("zenapikey arm should pack"), "alice:KEY-123");

        let no_user = cfg(serde_json::json!({"url": "https://example.com", "auth_type": "zenapikey", "apikey": "KEY-123"}));
        let err = get_auth_token_with(&no_user, "common_core", "zenapikey").unwrap_err();
        assert!(err.to_string().contains("No 'username' configured for service 'common_core' (auth_type: zenapikey)"), "missing username: {err}");

        let no_key = cfg(serde_json::json!({"url": "https://example.com", "auth_type": "zenapikey", "username": "alice"}));
        let err = get_auth_token_with(&no_key, "common_core", "zenapikey").unwrap_err();
        assert!(err.to_string().contains("No 'apikey' configured for service 'common_core' (auth_type: zenapikey)"), "missing apikey: {err}");
    }

    #[test]
    fn c_api_key_passes_apikey_through_raw_and_errors_on_missing_apikey() {
        // c_api_key mirrors bearer: the raw 'apikey' field is passed through unchanged
        // (no packing), for services (e.g. IBM Concert) that expect
        // `Authorization: C_API_KEY <token>`.
        let full = cfg(serde_json::json!({"url": "https://example.com", "auth_type": "c_api_key", "apikey": "CONCERT-TOKEN-abc"}));
        assert_eq!(get_auth_token_with(&full, "concert", "c_api_key").expect("c_api_key arm should pass through"), "CONCERT-TOKEN-abc");

        let no_key = cfg(serde_json::json!({"url": "https://example.com", "auth_type": "c_api_key"}));
        let err = get_auth_token_with(&no_key, "concert", "c_api_key").unwrap_err();
        assert!(err.to_string().contains("No 'apikey' (bearer/API-key token) configured for service 'concert'"), "missing apikey: {err}");
    }

    #[test]
    fn api_token_passes_apikey_through_raw_and_errors_on_missing_apikey() {
        // api_token mirrors bearer/c_api_key: the raw 'apikey' field is passed through
        // unchanged for services (e.g. IBM Instana) that expect `Authorization: apiToken <token>`.
        let full = cfg(serde_json::json!({"url": "https://example.com", "auth_type": "api_token", "apikey": "INSTANA-TOKEN-abc"}));
        assert_eq!(get_auth_token_with(&full, "instana", "api_token").expect("api_token arm should pass through"), "INSTANA-TOKEN-abc");

        let no_key = cfg(serde_json::json!({"url": "https://example.com", "auth_type": "api_token"}));
        let err = get_auth_token_with(&no_key, "instana", "api_token").unwrap_err();
        assert!(err.to_string().contains("No 'apikey' (bearer/API-key token) configured for service 'instana'"), "missing apikey: {err}");
    }

    #[test]
    fn pa_session_prefers_apikey_then_packs_user_pass_then_errors() {
        // apikey present -> static-cookie value passed through unchanged.
        let static_mode = cfg(serde_json::json!({"url": "http://h", "auth_type": "pa_session", "apikey": "PASESSION-abc"}));
        assert_eq!(get_auth_token_with(&static_mode, "planning_analytics", "pa_session").expect("static"), "PASESSION-abc");
        // No apikey but username+password -> login mode packs "user:pass".
        let login_mode = cfg(serde_json::json!({"url": "http://h", "auth_type": "pa_session", "username": "alice", "password": "s3cret"}));
        assert_eq!(get_auth_token_with(&login_mode, "planning_analytics", "pa_session").expect("login"), "alice:s3cret");
        // Neither -> error naming the auth_type.
        let none = cfg(serde_json::json!({"url": "http://h", "auth_type": "pa_session"}));
        let err = get_auth_token_with(&none, "planning_analytics", "pa_session").unwrap_err();
        assert!(err.to_string().contains("auth_type: pa_session"), "unexpected: {err}");
    }

    #[test]
    fn zenapikey_on_saas_profile_errors_at_create_client() {
        let tmp = std::env::temp_dir().join(format!("wxctl-zenapikey-saas-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &tmp,
            r#"{
  "profiles": {
    "test-saas-zen": {
      "deployment": "saas",
      "common_core": {
        "url": "https://example.com",
        "deployment": "saas",
        "auth_type": "zenapikey",
        "username": "alice",
        "apikey": "KEY-123"
      }
    }
  }
}"#,
        )
        .expect("write tmp profile");

        let cc = ConcurrencyConfig::default();
        let factory = ClientFactory::new("test-saas-zen", Some(tmp.to_str().unwrap()), &cc).expect("factory new");
        let err = factory.create_client("common_core").err().expect("expected create_client to fail");
        assert!(err.to_string().contains("does not support auth_type 'zenapikey'"), "got: {err}");
        assert!(err.to_string().contains("Software-Hub-only construct"), "got: {err}");

        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod auth_type_required_tests {
    use super::*;
    use crate::concurrency::ConcurrencyConfig;

    #[test]
    fn software_profile_with_no_auth_type_errors_at_create_client() {
        let tmp = std::env::temp_dir().join(format!("wxctl-no-auth-type-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &tmp,
            r#"{
  "profiles": {
    "test-software-no-auth": {
      "deployment": "software-5.3.0",
      "common_core": {
        "url": "https://example.com",
        "deployment": "software-5.3.0",
        "username": "admin",
        "password": "secret"
      }
    }
  }
}"#,
        )
        .expect("write tmp profile");

        let cc = ConcurrencyConfig::default();
        let factory = ClientFactory::new("test-software-no-auth", Some(tmp.to_str().unwrap()), &cc).expect("factory new");
        let err = factory.create_client("common_core").err().expect("expected create_client to fail");
        let msg = err.to_string();
        assert!(msg.contains("has no auth_type configured"), "missing primary phrase: {msg}");
        assert!(msg.contains("'cp4d' for username/password"), "missing cp4d hint: {msg}");
        assert!(msg.contains("'zenapikey' for ZenApiKey auth"), "missing zenapikey hint: {msg}");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn saas_profile_with_no_auth_type_still_defaults_to_apikey() {
        let tmp = std::env::temp_dir().join(format!("wxctl-saas-default-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &tmp,
            r#"{
  "profiles": {
    "test-saas-default": {
      "deployment": "saas",
      "common_core": {
        "url": "https://example.com",
        "deployment": "saas",
        "apikey": "KEY-123"
      }
    }
  }
}"#,
        )
        .expect("write tmp profile");

        let cc = ConcurrencyConfig::default();
        let factory = ClientFactory::new("test-saas-default", Some(tmp.to_str().unwrap()), &cc).expect("factory new");
        let client = factory.create_client("common_core").expect("SaaS default should succeed");
        assert_eq!(client.auth_type(), "apikey");

        let _ = std::fs::remove_file(&tmp);
    }
}

#[cfg(test)]
mod duplicate_service_block_tests {
    use super::*;
    use crate::concurrency::ConcurrencyConfig;

    #[test]
    fn duplicate_service_block_errors_loudly() {
        let tmp = std::env::temp_dir().join(format!("wxctl-dup-block-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &tmp,
            r#"{
  "profiles": {
    "test-dup": {
      "deployment": "saas",
      "common_core": { "url": "https://first.example", "deployment": "saas", "apikey": "A" },
      "common_core": { "url": "https://second.example", "deployment": "saas", "apikey": "B" }
    }
  }
}"#,
        )
        .expect("write tmp profile");

        let cc = ConcurrencyConfig::default();
        let err = ClientFactory::new("test-dup", Some(tmp.to_str().unwrap()), &cc).err().expect("expected duplicate block to fail factory creation");
        let msg = err.to_string();
        assert!(msg.contains(crate::logging::error_codes::C001), "missing C001 code: {msg}");
        assert!(msg.contains("declares the block 'common_core' more than once"), "missing duplicate phrase: {msg}");
        assert!(msg.contains("two separate profiles"), "missing remediation hint: {msg}");

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn clean_target_loads_even_with_distinct_services_or_a_duplicate_sibling() {
        // Two cases that must both load cleanly: (1) a profile with distinct
        // service blocks; (2) a clean target profile sitting alongside a *sibling*
        // profile that itself repeats a block — the sibling's duplicate is
        // irrelevant to loading the one we asked for.
        let cc = ConcurrencyConfig::default();

        let clean = std::env::temp_dir().join(format!("wxctl-no-dup-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &clean,
            r#"{
  "profiles": {
    "test-clean": {
      "deployment": "saas",
      "common_core": { "url": "https://cc.example", "deployment": "saas", "apikey": "A" },
      "watsonx_orchestrate": { "url": "https://wxo.example", "deployment": "saas", "apikey": "B" }
    }
  }
}"#,
        )
        .expect("write tmp profile");
        ClientFactory::new("test-clean", Some(clean.to_str().unwrap()), &cc).expect("distinct service blocks should load cleanly");
        let _ = std::fs::remove_file(&clean);

        let sibling = std::env::temp_dir().join(format!("wxctl-dup-other-{}.json", uuid::Uuid::new_v4()));
        std::fs::write(
            &sibling,
            r#"{
  "profiles": {
    "wanted": {
      "deployment": "saas",
      "common_core": { "url": "https://cc.example", "deployment": "saas", "apikey": "A" }
    },
    "other": {
      "deployment": "saas",
      "common_core": { "url": "https://x.example", "deployment": "saas", "apikey": "A" },
      "common_core": { "url": "https://y.example", "deployment": "saas", "apikey": "B" }
    }
  }
}"#,
        )
        .expect("write tmp profile");
        ClientFactory::new("wanted", Some(sibling.to_str().unwrap()), &cc).expect("target profile is clean; sibling duplicate is irrelevant");
        let _ = std::fs::remove_file(&sibling);
    }
}
