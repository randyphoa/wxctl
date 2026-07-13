use anyhow::{Context, Result};
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use tracing::Instrument;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct ConnectionHandler;

/// OAuth2 connections post client_id/secret to `/credentials` with an `app_credentials`
/// wrapper; everything else posts to `/runtime_credentials` with a `runtime_credentials`
/// wrapper. The orchestrate API 404s on runtime_credentials for OAuth configurations.
const CREDS_APP: CredsRoute = CredsRoute { path_segment: "credentials", wrapper_key: "app_credentials", span_name: "set_app_credentials", error_context: "Failed to set app credentials" };
const CREDS_RUNTIME: CredsRoute = CredsRoute { path_segment: "runtime_credentials", wrapper_key: "runtime_credentials", span_name: "set_runtime_credentials", error_context: "Failed to set runtime credentials" };

struct CredsRoute {
    path_segment: &'static str,
    wrapper_key: &'static str,
    span_name: &'static str,
    error_context: &'static str,
}

/// Either `config_security_scheme` or `connection_type` can carry the oauth2 signal —
/// fixtures set scheme to the literal `oauth2` category while `connection_type` names the
/// specific flow (`oauth2_client_creds`). Accept either.
fn is_oauth2(resource: &Value) -> bool {
    let scheme_is_oauth = resource.get("config_security_scheme").and_then(|v| v.as_str()).map(|s| s.starts_with("oauth2")).unwrap_or(false);
    let type_is_oauth = resource.get("connection_type").and_then(|v| v.as_str()).map(|s| s.starts_with("oauth2_")).unwrap_or(false);
    scheme_is_oauth || type_is_oauth
}

/// Normalize the `environment` field to a sorted, deduped list of environment
/// names. Accepts the list form (`["draft", "live"]`) and, defensively, a legacy
/// scalar string; anything else yields an empty list.
fn desired_environments(resource: &Value) -> Vec<String> {
    let mut envs: Vec<String> = match resource.get("environment") {
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        Some(Value::String(s)) => vec![s.clone()],
        _ => Vec::new(),
    };
    envs.sort();
    envs.dedup();
    envs
}

/// Environments the connection is currently configured for, read from the
/// handler-managed `configured_environments` state field (set on the discovered
/// remote by `post_discover`). Empty when the field is absent.
fn configured_set(value: &Value) -> Vec<String> {
    match value.get("configured_environments") {
        Some(Value::Array(items)) => items.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        _ => Vec::new(),
    }
}

/// True if any string anywhere within `value` still carries an unresolved `${...}`
/// reference — a credential whose upstream dependency was not discovered. Used to
/// skip converge rather than POST a literal template.
fn has_unresolved_template(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains("${"),
        Value::Array(items) => items.iter().any(has_unresolved_template),
        Value::Object(map) => map.values().any(has_unresolved_template),
        _ => false,
    }
}

impl ResourceHandler for ConnectionHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let app_id = response.get("app_id").or_else(|| resource.get("app_id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No app_id found in connection response or resource"))?;

            // Configure each declared environment independently (draft and/or live).
            // create_configuration + post_credentials are the existing probe-then-create
            // idempotent pair; the credentials and OAuth route are the same for every env.
            for environment in desired_environments(resource) {
                create_configuration(client, operation_id, app_id, &environment, resource).await?;

                if let Some(credentials) = resource.get("credentials") {
                    let route = if is_oauth2(resource) { &CREDS_APP } else { &CREDS_RUNTIME };
                    post_credentials(client, operation_id, app_id, &environment, credentials, route).await?;
                }
            }

            Ok(())
        })
    }

    fn post_validate<'a>(&'a self, resource: &'a mut Value, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Publish the declared environments as comparable state so a missing env on an
            // already-created connection surfaces as a state diff (Update -> converge). Only
            // set it when `environment` is declared: an absent field must stay absent so
            // compare() skips it (inert for connections that don't opt into dual-env).
            let envs = desired_environments(resource);
            if !envs.is_empty() {
                resource["configured_environments"] = json!(envs);
            }
            Ok(())
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Probe which environments the remote connection actually has configured so
            // compare() can diff against the desired set. app_id keys the config endpoints.
            let Some(app_id) = remote_data.get("app_id").and_then(|v| v.as_str()).map(String::from) else {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, "connection remote has no app_id -- skipping configured-environments probe");
                return Ok(());
            };
            let mut configured: Vec<String> = Vec::new();
            for environment in ["draft", "live"] {
                let endpoint = format!("/v1/orchestrate/connections/applications/{}/configurations/{}", app_id, environment);
                // not_found_ok() suppresses the expected-404 error event for an unconfigured env.
                let spec = RequestSpec::new(Method::GET, &endpoint).body(BodyKind::None).not_found_ok();
                match client.execute::<Value>(operation_id, spec).await {
                    Ok(_) => configured.push(environment.to_string()),
                    Err(e) if wxctl_core::client::error_has_status(&e, 404) => {}
                    Err(e) => return Err(e).context("Failed to probe connection configuration during discovery"),
                }
            }
            configured.sort();
            remote_data["configured_environments"] = json!(configured);
            Ok(())
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // Converge: add each environment listed in desired but not yet configured on the
            // remote connection, in place. Never PATCH/DELETE -- return Handled(current) so the
            // connection keeps its id and existing (e.g. draft) configuration untouched.
            let desired: &Value = desired;
            let app_id = desired.get("app_id").or_else(|| current.get("app_id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No app_id found for connection converge"))?.to_string();

            let current_envs = configured_set(current);
            let missing: Vec<String> = desired_environments(desired).into_iter().filter(|env| !current_envs.contains(env)).collect();

            if missing.is_empty() {
                return Ok(HookOutcome::Handled(current.clone()));
            }

            // Guard: never POST a literal unresolved `${...}` credential template. Reachable only
            // if a referenced dependency was not discovered before converge; `depends_on` prevents
            // this in the demo, the guard protects general use.
            if let Some(credentials) = desired.get("credentials")
                && has_unresolved_template(credentials)
            {
                tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, app_id = %app_id, "connection credentials carry an unresolved reference -- skipping live-config converge");
                return Ok(HookOutcome::Handled(current.clone()));
            }

            for environment in missing {
                create_configuration(client, operation_id, &app_id, &environment, desired).await?;
                if let Some(credentials) = desired.get("credentials") {
                    let route = if is_oauth2(desired) { &CREDS_APP } else { &CREDS_RUNTIME };
                    post_credentials(client, operation_id, &app_id, &environment, credentials, route).await?;
                }
            }

            Ok(HookOutcome::Handled(current.clone()))
        })
    }
}

/// Create configuration for the connection
fn create_configuration<'a>(client: &'a HttpClient, operation_id: &'a str, app_id: &'a str, environment: &'a str, resource: &'a Value) -> impl Future<Output = Result<Value>> + Send + 'a {
    let span = tracing::debug_span!(
        target: "wxctl::substage::provider",
        "create_connection_configuration",
        operation_id = %operation_id,
        app_id = %app_id,
        environment = %environment
    );

    async move {
        // Check if configuration already exists; 404 = not yet created → proceed.
        // not_found_ok() suppresses the wxctl::error event for the expected 404 so it
        // doesn't count as a failure in the plan/apply summary.
        let get_config_endpoint = format!("/v1/orchestrate/connections/applications/{}/configurations/{}", app_id, environment);
        let probe_spec = RequestSpec::new(Method::GET, &get_config_endpoint).body(BodyKind::None).not_found_ok();

        match client.execute::<Value>(operation_id, probe_spec).await {
            Ok(existing_config) => {
                return Ok(existing_config);
            }
            Err(e) if wxctl_core::client::error_has_status(&e, 404) => {
                // Configuration doesn't exist, proceed with creation
            }
            Err(e) => return Err(e).context("Failed to probe connection configuration"),
        }

        let endpoint = format!("/v1/orchestrate/connections/applications/{}/configurations", app_id);

        let security_scheme = resource.get("config_security_scheme").and_then(|v| v.as_str()).unwrap_or("key_value_creds").to_string();

        // Build configuration payload
        let mut payload = json!({
            "app_id": app_id,
            "environment": environment,
            "preference": resource.get("preference").and_then(|v| v.as_str()).unwrap_or("team"),
            "security_scheme": security_scheme,
            "sso": resource.get("config_sso").and_then(|v| v.as_bool()).unwrap_or(false),
            "config_id": null,
            "tenant_id": null
        });

        // Add optional fields (use null if not present).
        // auth_type is only valid when the security scheme is oauth2 — the orchestrate
        // API rejects the configuration with HTTP 400 "auth type should not be provided
        // unless security scheme is oauth2" otherwise.
        for (resource_key, payload_key) in [("config_server_url", "server_url"), ("idp_config_data", "idp_config_data"), ("app_config_data", "app_config_data")] {
            payload[payload_key] = resource.get(resource_key).cloned().unwrap_or(Value::Null);
        }
        if security_scheme.starts_with("oauth2") {
            payload["auth_type"] = resource.get("config_auth_type").cloned().unwrap_or(Value::Null);
        }

        // idp_config_data / app_config_data carry OAuth client secrets — redact them at emission.
        let spec = RequestSpec::new(Method::POST, &endpoint).body(BodyKind::Json(payload)).sensitive_paths(vec!["idp_config_data".into(), "app_config_data".into()]);
        let response: Value = client.execute(operation_id, spec).await.context("Failed to create connection configuration")?;

        Ok(response)
    }
    .instrument(span)
}

fn post_credentials<'a>(client: &'a HttpClient, operation_id: &'a str, app_id: &'a str, environment: &'a str, credentials: &'a Value, route: &'static CredsRoute) -> impl Future<Output = Result<Value>> + Send + 'a {
    let span = tracing::debug_span!(
        target: "wxctl::substage::provider",
        "post_connection_credentials",
        operation_id = %operation_id,
        app_id = %app_id,
        environment = %environment,
        kind = %route.span_name
    );

    async move {
        let endpoint = format!("/v1/orchestrate/connections/applications/{}/configs/{}/{}", app_id, environment, route.path_segment);
        let payload = json!({ route.wrapper_key: credentials });
        // Credentials are arbitrary key-value pairs (keys like `url` defeat keyword
        // redaction) — redact the whole wrapper at emission.
        let spec = RequestSpec::new(Method::POST, &endpoint).body(BodyKind::Json(payload)).sensitive_paths(vec![route.wrapper_key.to_string()]);
        client.execute(operation_id, spec).await.context(route.error_context)
    }
    .instrument(span)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desired_environments_normalizes() {
        // Absent -> empty.
        assert_eq!(desired_environments(&json!({})), Vec::<String>::new());
        // Single.
        assert_eq!(desired_environments(&json!({"environment": ["draft"]})), vec!["draft".to_string()]);
        // Both, already ordered.
        assert_eq!(desired_environments(&json!({"environment": ["draft", "live"]})), vec!["draft".to_string(), "live".to_string()]);
        // Unsorted + duplicate -> sorted + deduped.
        assert_eq!(desired_environments(&json!({"environment": ["live", "draft", "live"]})), vec!["draft".to_string(), "live".to_string()]);
        // Defensive legacy-scalar fallback.
        assert_eq!(desired_environments(&json!({"environment": "draft"})), vec!["draft".to_string()]);
    }

    #[test]
    fn configured_set_reads_discovered_state() {
        assert_eq!(configured_set(&json!({"configured_environments": ["draft"]})), vec!["draft".to_string()]);
        assert_eq!(configured_set(&json!({"configured_environments": ["draft", "live"]})), vec!["draft".to_string(), "live".to_string()]);
        assert_eq!(configured_set(&json!({})), Vec::<String>::new());
    }

    #[test]
    fn has_unresolved_template_detects_refs() {
        assert!(has_unresolved_template(&json!({"api_key": "${env:X}"})));
        assert!(has_unresolved_template(&json!({"nested": {"space_id": "${space.s.metadata.id}"}})));
        assert!(!has_unresolved_template(&json!({"wml_apikey": "literal", "wml_url": "https://x.example.com"})));
    }
}
