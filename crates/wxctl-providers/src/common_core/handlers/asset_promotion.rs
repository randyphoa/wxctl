//! `common_core/asset_promotion` handler — promotes a project-side asset (a
//! model stored by the training job, matched by `asset_name`) into a deployment
//! space via `POST /v2/assets/{id}/promote`.
//!
//! The schema's discovery is `skip` (no server id to GET-by), but the kind
//! registers [`AssetPromotionReconciler`] — a custom reconciler whose
//! `discover_all` runs the same target-space CAMS name search `pre_create`
//! uses for adoption, so an unchanged re-apply plans **NoChange** instead of
//! re-running the adopt as a phantom "created" (spec AC6). The discovered
//! remote also re-resolves the project-side `source_asset_id`, keeping
//! `${asset_promotion.x.source_asset_id}` referenceable from cached state
//! (feeds factsheets model_tracking). Execution-time idempotency is unchanged:
//! `pre_create` still adopts a promoted asset already present in the space, and
//! a promote 409/duplicate falls back to the same adopt. A missing project-side
//! asset raises an explicit name-contract error. `pre_delete` resolves the
//! space-side id by name and deletes only the promoted copy (404-tolerant).
//! Live promote route/response shapes are verified in Phase 5.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec, error_matches};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, Reconciler, ResourceHandler, StateComparison};
use wxctl_core::types::{RemoteResource, ValidatedResource};

pub struct AssetPromotionHandler;

/// Custom reconciler: schema-driven discovery can't express this kind (the
/// promoted asset is matched by a type-scoped CAMS name search in the TARGET
/// space, and the cached state must carry the PROJECT-side `source_asset_id`,
/// which no single list response provides), so discovery is owned here instead
/// of `method: skip`'s always-create. Registered via
/// `wxctl_providers::get_reconciler`.
pub struct AssetPromotionReconciler;

const DEFAULT_ASSET_TYPE: &str = "wml_model";

fn require_str<'a>(resource: &'a Value, field: &str, operation_id: &str) -> Result<&'a str> {
    resource.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{operation_id}] asset_promotion missing required field '{field}'"))
}

/// CAMS asset type for the search + promote; defaults to `wml_model`.
fn asset_type(resource: &Value) -> String {
    resource.get("asset_type").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or(DEFAULT_ASSET_TYPE).to_string()
}

/// Extract a CAMS asset id from a search-result item or an asset envelope.
fn extract_asset_id(v: &Value) -> Option<String> {
    v.pointer("/metadata/asset_id").or_else(|| v.get("asset_id")).or_else(|| v.pointer("/metadata/id")).and_then(|x| x.as_str()).map(str::to_string)
}

/// CAMS name search within a single scope. `scope` is ("space_id"|"project_id", value).
/// Returns the first matching asset's id, or None (any error → None, so a search
/// that 403s on an empty scope reads as "absent" — the script_asset/synthetics pattern).
async fn find_asset_id_in_scope(client: &HttpClient, operation_id: &str, asset_type: &str, name: &str, scope: (&str, &str)) -> Result<Option<String>> {
    let path = format!("/v2/asset_types/{asset_type}/search");
    let body = json!({"query": format!("asset.name:\"{name}\""), "limit": 1});
    let spec = RequestSpec::new(Method::POST, &path).query_param(scope.0, scope.1).body(BodyKind::Json(body));
    let resp: Value = match client.execute(operation_id, spec).await {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    Ok(resp.get("results").and_then(|r| r.as_array()).and_then(|a| a.first()).and_then(extract_asset_id))
}

/// Build the Handled response carrying the computed ids + echoed state fields.
fn build_result(promoted_id: &str, source_id: &str, resource: &Value) -> Value {
    json!({
        "id": promoted_id,
        "source_asset_id": source_id,
        "asset_name": resource.get("asset_name").and_then(|v| v.as_str()).unwrap_or_default(),
        "asset_type": asset_type(resource),
    })
}

/// Discovery inputs resolved from the local data: `(asset_type, asset_name,
/// space_id, project_id?)`. `None` when `asset_name`/`space_id` are absent or
/// still `${...}`-templated (deps not reconciled yet — a from-scratch first
/// apply), in which case discovery reports nothing and the pipeline keeps its
/// CreateUnchecked → `pre_create`-adopt path. `project_id` is optional here:
/// the space search alone decides existence; the project search only back-fills
/// `source_asset_id`.
fn discovery_scope(data: &Value) -> Option<(String, String, String, Option<String>)> {
    let get = |field: &str| data.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty() && !s.contains("${")).map(str::to_string);
    let asset_name = get("asset_name")?;
    let space_id = get("space_id")?;
    Some((asset_type(data), asset_name, space_id, get("project_id")))
}

impl Reconciler for AssetPromotionReconciler {
    fn discover<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<RemoteResource>> + Send + 'a>> {
        Box::pin(async move {
            let mut matches = self.discover_all(operation_id, resource, client).await?;
            if matches.is_empty() { Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false }) } else { Ok(matches.swap_remove(0)) }
        })
    }

    /// Search the TARGET space for the promoted asset by name (the same lookup
    /// `pre_create`'s adopt step runs). Found ⇒ one remote shaped exactly like
    /// `pre_create`'s Handled result — `id`, `source_asset_id` (re-resolved from
    /// the source project when `project_id` is available), `asset_name`,
    /// `asset_type` — plus the scope ids, so downstream `${asset_promotion.x.*}`
    /// refs resolve from cached state on the NoChange path. Not found (or
    /// identity fields still templated) ⇒ empty, and the pipeline decides
    /// Create/CreateUnchecked as before.
    fn discover_all<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<Vec<RemoteResource>>> + Send + 'a>> {
        Box::pin(async move {
            let Some((at, asset_name, space_id, project_id)) = discovery_scope(&resource.data) else {
                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "skipping discovery: asset_name/space_id absent or unresolved");
                return Ok(vec![]);
            };
            let Some(promoted) = find_asset_id_in_scope(&client, operation_id, &at, &asset_name, ("space_id", &space_id)).await? else {
                return Ok(vec![]);
            };
            let source = match project_id.as_deref() {
                Some(p) => find_asset_id_in_scope(&client, operation_id, &at, &asset_name, ("project_id", p)).await?.unwrap_or_default(),
                None => String::new(),
            };
            let mut data = build_result(&promoted, &source, &resource.data);
            if let Some(obj) = data.as_object_mut() {
                obj.insert("space_id".to_string(), json!(space_id));
                if let Some(p) = project_id {
                    obj.insert("project_id".to_string(), json!(p));
                }
            }
            tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, promoted_id = %promoted, "promoted asset present in target space — discovered");
            Ok(vec![RemoteResource { key: resource.key.clone(), data, exists: true }])
        })
    }

    /// Existence IS convergence: the discovery search is already scoped by
    /// space + asset type and exact on the asset name (the kind's whole
    /// mutable surface), so a discovered remote can only be NoChange. A
    /// changed name/space/type simply stops matching ⇒ Create ⇒ `pre_create`
    /// promotes into the new identity.
    fn compare(&self, _local: &ValidatedResource, remote: &RemoteResource) -> StateComparison {
        if remote.exists { StateComparison::NoChange } else { StateComparison::Create }
    }
}

impl ResourceHandler for AssetPromotionHandler {
    /// Own the full create. Discovery is `skip`, so this runs on every apply and is
    /// the idempotency point:
    ///   1. Adopt — search the TARGET space for `asset_name`; if present, no re-promote
    ///      (re-resolve `source_asset_id` from the project for a stable computed value).
    ///   2. Resolve source — search the SOURCE project for `asset_name` → the id to
    ///      promote. Absent → explicit name-contract error (not a bare 404).
    ///   3. Promote — POST /v2/assets/{source}/promote?project_id=… {space_id}. A
    ///      409/400-already-exists falls back to the space-side adopt.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let asset_name = require_str(resource, "asset_name", operation_id)?.to_string();
            let project_id = require_str(resource, "project_id", operation_id)?.to_string();
            let space_id = require_str(resource, "space_id", operation_id)?.to_string();
            let at = asset_type(resource);

            // 1. Adopt — already promoted in the target space.
            if let Some(promoted) = find_asset_id_in_scope(client, operation_id, &at, &asset_name, ("space_id", &space_id)).await? {
                let source = find_asset_id_in_scope(client, operation_id, &at, &asset_name, ("project_id", &project_id)).await?.unwrap_or_default();
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "asset_promotion", asset_name = %asset_name, "adopt: promoted asset already present in space — skipping promote");
                return Ok(HookOutcome::Handled(build_result(&promoted, &source, resource)));
            }

            // 2. Resolve the project-side source id (the name contract).
            let source_id = find_asset_id_in_scope(client, operation_id, &at, &asset_name, ("project_id", &project_id)).await?.ok_or_else(|| anyhow!("[{operation_id}] asset_promotion: no {at} named '{asset_name}' in project {project_id} — the training script must store_model as '{asset_name}'"))?;

            // 3. Promote (project_id is a required query; space_id in the body).
            let promote_spec = RequestSpec::new(Method::POST, format!("/v2/assets/{source_id}/promote")).query_param("project_id", &project_id).body(BodyKind::Json(json!({"space_id": space_id})));
            let promoted_id = match client.execute::<Value>(operation_id, promote_spec).await {
                // Prefer the id in the promote response; fall back to a space name search (P1).
                Ok(resp) => match extract_asset_id(&resp) {
                    Some(id) => id,
                    None => find_asset_id_in_scope(client, operation_id, &at, &asset_name, ("space_id", &space_id)).await?.ok_or_else(|| anyhow!("[{operation_id}] asset_promotion: promote succeeded but no promoted id in the response or space {space_id}"))?,
                },
                // Duplicate — adopt by name in the space (the `duplicate_action` query is the
                // Phase-5 alternative; adopt-by-name needs no guessed enum value).
                Err(e) if error_matches(&e, 409, &[]) || error_matches(&e, 400, &["already", "exist"]) => {
                    find_asset_id_in_scope(client, operation_id, &at, &asset_name, ("space_id", &space_id)).await?.ok_or_else(|| anyhow!("[{operation_id}] asset_promotion: promote reported a duplicate but no {at} named '{asset_name}' in space {space_id}: {e}"))?
                }
                Err(e) => return Err(anyhow!("[{operation_id}] asset_promotion promote failed (source={source_id}, space={space_id}): {e}")),
            };
            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "asset_promotion", source_asset_id = %source_id, promoted_id = %promoted_id, "promoted asset into space");
            Ok(HookOutcome::Handled(build_result(&promoted_id, &source_id, resource)))
        })
    }

    /// Destroy deletes only the promoted (space-side) copy. Prefer the computed `id`;
    /// otherwise resolve it by name in the space. Unresolved `${...}` refs (parent
    /// absent) or an already-gone asset are idempotent no-ops (mirrors model_tracking).
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let asset_name = resource.get("asset_name").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let space_id = resource.get("space_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            let at = asset_type(resource);
            if asset_name.is_empty() || space_id.is_empty() || asset_name.contains("${") || space_id.contains("${") {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "asset_promotion", "space_id/asset_name unresolved — nothing to delete");
                return Ok(HookOutcome::Handled(json!({"deleted": false})));
            }
            let promoted_id = match resource.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty() && !s.contains("${")) {
                Some(id) => id.to_string(),
                None => match find_asset_id_in_scope(client, operation_id, &at, &asset_name, ("space_id", &space_id)).await? {
                    Some(id) => id,
                    None => return Ok(HookOutcome::Handled(json!({"deleted": false}))),
                },
            };
            let spec = RequestSpec::new(Method::DELETE, format!("/v2/assets/{promoted_id}")).query_param("space_id", &space_id).body(BodyKind::None);
            match client.execute::<Value>(operation_id, spec).await {
                Ok(_) => Ok(HookOutcome::Handled(json!({"deleted": true}))),
                Err(e) if error_matches(&e, 404, &[]) => {
                    tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "asset_promotion", promoted_id = %promoted_id, "promoted asset already absent on delete (404 tolerated)");
                    Ok(HookOutcome::Handled(json!({"deleted": true})))
                }
                Err(e) => Err(anyhow!("[{operation_id}] asset_promotion delete failed (id={promoted_id}, space={space_id}): {e}")),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // asset_type falls back to wml_model when absent/empty.
    #[test]
    fn asset_type_defaults_to_wml_model() {
        assert_eq!(asset_type(&json!({})), "wml_model");
        assert_eq!(asset_type(&json!({"asset_type": ""})), "wml_model");
        assert_eq!(asset_type(&json!({"asset_type": "data_asset"})), "data_asset");
    }

    // extract_asset_id scans metadata.asset_id, top-level asset_id, then metadata.id.
    #[test]
    fn extract_asset_id_scans_common_locations() {
        assert_eq!(extract_asset_id(&json!({"metadata": {"asset_id": "a-1"}})).as_deref(), Some("a-1"));
        assert_eq!(extract_asset_id(&json!({"asset_id": "a-2"})).as_deref(), Some("a-2"));
        assert_eq!(extract_asset_id(&json!({"metadata": {"id": "a-3"}})).as_deref(), Some("a-3"));
        assert_eq!(extract_asset_id(&json!({"nope": true})), None);
    }

    // build_result carries the computed ids plus echoed state fields.
    #[test]
    fn build_result_carries_computed_ids() {
        let r = json!({"asset_name": "churn_model", "asset_type": "wml_model"});
        let v = build_result("promoted-9", "source-1", &r);
        assert_eq!(v.get("id").and_then(|x| x.as_str()), Some("promoted-9"));
        assert_eq!(v.get("source_asset_id").and_then(|x| x.as_str()), Some("source-1"));
        assert_eq!(v.get("asset_name").and_then(|x| x.as_str()), Some("churn_model"));
        assert_eq!(v.get("asset_type").and_then(|x| x.as_str()), Some("wml_model"));
    }

    // discovery_scope gates the custom reconciler's search: resolved identity
    // fields pass through; an absent or still-templated asset_name/space_id
    // (from-scratch first apply) yields None so discovery reports nothing and
    // the CreateUnchecked → pre_create-adopt path is preserved. project_id is
    // optional — only source_asset_id back-fill needs it.
    #[test]
    fn discovery_scope_requires_resolved_identity_fields() {
        let resolved = json!({"asset_name": "m", "asset_type": "wml_model", "space_id": "s-1", "project_id": "p-1"});
        assert_eq!(discovery_scope(&resolved), Some(("wml_model".into(), "m".into(), "s-1".into(), Some("p-1".into()))));

        // project_id templated → scope still usable, just without a source search.
        let no_project = json!({"asset_name": "m", "space_id": "s-1", "project_id": "${project.x}"});
        assert_eq!(discovery_scope(&no_project), Some(("wml_model".into(), "m".into(), "s-1".into(), None)));

        // space_id templated / absent → no discovery.
        assert_eq!(discovery_scope(&json!({"asset_name": "m", "space_id": "${space.x}"})), None);
        assert_eq!(discovery_scope(&json!({"asset_name": "m"})), None);
        // asset_name templated → no discovery.
        assert_eq!(discovery_scope(&json!({"asset_name": "${x.y}", "space_id": "s-1"})), None);
    }

    // The custom compare is existence-only: a discovered remote is NoChange
    // (the search already matched space + type + exact name), absence is Create.
    #[test]
    fn reconciler_compare_is_existence_only() {
        use wxctl_core::ResourceKey;
        let key = ResourceKey::new("asset_promotion", "promote_model");
        let schema = wxctl_schema::load_all_schemas().unwrap().into_iter().find(|s| s.resource.kind == "asset_promotion").expect("asset_promotion schema present");
        let local = ValidatedResource { key: key.clone(), data: json!({"asset_name": "m"}), descriptor: std::sync::Arc::new(wxctl_core::registry::ResourceDescriptor::from_schema(&schema).unwrap()), dependencies: vec![], on_destroy: wxctl_core::OnDestroyPolicy::Delete };
        let found = RemoteResource { key: key.clone(), data: json!({"id": "a-1"}), exists: true };
        let absent = RemoteResource { key, data: Value::Null, exists: false };
        assert!(matches!(AssetPromotionReconciler.compare(&local, &found), StateComparison::NoChange));
        assert!(matches!(AssetPromotionReconciler.compare(&local, &absent), StateComparison::Create));
    }
}
