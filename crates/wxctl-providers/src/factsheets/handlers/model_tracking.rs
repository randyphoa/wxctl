//! `factsheets/model_tracking` handler — associates a WML model with a
//! governance `model_entry` (AI use case) via
//! `POST /v1/aigov/model_inventory/models/{model}/model_entry`.
//!
//! Discovery is `skip` (no server-assigned association id). Idempotency comes
//! from a manual existence check in `pre_create` (GET the entry's tracked
//! `physical_models[]`); if the model is already tracked the tracking POST is
//! skipped. The body itself is materialized from the schema's `api_field`
//! mappings (default POST), so `pre_create` returns `Continue` on the
//! not-yet-tracked path. `recover_from_create_error` treats an "already
//! tracked" conflict (409) as success; `pre_delete` untracks with 404
//! tolerance (mirrors the storage_registration / s3_object destroy pattern).

use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec, error_has_status};
use wxctl_core::logging::error_codes;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct ModelTrackingHandler;

impl ResourceHandler for ModelTrackingHandler {
    /// Require exactly one of space_id / project_id / catalog_id (model scope).
    fn post_validate<'a>(&'a self, resource: &'a mut Value, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let set: Vec<&str> = ["space_id", "project_id", "catalog_id"].into_iter().filter(|k| resource.get(*k).and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty())).collect();
            if set.len() != 1 {
                bail!("[{}] model_tracking requires exactly one of space_id/project_id/catalog_id (got {}: {:?})", error_codes::H901, set.len(), set);
            }
            Ok(())
        })
    }

    /// Idempotent existence check. If the model is already in the use case's
    /// tracked physical_models[], synthesize the association and skip the POST;
    /// otherwise fall through to the default POST (body from api_field mappings).
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let model = require_str(resource, "model")?.to_string();
            let model_entry = require_str(resource, "model_entry")?.to_string();
            let catalog_id = require_str(resource, "model_entry_catalog_id")?.to_string();
            if is_already_tracked(client, operation_id, &model, &model_entry, &catalog_id).await? {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, model = %model, model_entry = %model_entry, "model already tracked — skipping tracking POST");
                return Ok(HookOutcome::Handled(synthesize(resource, &model, &model_entry)));
            }
            ensure_workspace_associated(client, operation_id, resource, &model_entry, &catalog_id).await?;
            Ok(HookOutcome::Continue)
        })
    }

    /// Stamp the synthesized stable id (and echo scope inputs) onto the default
    /// POST response — the LinkModel response carries no association id, so the
    /// runtime store needs one for id_field extraction.
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let (Some(model), Some(entry)) = (resource.get("model").and_then(|v| v.as_str()), resource.get("model_entry").and_then(|v| v.as_str()))
                && let Some(obj) = response.as_object_mut()
            {
                obj.entry("id").or_insert_with(|| Value::String(format!("{model}:{entry}")));
                obj.entry("model").or_insert_with(|| Value::String(model.to_string()));
                obj.entry("model_entry").or_insert_with(|| Value::String(entry.to_string()));
            }
            Ok(())
        })
    }

    /// An "already tracked" conflict (409) is a successful, idempotent track.
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            if error_has_status(error, 409) {
                let model = require_str(resource, "model")?.to_string();
                let model_entry = require_str(resource, "model_entry")?.to_string();
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, model = %model, model_entry = %model_entry, "tracking POST returned 409 already-tracked — treating as success");
                return Ok(Some(synthesize(resource, &model, &model_entry)));
            }
            Ok(None)
        })
    }

    /// Untrack the model from the use case, tolerating "not tracked" (404).
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let model = require_str(resource, "model")?.to_string();
            // On a clean slate (or an apply that failed before the model existed) the
            // ${wml_model...}/${space...} references don't resolve, leaving literal
            // templates here. Nothing is tracked to remove — skip the untrack DELETE
            // (idempotent no-op) instead of POSTing a malformed path the API 400s on.
            if model.contains("${") {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, model = %model, "model reference unresolved (parent absent) — nothing to untrack");
                return Ok(HookOutcome::Handled(json!({ "model": model, "untracked": false })));
            }
            let mut path = format!("/v1/aigov/model_inventory/models/{model}/model_entry");
            if let Some((k, v)) = scope_param(resource) {
                path.push_str(&format!("?{k}={v}"));
            }
            let spec = RequestSpec::new(Method::DELETE, &path).body(BodyKind::None).not_found_ok();
            match client.execute::<Value>(operation_id, spec).await {
                Ok(_) => {}
                Err(e) if error_has_status(&e, 404) => {
                    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, model = %model, error_code = %error_codes::H601, "model already untracked — idempotent DELETE");
                }
                Err(e) => return Err(e),
            }
            Ok(HookOutcome::Handled(json!({ "model": model, "untracked": true })))
        })
    }
}

/// GET the use case's tracked models and scan `physical_models[]` for `model`.
/// A 404 (entry has no tracked models yet) reads as not-tracked. The exact id
/// key is not pinned by the OpenAPI, so match defensively on common id fields
/// (Phase 3 live confirms/tightens this).
async fn is_already_tracked(client: &HttpClient, operation_id: &str, model: &str, model_entry: &str, catalog_id: &str) -> Result<bool> {
    let path = format!("/v1/aigov/model_inventory/model_entries/{model_entry}/models?catalog_id={catalog_id}");
    let spec = RequestSpec::new(Method::GET, &path).body(BodyKind::None).not_found_ok();
    let resp: Value = match client.execute::<Value>(operation_id, spec).await {
        Ok(v) => v,
        Err(e) if error_has_status(&e, 404) => return Ok(false),
        Err(e) => return Err(e),
    };
    let found = resp.get("physical_models").and_then(|v| v.as_array()).is_some_and(|arr| arr.iter().any(|m| ["id", "asset_id", "model_id"].iter().any(|k| m.get(*k).and_then(|v| v.as_str()) == Some(model))));
    Ok(found)
}

/// Ensure the model's deployment workspace is associated with the governance use case before
/// tracking. The Watson Governance API requires the workspace (space or project) to be
/// associated with the use case first; without this the tracking POST returns 400.
/// Catalog-scoped models have no associate-able workspace — skip silently.
async fn ensure_workspace_associated(client: &HttpClient, operation_id: &str, resource: &Value, model_entry: &str, catalog_id: &str) -> Result<()> {
    let (workspace_id, workspace_type, phase_name) = match scope_param(resource) {
        Some(("space_id", v)) => (v, "space", "Operate"),
        Some(("project_id", v)) => (v, "project", "Develop"),
        Some(("catalog_id", _)) => {
            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, "model scope is a catalog — skipping workspace association");
            return Ok(());
        }
        _ => return Ok(()),
    };
    let path = format!("/v1/aigov/factsheet/ai_usecases/{model_entry}/workspaces?inventory_id={catalog_id}");
    let get_spec = RequestSpec::new(Method::GET, &path).body(BodyKind::None).not_found_ok();
    let resp: Value = match client.execute::<Value>(operation_id, get_spec).await {
        Ok(v) => v,
        Err(e) if error_has_status(&e, 404) => Value::Null,
        Err(e) => return Err(e),
    };
    if workspace_is_associated(&resp, &workspace_id) {
        tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, workspace = %workspace_id, "workspace already associated with use case");
        return Ok(());
    }
    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, workspace = %workspace_id, model_entry = %model_entry, "associating workspace with use case before tracking");
    let body = json!({ "phase_name": phase_name, "workspaces": [ { "id": workspace_id, "type": workspace_type } ] });
    let post_spec = RequestSpec::new(Method::POST, &path).body(BodyKind::Json(body));
    match client.execute::<Value>(operation_id, post_spec).await {
        Ok(_) => {}
        Err(e) if error_has_status(&e, 409) => {}
        Err(e) => return Err(e),
    }
    Ok(())
}

/// Scan `resp["associated_workspaces"][*]["workspaces"][*]["id"]` for `workspace_id`.
/// Missing keys or non-array values are treated as no match (false).
fn workspace_is_associated(resp: &Value, workspace_id: &str) -> bool {
    resp.get("associated_workspaces").and_then(|v| v.as_array()).is_some_and(|outer| outer.iter().any(|entry| entry.get("workspaces").and_then(|v| v.as_array()).is_some_and(|inner| inner.iter().any(|w| w.get("id").and_then(|v| v.as_str()) == Some(workspace_id)))))
}

/// Build the stored association value: a stable synthesized id plus echoed
/// inputs (so state_fields are present on the Handled/recover responses).
fn synthesize(resource: &Value, model: &str, model_entry: &str) -> Value {
    let mut out = serde_json::Map::new();
    out.insert("id".into(), Value::String(format!("{model}:{model_entry}")));
    out.insert("model".into(), Value::String(model.to_string()));
    out.insert("model_entry".into(), Value::String(model_entry.to_string()));
    if let Some(v) = resource.get("version_number") {
        out.insert("version_number".into(), v.clone());
    }
    Value::Object(out)
}

/// Return the single set model-scope query param (post_validate guarantees one).
fn scope_param(resource: &Value) -> Option<(&'static str, String)> {
    for k in ["space_id", "project_id", "catalog_id"] {
        if let Some(s) = resource.get(k).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
            return Some((k, s.to_string()));
        }
    }
    None
}

fn require_str<'a>(resource: &'a Value, field: &str) -> Result<&'a str> {
    resource.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{}] model_tracking missing required field '{field}'", error_codes::H901))
}

#[cfg(test)]
mod tests {
    use super::*;

    // physical_models[] is scanned for the model id across the common id keys;
    // a missing array or no match reads as not-tracked.
    #[test]
    fn synthesize_builds_stable_composite_id() {
        let r = json!({"version_number": "1.0.0"});
        let v = synthesize(&r, "m-1", "e-9");
        assert_eq!(v.get("id").and_then(|x| x.as_str()), Some("m-1:e-9"));
        assert_eq!(v.get("model").and_then(|x| x.as_str()), Some("m-1"));
        assert_eq!(v.get("model_entry").and_then(|x| x.as_str()), Some("e-9"));
        assert_eq!(v.get("version_number").and_then(|x| x.as_str()), Some("1.0.0"));
    }

    // scope_param returns the single non-empty scope; absent → None.
    #[test]
    fn scope_param_picks_the_one_set_scope() {
        assert_eq!(scope_param(&json!({"space_id": "s-1"})), Some(("space_id", "s-1".to_string())));
        assert_eq!(scope_param(&json!({"project_id": "p-2"})), Some(("project_id", "p-2".to_string())));
        assert_eq!(scope_param(&json!({})), None);
        assert_eq!(scope_param(&json!({"space_id": ""})), None);
    }

    // workspace_is_associated scans nested associated_workspaces[*].workspaces[*].id;
    // missing/empty structure → false; matching id → true; non-matching → false.
    #[test]
    fn workspace_is_associated_scans_nested_ids() {
        let resp = json!({
            "associated_workspaces": [
                { "phase_name": "Develop", "workspaces": [ { "id": "w-match", "type": "project" }, { "id": "w-other", "type": "project" } ] },
                { "phase_name": "Operate", "workspaces": [ { "id": "w-space", "type": "space" } ] }
            ]
        });
        assert!(workspace_is_associated(&resp, "w-match"), "should find w-match");
        assert!(workspace_is_associated(&resp, "w-other"), "should find w-other");
        assert!(workspace_is_associated(&resp, "w-space"), "should find w-space");
        assert!(!workspace_is_associated(&resp, "w-absent"), "w-absent should not match");
        assert!(!workspace_is_associated(&json!({}), "w-match"), "empty resp → false");
        assert!(!workspace_is_associated(&Value::Null, "w-match"), "null resp → false");
        assert!(!workspace_is_associated(&json!({ "associated_workspaces": [] }), "w-match"), "empty outer array → false");
    }
}
