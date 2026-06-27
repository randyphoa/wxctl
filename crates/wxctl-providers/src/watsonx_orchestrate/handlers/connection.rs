use anyhow::{Context, Result};
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use tracing::Instrument;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::traits::ResourceHandler;

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

impl ResourceHandler for ConnectionHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let app_id = response.get("app_id").or_else(|| resource.get("app_id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No app_id found in connection response or resource"))?;

            if let Some(environment) = resource.get("environment").and_then(|v| v.as_str()) {
                create_configuration(client, operation_id, app_id, environment, resource).await?;

                if let Some(credentials) = resource.get("credentials") {
                    let route = if is_oauth2(resource) { &CREDS_APP } else { &CREDS_RUNTIME };
                    post_credentials(client, operation_id, app_id, environment, credentials, route).await?;
                }
            }

            Ok(())
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
            Err(_) => {
                // Configuration doesn't exist, proceed with creation
            }
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

        let response: Value = client.create(operation_id, &endpoint, payload).await.context("Failed to create connection configuration")?;

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
        client.create(operation_id, &endpoint, payload).await.context(route.error_context)
    }
    .instrument(span)
}
