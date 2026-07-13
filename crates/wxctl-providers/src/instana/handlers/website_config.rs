//! `website_config` handler — Instana creates an EUM website via
//! `POST /api/website-monitoring/config?name=<name>`: the name rides a QUERY
//! param and there is no meaningful body (the optional array body seeds
//! monitoring configs we don't use). The default materializer would drop any
//! non-declared body (docs/troubleshoot/pre-create-body-reshape-dropped-fix.md)
//! and has no query-only create path, so `WebsiteConfigHandler` OWNS the create
//! via `pre_create` returning `HookOutcome::Handled`: it POSTs with the `name`
//! query param and no body, then returns the created `Website { appName, id,
//! name }` so the engine records the server-assigned `id`. It errors clearly
//! when the response carries no `id` (the website can't be tracked otherwise).
//! Update (rename-only PUT) and delete are schema-driven; update never fires
//! because `name` is the discovery identity (a name change is a different
//! website → create, not update).

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const WEBSITE_CONFIG_PATH: &str = "/api/website-monitoring/config";

pub struct WebsiteConfigHandler;

impl ResourceHandler for WebsiteConfigHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = resource.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("instana_website_config requires a non-empty 'name' field"))?.to_string();
            let spec = RequestSpec::new(Method::POST, WEBSITE_CONFIG_PATH).query_param("name", &name).body(BodyKind::None);
            let response: Value = client.execute(operation_id, spec).await?;
            let created = extract_created_website(response, &name)?;
            Ok(HookOutcome::Handled(created))
        })
    }
}

/// Validate the `createWebsite` response carries a usable server-assigned `id`,
/// returning it unchanged (the engine records `id` from it via
/// `merge_request_response`). Instana's `Website` create body is
/// `{ appName, id, name }`; a missing/empty `id` means the website can't be
/// tracked, so surface a clear error rather than recording an id-less resource.
fn extract_created_website(response: Value, name: &str) -> Result<Value> {
    match response.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
        Some(_) => Ok(response),
        None => Err(anyhow!("instana_website_config create for '{name}' returned no 'id' (response: {response}) — cannot track the created website")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extract_created_website_returns_object_with_id() {
        let resp = json!({"appName": "shop", "id": "WEBSITE-abc", "name": "shop"});
        let got = extract_created_website(resp, "shop").expect("id present");
        assert_eq!(got.get("id").and_then(|v| v.as_str()), Some("WEBSITE-abc"));
    }

    #[test]
    fn extract_created_website_errors_when_id_absent() {
        let err = extract_created_website(json!({"appName": "shop", "name": "shop"}), "shop").unwrap_err();
        assert!(err.to_string().contains("no 'id'"), "unexpected error: {err}");
    }

    #[test]
    fn extract_created_website_errors_when_id_empty() {
        assert!(extract_created_website(json!({"id": "", "name": "shop"}), "shop").is_err());
    }
}
