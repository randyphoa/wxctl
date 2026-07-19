//! `instana_builtin_event_spec` handler — built-in event specifications are
//! ADOPTED by id and their `enabled` state converges via POST `/{id}/enable` |
//! `/{id}/disable`. The 1.307 API has no create/update BODY for built-ins (GET
//! list + GET /{id} only; enable/disable are side-effect POSTs), so a single
//! `converge_enabled` fn serves both `pre_create` (adopt by id, or error) and
//! `pre_update` (converge against the discovered current). Destroy is the
//! schema-driven DELETE /{id} (endpoint-contract "reset to default", OQ2 —
//! live-probed in Phase 3). Precedent: adopt shape from `AutomationActionHandler`,
//! shared-fn-for-both-verbs from `AlertHandler`.

use anyhow::{Result, anyhow};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const BUILTIN_PATH: &str = "/api/events/settings/event-specifications/built-in";

pub struct BuiltinEventSpecHandler;

/// The enable/disable verb needed to move `current` -> `desired`, or None if
/// already converged.
fn toggle_verb(desired: bool, current: bool) -> Option<&'static str> {
    if desired == current {
        None
    } else if desired {
        Some("enable")
    } else {
        Some("disable")
    }
}

impl ResourceHandler for BuiltinEventSpecHandler {
    /// Adopt the built-in by `id` (GET /{id}; a miss is an explicit error) and
    /// converge `enabled`. Fires only when discovery reported the built-in absent
    /// (a mistyped id); built-ins normally exist -> the Update/pre_update path.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { converge_enabled(resource, client, operation_id).await })
    }

    /// Converge the discovered built-in's `enabled` to the desired value.
    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { converge_enabled(desired, client, operation_id).await })
    }
}

/// Adopt the built-in event spec by `id` and converge its `enabled` state. The
/// API has no create/update body — enable/disable are POST side-effects. GET the
/// current spec (a miss is an adopt error naming the id), and POST `/{id}/enable`
/// or `/{id}/disable` only when the current state differs from desired. Return the
/// fetched spec with the desired `enabled` overlaid so recorded state reflects the
/// converged value.
async fn converge_enabled(resource: &Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    let id = resource.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{operation_id}] instana_builtin_event_spec requires a built-in 'id'"))?.to_string();
    let desired_enabled = resource.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    let get_path = format!("{BUILTIN_PATH}/{id}");
    let current: Value = client.execute(operation_id, RequestSpec::new(Method::GET, &get_path)).await.map_err(|e| anyhow!("[{operation_id}] instana_builtin_event_spec '{id}' not found — built-in event specs are adopted, not created (pick an id from GET {BUILTIN_PATH}): {e}"))?;
    let current_enabled = current.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
    if let Some(verb) = toggle_verb(desired_enabled, current_enabled) {
        let toggle_path = format!("{BUILTIN_PATH}/{id}/{verb}");
        let _: Value = client.execute(operation_id, RequestSpec::new(Method::POST, &toggle_path)).await.map_err(|e| anyhow!("[{operation_id}] instana_builtin_event_spec '{id}' {verb} failed: {e}"))?;
        tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "instana_builtin_event_spec", id = %id, enabled = desired_enabled, "converged built-in event spec enabled state");
    }
    let mut out = current;
    if let Some(obj) = out.as_object_mut() {
        obj.insert("enabled".to_string(), Value::Bool(desired_enabled));
    }
    Ok(HookOutcome::Handled(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    // toggle_verb fires enable/disable only on drift, None when already converged.
    #[test]
    fn toggle_verb_only_fires_on_drift() {
        assert_eq!(toggle_verb(true, true), None);
        assert_eq!(toggle_verb(false, false), None);
        assert_eq!(toggle_verb(false, true), Some("disable"));
        assert_eq!(toggle_verb(true, false), Some("enable"));
    }
}
