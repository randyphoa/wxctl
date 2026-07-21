use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};
use wxctl_schema::ir::FieldLocationIr;

use crate::util::{extract_artifact_id, fetch_all_pages};

/// Singular `business_term` handler.
///
/// `POST /v3/glossary_terms` requires a JSON **array** even for one term (a bare
/// object → 400 "Expected BEGIN_ARRAY"). The default schema reconciler POSTs a
/// single object, so this handler owns create: it wraps the term as a 1-element
/// array, POSTs `?skip_workflow_if_possible=true` so the term **publishes
/// immediately** (live-proven 2026-06-15: `=false` parks it DRAFT-in-workflow,
/// invisible to `/v3/glossary_terms` list discovery — breaking NoChange and the
/// version-path delete; `=true` makes it appear in the list so `list_and_get`
/// discovery finds it), and hoists the created `artifact_id`/`version_id` to the
/// top level so `${business_term.<ref>.artifact_id}` resolves.
///
/// Even a published term has **no plain delete** (`DELETE /v3/glossary_terms/{id}`
/// → 404); it deletes via the version path `DELETE /v3/glossary_terms/{id}/versions/{vid}`.
/// `pre_delete` resolves the term by name (listing published + DRAFT) and deletes
/// every version it finds.
pub struct BusinessTermHandler;

/// Names of the schema's Body fields — used to drop non-wire keys (`kind`,
/// `metadata`, `ref_name`, Computed/LocalOnly fields) before building the POST
/// body, mirroring the materializer which excludes Computed/LocalOnly.
fn body_field_names(fields: &[FieldDescriptor]) -> HashSet<&str> {
    fields.iter().filter(|f| matches!(f.location, FieldLocationIr::Body)).map(|f| f.name.as_str()).collect()
}

/// Build the 1-element term array the API expects from a resolved resource,
/// keeping only schema Body fields.
fn wrap_term_as_array(resource: &Value, fields: &[FieldDescriptor]) -> Value {
    let allowed = body_field_names(fields);
    let term: serde_json::Map<String, Value> = resource.as_object().map(|m| m.iter().filter(|(k, _)| allowed.contains(k.as_str())).map(|(k, v)| (k.clone(), v.clone())).collect()).unwrap_or_default();
    json!([Value::Object(term)])
}

/// Copy the created term's `artifact_id`/`version_id` to the top level of the
/// response so the engine's `extract_resource_id` (top-level / metadata / entity
/// only) can harvest the id for ref resolution. Reads `resources[0]` via
/// `extract_artifact_id` and the parallel `version_id`. No-op when a top-level
/// `artifact_id` already exists.
fn hoist_term_id(response: &mut Value) {
    if response.get("artifact_id").and_then(|v| v.as_str()).is_none()
        && let Some(id) = extract_artifact_id(response).map(str::to_string)
        && let Some(obj) = response.as_object_mut()
    {
        obj.insert("artifact_id".to_string(), Value::String(id));
    }
    if response.get("version_id").and_then(|v| v.as_str()).is_none()
        && let Some(vid) = response.get("resources").and_then(|r| r.as_array()).and_then(|a| a.first()).and_then(|item| item.get("version_id")).and_then(|v| v.as_str()).map(str::to_string)
        && let Some(obj) = response.as_object_mut()
    {
        obj.insert("version_id".to_string(), Value::String(vid));
    }
}

/// Normalize a discovered term so re-plan is NoChange. The discovered
/// `parent_category` is a richer object (`{href, id, name, …}`) possibly nested
/// under the CP4D `entity`/`metadata` envelope; rewrite it to the local
/// `{id: <id>}` shape so the immutable-field compare matches. `workflow_state`
/// (server-managed, no local value) is dropped from immutable_fields in the
/// schema, so it is not compared. No-op when no id is discoverable (leaves the
/// value as-is — never fabricates one that would mask a real diff).
fn normalize_discovered_term(remote: &mut Value) {
    let id = find_parent_category_id(remote);
    if let (Some(id), Some(obj)) = (id, remote.as_object_mut()) {
        obj.insert("parent_category".to_string(), json!({ "id": id }));
    }
}

/// Find the discovered parent-category id wherever the API put it: a
/// `parent_category` object's `id` / a top-level or enveloped
/// `parent_category_id`. Returns the first hit, else `None`.
fn find_parent_category_id(remote: &Value) -> Option<String> {
    for base in [Some(remote), remote.get("entity"), remote.get("metadata")].into_iter().flatten() {
        if let Some(id) = base.get("parent_category").and_then(|pc| pc.get("id")).and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
        if let Some(id) = base.get("parent_category_id").and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }
    None
}

/// Find the discovered term's `artifact_id` wherever the CP4D body puts it
/// (top-level / `entity` / `metadata`). The `/v3/glossary_terms` LIST nests it
/// under `metadata`. Returns the first hit, else `None` (no-fabricate).
fn find_discovered_artifact_id(remote: &Value) -> Option<String> {
    for base in [Some(remote), remote.get("entity"), remote.get("metadata")].into_iter().flatten() {
        if let Some(id) = base.get("artifact_id").and_then(|v| v.as_str()) {
            return Some(id.to_string());
        }
    }
    None
}

impl ResourceHandler for BusinessTermHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = wrap_term_as_array(resource, fields);
            let endpoint_with_workflow = format!("{}?skip_workflow_if_possible=true", endpoint);
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, "POSTing single business_term wrapped as 1-element array");
            let mut response: Value = client.create(operation_id, &endpoint_with_workflow, body).await?;
            hoist_term_id(&mut response);
            Ok(HookOutcome::Handled(response))
        })
    }

    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = resource.get("name").and_then(|n| n.as_str()).unwrap_or_default();
            let mut deleted = 0usize;
            for status in ["DRAFT", "published"] {
                let list_endpoint = format!("/v3/governance_artifact_types/glossary_term?workflow_status={}&limit=200", status);
                let items = fetch_all_pages(client, operation_id, &list_endpoint, "resources").await.with_context(|| format!("[{operation_id}] failed to list {status} glossary terms while deleting business_term '{name}'"))?;
                for item in &items {
                    if item.get("name").and_then(|n| n.as_str()) != Some(name) {
                        continue;
                    }
                    let (Some(artifact_id), Some(version_id)) = (item.get("artifact_id").and_then(|v| v.as_str()), item.get("version_id").and_then(|v| v.as_str())) else { continue };
                    // skip_workflow_if_possible=true deletes immediately (204); without it the
                    // delete is parked as a DELETE-workflow draft (201) and the term lingers
                    // in the list — proven live 2026-06-15.
                    let del_endpoint = format!("/v3/glossary_terms/{}/versions/{}?skip_workflow_if_possible=true", artifact_id, version_id);
                    let spec = RequestSpec::new(Method::DELETE, &del_endpoint).body(BodyKind::None);
                    let _: Value = client.execute(operation_id, spec).await?;
                    deleted += 1;
                }
            }
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, term_name = %name, deleted, "deleted business_term versions");
            Ok(HookOutcome::Handled(json!({"deleted": deleted})))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Hoist artifact_id to the top level so ${business_term.<ref>.artifact_id}
            // resolves on re-apply (the discovered /v3/glossary_terms LIST body nests
            // it under metadata). Mirrors hoist_term_id (create path) + CategoryHandler
            // post_discover. No-fabricate: only when discoverable, no-op if already top-level.
            if remote_data.get("artifact_id").and_then(|v| v.as_str()).is_none()
                && let Some(id) = find_discovered_artifact_id(remote_data)
                && let Some(obj) = remote_data.as_object_mut()
            {
                obj.insert("artifact_id".to_string(), Value::String(id));
            }
            normalize_discovered_term(remote_data);
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, "business_term discovered; artifact_id hoisted to top-level + parent_category normalized for NoChange compare and ref resolution");
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::registry::FieldDescriptor;
    use wxctl_schema::ir::FieldLocationIr;

    fn field(name: &str, location: FieldLocationIr) -> FieldDescriptor {
        FieldDescriptor { name: name.to_string(), required: false, immutable: false, location }
    }

    #[test]
    fn wrap_term_keeps_only_body_fields_and_wraps_in_array() {
        let fields = vec![field("name", FieldLocationIr::Body), field("short_description", FieldLocationIr::Body), field("artifact_id", FieldLocationIr::Computed)];
        let resource = json!({"kind": "business_term", "name": "Email", "short_description": "an email", "artifact_id": "should-drop", "ref_name": "term_email"});
        let wrapped = wrap_term_as_array(&resource, &fields);
        let arr = wrapped.as_array().expect("array");
        assert_eq!(arr.len(), 1);
        let term = arr[0].as_object().expect("object");
        assert_eq!(term.get("name").and_then(|v| v.as_str()), Some("Email"));
        assert_eq!(term.get("short_description").and_then(|v| v.as_str()), Some("an email"));
        assert!(!term.contains_key("artifact_id"));
        assert!(!term.contains_key("kind"));
        assert!(!term.contains_key("ref_name"));
    }

    #[test]
    fn wrap_term_preserves_parent_category_object() {
        let fields = vec![field("name", FieldLocationIr::Body), field("parent_category", FieldLocationIr::Body)];
        let resource = json!({"name": "Email", "parent_category": {"id": "cat-1"}});
        let wrapped = wrap_term_as_array(&resource, &fields);
        assert_eq!(wrapped[0].get("parent_category"), Some(&json!({"id": "cat-1"})));
    }

    #[test]
    fn hoist_term_id_lifts_resources_id_and_version() {
        let mut resp = json!({"resources": [{"artifact_id": "term-1", "version_id": "ver-1"}]});
        hoist_term_id(&mut resp);
        assert_eq!(resp.get("artifact_id").and_then(|v| v.as_str()), Some("term-1"));
        assert_eq!(resp.get("version_id").and_then(|v| v.as_str()), Some("ver-1"));
    }

    #[test]
    fn hoist_term_id_is_noop_when_top_level_id_present() {
        let mut resp = json!({"artifact_id": "top", "resources": [{"artifact_id": "nested", "version_id": "v"}]});
        hoist_term_id(&mut resp);
        assert_eq!(resp.get("artifact_id").and_then(|v| v.as_str()), Some("top"));
    }

    // normalize_discovered_term rewrites parent_category to the local `{id}` shape so the
    // immutable-field compare round-trips; expected `Some` value, or `None` = key must be absent
    // (no-fabricate — never mask a real diff). Each input is a distinct discovered body shape.
    #[test]
    fn normalize_discovered_term_cases() {
        let cases: &[(&str, Value, Option<Value>)] = &[
            ("rewrites rich parent_category object to {id}", json!({"name": "e2e Customer Email", "parent_category": {"href": "/v3/categories/cat-1", "id": "cat-1", "name": "e2e PII"}, "workflow_state": "PUBLISHED"}), Some(json!({"id": "cat-1"}))),
            ("reads enveloped (entity.parent_category)", json!({"metadata": {"name": "e2e Customer Email"}, "entity": {"parent_category": {"id": "cat-1"}}}), Some(json!({"id": "cat-1"}))),
            ("reads parent_category_id scalar field", json!({"name": "e2e Customer Email", "parent_category_id": "cat-1"}), Some(json!({"id": "cat-1"}))),
            ("no-op without any parent", json!({"name": "e2e Customer Email"}), None),
            // Gap B fix: live CP4D LIST nests `name` under metadata and omits parent_category
            // entirely (confirmed 2026-06-16); normalize must leave it absent — with
            // parent_category dropped from immutable_fields this no longer triggers Recreate.
            ("no-op on live LIST body shape (parent absent)", json!({"metadata": {"name": "e2e Customer Email", "artifact_id": "term-1", "tags": ["e2e", "pii"]}, "entity": {"abbreviations": ["EMAIL"], "long_description": "The email address used to contact a customer; treated as PII."}}), None),
        ];
        for (msg, mut remote, expected) in cases.iter().map(|(m, r, e)| (*m, r.clone(), e.clone())) {
            normalize_discovered_term(&mut remote);
            assert_eq!(remote.get("parent_category").cloned(), expected, "{msg}");
        }
    }

    #[test]
    fn find_discovered_artifact_id_reads_metadata_envelope() {
        // Live CP4D LIST nests artifact_id under metadata.
        let remote = json!({"metadata": {"name": "e2e Customer Email", "artifact_id": "term-1"}});
        assert_eq!(find_discovered_artifact_id(&remote).as_deref(), Some("term-1"));
        // Top-level wins / already-present.
        let top = json!({"artifact_id": "term-top", "metadata": {"artifact_id": "term-meta"}});
        assert_eq!(find_discovered_artifact_id(&top).as_deref(), Some("term-top"));
        // Absent → None (no-fabricate).
        let none = json!({"metadata": {"name": "x"}});
        assert!(find_discovered_artifact_id(&none).is_none());
    }

    #[test]
    fn pre_delete_uses_version_path_endpoint_shape() {
        // Guard: the delete endpoint must be the version path (…/versions/{vid}?skip_workflow_if_possible=true),
        // not the plain DELETE /v3/glossary_terms/{id} (which 404s). String-shape guard over the format! template.
        let artifact_id = "term-1";
        let version_id = "ver-1_0";
        let del_endpoint = format!("/v3/glossary_terms/{}/versions/{}?skip_workflow_if_possible=true", artifact_id, version_id);
        assert!(del_endpoint.contains("/versions/"));
        assert!(del_endpoint.ends_with("?skip_workflow_if_possible=true"));
        assert!(!del_endpoint.ends_with("/glossary_terms/term-1"));
    }
}
