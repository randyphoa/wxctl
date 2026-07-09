use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

use crate::util::extract_artifact_id;

pub struct CategoryHandler;

/// Normalize a category create response so the engine's ref resolver finds the id.
///
/// The CP4D `/v3/categories` create response puts the id at
/// `resources[0].artifact_id`, but `extract_resource_id` only reads top-level
/// `artifact_id` / `metadata.artifact_id` / `entity.artifact_id`. Copy the id up
/// to the top level so `${category.<ref>.artifact_id}` resolves into dependents
/// (child categories, term `parent_category.id`, rule triggers). No-op when a
/// top-level `artifact_id` already exists (SaaS shape) or no id is present.
fn hoist_artifact_id(response: &mut Value) {
    if response.get("artifact_id").and_then(|v| v.as_str()).is_some() {
        return;
    }
    if let Some(id) = extract_artifact_id(response).map(str::to_string)
        && let Some(obj) = response.as_object_mut()
    {
        obj.insert("artifact_id".to_string(), Value::String(id));
    }
}

impl ResourceHandler for CategoryHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            hoist_artifact_id(response);
            if let Some(id) = response.get("artifact_id").and_then(|v| v.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, artifact_id = %id, "Category created; artifact_id hoisted to top level");
            }
            Ok(())
        })
    }

    /// Mirror the create-time hoist on the discovery path. A discovered category
    /// (already on the cluster) carries its id at `metadata.artifact_id`, but
    /// `${category.<ref>.artifact_id}` resolves against a top-level `artifact_id`.
    /// Without this, a child category / term / rule that references a *pre-existing*
    /// parent fails with "Field 'artifact_id' not found" (and re-apply NoChange,
    /// AC4, can never resolve the parent ref). Hoist the discovered id to top level
    /// too so created and discovered categories present the id identically.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            hoist_artifact_id(remote_data);
            if let Some(id) = remote_data.get("artifact_id").and_then(|v| v.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, artifact_id = %id, "Category discovered; artifact_id hoisted to top level");
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // hoist_artifact_id lifts the id to the top level so `${category.<ref>.artifact_id}`
    // resolves. Sources: `resources[0].artifact_id` (CP4D create) or `metadata.artifact_id`
    // (SaaS create + discovered list_and_get). A pre-existing top-level id wins (no
    // clobber); no discoverable id → absent (no-fabricate). Expected `None` = key absent.
    #[test]
    fn hoist_artifact_id_cases() {
        let cases: &[(&str, Value, Option<&str>)] = &[
            ("lifts resources[0].artifact_id", json!({"resources": [{"artifact_id": "cat-123", "name": "PII"}]}), Some("cat-123")),
            ("existing top-level id wins over nested", json!({"artifact_id": "top-1", "resources": [{"artifact_id": "nested-2"}]}), Some("top-1")),
            ("lifts metadata.artifact_id (bare create)", json!({"metadata": {"artifact_id": "saas-9"}}), Some("saas-9")),
            ("lifts metadata.artifact_id (discovered shape with entity/name siblings)", json!({"metadata": {"artifact_id": "9d7768bd", "name": "e2e Glossary Domain"}, "entity": {}}), Some("9d7768bd")),
            ("no-op when no id present", json!({"name": "PII"}), None),
        ];
        for (msg, mut resp, expected) in cases.iter().map(|(m, r, e)| (*m, r.clone(), *e)) {
            hoist_artifact_id(&mut resp);
            assert_eq!(resp.get("artifact_id").and_then(|v| v.as_str()), expected, "{msg}");
        }
    }
}
