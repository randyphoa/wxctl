//! `common_core/environment` handler — platform notebook runtimes are **adopted,
//! never created**. Live evidence (SaaS + CP4D) refutes a generic create body:
//! `POST /v2/environments` rejects `{"display_name":...,"type":"notebook"}` with
//! 4 missing-field errors (`name`, `software_specification`, `hardware_specification`,
//! `tools_specification` all required) — custom-environment creation is out of
//! scope, so this handler never issues that POST.
//!
//! Discovery is `list_and_get` (matches the generic reconciler's normal adopt
//! path), but that path has an engine gap: when `project_id` references a
//! project created in the SAME apply, the identity-relevant path is still an
//! unresolved `${...}` template at plan time, so reconciliation logs
//! "skipping discovery_all" and emits `CreateUnchecked` — at execution time
//! the engine blind-creates without ever re-discovering. `pre_create` closes
//! that gap by owning the full create: it re-runs the same list + name-match
//! adopt logic itself (now that `project_id` has resolved), so the adopt path
//! fires even when the generic reconciler's discovery was skipped.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct EnvironmentHandler;

fn require_str<'a>(resource: &'a Value, field: &str, operation_id: &str) -> Result<&'a str> {
    resource.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{operation_id}] environment requires '{field}'"))
}

/// The list response's entries live under `resources` (the documented CAMS shape);
/// fall back to a bare top-level array or an `environments` key for tolerance.
fn extract_entries(list: &Value) -> Vec<&Value> {
    if let Some(arr) = list.get("resources").and_then(|v| v.as_array()) {
        return arr.iter().collect();
    }
    if let Some(arr) = list.as_array() {
        return arr.iter().collect();
    }
    if let Some(arr) = list.get("environments").and_then(|v| v.as_array()) {
        return arr.iter().collect();
    }
    Vec::new()
}

/// An entry's display name: entity.environment.display_name (CAMS envelope),
/// metadata.name, top-level display_name, then top-level name.
fn extract_display_name(entry: &Value) -> Option<String> {
    entry.pointer("/entity/environment/display_name").or_else(|| entry.pointer("/metadata/name")).or_else(|| entry.get("display_name")).or_else(|| entry.get("name")).and_then(|v| v.as_str()).map(str::to_string)
}

/// An entry's guid: metadata.asset_id, metadata.guid, top-level guid, then top-level id.
fn extract_guid(entry: &Value) -> Option<String> {
    entry.pointer("/metadata/asset_id").or_else(|| entry.pointer("/metadata/guid")).or_else(|| entry.get("guid")).or_else(|| entry.get("id")).and_then(|v| v.as_str()).map(str::to_string)
}

/// GET /v2/environments?types=notebook&project_id=<id> and return its entries.
async fn list_notebook_environments(client: &HttpClient, operation_id: &str, project_id: &str) -> Result<Vec<Value>> {
    let spec = RequestSpec::new(Method::GET, "/v2/environments").query_param("types", "notebook").query_param("project_id", project_id);
    let resp: Value = client.execute(operation_id, spec).await.map_err(|e| anyhow!("[{operation_id}] environment: listing notebook environments failed (project_id={project_id}): {e}"))?;
    Ok(extract_entries(&resp).into_iter().cloned().collect())
}

/// Build the Handled adoption payload: the matched entry with the guid hoisted
/// top-level (so downstream `${environment.x}` refs resolve) and display_name echoed.
fn build_adopted(entry: &Value, guid: &str, display_name: &str) -> Value {
    let mut adopted = entry.clone();
    if let Some(obj) = adopted.as_object_mut() {
        obj.insert("guid".to_string(), json!(guid));
        obj.insert("display_name".to_string(), json!(display_name));
    }
    adopted
}

impl ResourceHandler for EnvironmentHandler {
    /// Own the full create. Platform runtimes are adopted by `display_name`,
    /// never created — this is the ONLY path that ever runs for `environment`
    /// (discovery's normal adopt path lands here too when a match is found via
    /// the generic reconciler; the CreateUnchecked/deferred-ref path lands here
    /// having never discovered at all). Re-runs the list + match itself so
    /// adoption succeeds either way. No match ⇒ explicit error — NEVER falls
    /// through to a POST (the create API requires name + hardware/software/tools
    /// specification objects that this schema-only kind does not model).
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let display_name = require_str(resource, "display_name", operation_id)?.to_string();
            let project_id = require_str(resource, "project_id", operation_id)?.to_string();

            let entries = list_notebook_environments(client, operation_id, &project_id).await?;

            let mut found_names = Vec::with_capacity(entries.len());
            for entry in &entries {
                let Some(name) = extract_display_name(entry) else { continue };
                if name == display_name {
                    let guid = extract_guid(entry).ok_or_else(|| anyhow!("[{operation_id}] environment '{display_name}' matched in project {project_id} but no guid found in the list entry: {entry}"))?;
                    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "environment", display_name = %display_name, project_id = %project_id, guid = %guid, "adopted platform runtime by display_name");
                    return Ok(HookOutcome::Handled(build_adopted(entry, &guid, &name)));
                }
                found_names.push(name);
            }

            Err(anyhow!("[{operation_id}] environment '{display_name}' not found in project {project_id} — platform runtimes are adopted, not created (custom-environment creation is not supported); available notebook runtimes: [{}]", found_names.join(", ")))
        })
    }

    /// Hoist guid + display_name top-level on discovery (list_and_get GETs the
    /// bare CAMS entry) so `${environment.x}` refs and state comparison see them
    /// regardless of which path (generic reconciler adopt vs. pre_create adopt)
    /// produced the remote data.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let guid = extract_guid(remote_data);
            let display_name = extract_display_name(remote_data);
            if let Some(obj) = remote_data.as_object_mut() {
                if let Some(g) = guid {
                    obj.entry("guid".to_string()).or_insert(json!(g));
                }
                if let Some(n) = display_name {
                    obj.entry("display_name".to_string()).or_insert(json!(n));
                }
            }
            Ok(())
        })
    }

    /// Adopted platform runtimes are shared, never owned by this apply — destroy
    /// is unconditionally a no-op so tearing down the chain never removes the
    /// default runtime out from under other projects/users.
    fn pre_delete<'a>(&'a self, _resource: &'a Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "environment", "adopted platform runtime — nothing to delete");
            Ok(HookOutcome::Handled(json!({"deleted": false})))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // extract_entries reads resources[], falling back to a bare array, then environments[].
    #[test]
    fn extract_entries_scans_common_shapes() {
        assert_eq!(extract_entries(&json!({"resources": [{"a":1}]})).len(), 1);
        assert_eq!(extract_entries(&json!([{"a":1},{"b":2}])).len(), 2);
        assert_eq!(extract_entries(&json!({"environments": [{"a":1}]})).len(), 1);
        assert_eq!(extract_entries(&json!({"nope": true})).len(), 0);
    }

    // extract_display_name scans the CAMS envelope, metadata.name, then top-level keys.
    #[test]
    fn extract_display_name_scans_common_locations() {
        assert_eq!(extract_display_name(&json!({"entity": {"environment": {"display_name": "Runtime 25.1 on Python 3.12 XXS"}}})).as_deref(), Some("Runtime 25.1 on Python 3.12 XXS"));
        assert_eq!(extract_display_name(&json!({"metadata": {"name": "n2"}})).as_deref(), Some("n2"));
        assert_eq!(extract_display_name(&json!({"display_name": "n3"})).as_deref(), Some("n3"));
        assert_eq!(extract_display_name(&json!({"name": "n4"})).as_deref(), Some("n4"));
        assert_eq!(extract_display_name(&json!({"nope": true})), None);
    }

    // extract_guid scans metadata.asset_id, metadata.guid, then top-level guid/id.
    #[test]
    fn extract_guid_scans_common_locations() {
        assert_eq!(extract_guid(&json!({"metadata": {"asset_id": "g-1"}})).as_deref(), Some("g-1"));
        assert_eq!(extract_guid(&json!({"metadata": {"guid": "g-2"}})).as_deref(), Some("g-2"));
        assert_eq!(extract_guid(&json!({"guid": "g-3"})).as_deref(), Some("g-3"));
        assert_eq!(extract_guid(&json!({"id": "g-4"})).as_deref(), Some("g-4"));
        assert_eq!(extract_guid(&json!({"nope": true})), None);
    }

    // Drift guard for the re-apply NoChange fix: the list entry nests the display
    // name at entity.environment.display_name (live-verified 2026-07-05, SaaS +
    // CP4D — metadata.name is a short code like `rt251pys1`), which a plain
    // name_field lookup cannot reach. The schema must declare identity_match
    // pointing at that envelope path or discovery silently matches nothing and
    // every re-apply re-plans Create.
    #[test]
    fn environment_schema_identity_match_targets_cams_envelope() {
        let schema = wxctl_schema::ir::RESOURCE_IR.get("environment").copied().expect("kind in RESOURCE_IR");
        let im = schema.resource.reconciliation.discovery.identity_match.as_ref().expect("environment discovery must declare identity_match");
        assert_eq!(im.local_path, "display_name");
        assert_eq!(im.remote_path, "entity.environment.display_name");
    }

    // build_adopted hoists guid + display_name top-level while preserving the entry.
    #[test]
    fn build_adopted_hoists_guid_and_name() {
        let entry = json!({"metadata": {"asset_id": "g-1", "name": "Runtime 25.1 on Python 3.12"}});
        let adopted = build_adopted(&entry, "g-1", "Runtime 25.1 on Python 3.12");
        assert_eq!(adopted.get("guid").and_then(|v| v.as_str()), Some("g-1"));
        assert_eq!(adopted.get("display_name").and_then(|v| v.as_str()), Some("Runtime 25.1 on Python 3.12"));
        assert_eq!(adopted.pointer("/metadata/asset_id").and_then(|v| v.as_str()), Some("g-1"));
    }
}
