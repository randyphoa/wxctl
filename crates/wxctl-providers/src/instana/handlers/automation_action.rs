//! `instana_automation_action` handler — automation actions are ADOPTED by name,
//! never created. The 1.307 Instana API is GET-only for actions
//! (`GET /api/automation/actions` list + `GET /{id}`), so a policy that
//! references an action resolves it to a server id via this adopt-only kind.
//! `pre_create` owns the full create: it lists actions, matches on `name`, and
//! returns the matched entry (adopt) — a miss is an explicit error naming the
//! kind and match value; it NEVER POSTs. `pre_delete` is an unconditional no-op
//! (adopted actions are shared, not owned by the apply). Precedent: the
//! common_core `environment` adopt-only handler.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const ACTIONS_PATH: &str = "/api/automation/actions";

pub struct AutomationActionHandler;

/// GET /api/automation/actions returns a bare array of Action; tolerate an
/// `items`/`actions` envelope defensively.
fn extract_entries(list: &Value) -> Vec<&Value> {
    if let Some(arr) = list.as_array() {
        return arr.iter().collect();
    }
    for key in ["items", "actions"] {
        if let Some(arr) = list.get(key).and_then(|v| v.as_array()) {
            return arr.iter().collect();
        }
    }
    Vec::new()
}

impl ResourceHandler for AutomationActionHandler {
    /// Own the full create: adopt an existing action by `name` or error — never POST.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = resource.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{operation_id}] instana_automation_action requires 'name'"))?.to_string();
            let list: Value = client.execute(operation_id, RequestSpec::new(Method::GET, ACTIONS_PATH)).await.map_err(|e| anyhow!("[{operation_id}] instana_automation_action: listing actions failed: {e}"))?;
            let mut found = Vec::new();
            for entry in extract_entries(&list) {
                let Some(entry_name) = entry.get("name").and_then(|v| v.as_str()) else { continue };
                if entry_name == name {
                    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "instana_automation_action", name = %name, "adopted automation action by name");
                    return Ok(HookOutcome::Handled(entry.clone()));
                }
                found.push(entry_name.to_string());
            }
            Err(anyhow!("[{operation_id}] instana_automation_action '{name}' not found — automation actions are adopted, not created (the Instana API is GET-only for actions; create one in the Automation UI); available actions: [{}]", found.join(", ")))
        })
    }

    /// Adopted actions are shared — destroy is unconditionally a no-op.
    fn pre_delete<'a>(&'a self, _resource: &'a Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "instana_automation_action", "adopted automation action — nothing to delete");
            Ok(HookOutcome::Handled(json!({"deleted": false})))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // extract_entries reads a bare array, falling back to items[]/actions[].
    #[test]
    fn extract_entries_scans_common_shapes() {
        assert_eq!(extract_entries(&json!([{"a": 1}, {"b": 2}])).len(), 2);
        assert_eq!(extract_entries(&json!({"items": [{"a": 1}]})).len(), 1);
        assert_eq!(extract_entries(&json!({"actions": [{"a": 1}]})).len(), 1);
        assert_eq!(extract_entries(&json!({"nope": true})).len(), 0);
    }
}
