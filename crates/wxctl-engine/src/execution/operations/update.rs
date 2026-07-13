use super::super::ExecutionState;
use super::super::resolution::{enrich_with_linked_refs, extract_resource_id, merge_request_response, resolve_dependencies};
use super::super::types::ExecutionResult;
use crate::reconciliation::types::OperationType;
use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value};
use tracing::{Instrument, info_span};
use wxctl_core::client::{BodyKind, BodyKindSelector, RequestMaterializer};
use wxctl_core::logging::redact_for_log;
use wxctl_core::traits::{HookOutcome, StateComparison};
use wxctl_schema::schema::FieldDefinition;

/// Prune a materialized request body down to the keys that `state_fields`
/// allows on plain-JSON PATCH/PUT updates.
///
/// `state_fields` entries are **local snake_case field names** (schema
/// `FieldDefinition::name`), but the materialized body is keyed by the
/// **api_field-mapped** name (`RequestMaterializer` writes each field under
/// `field.api_field.unwrap_or(&field.name)` — see `materializer.rs`). When a
/// field's `api_field` differs from its local name (e.g. Planning Analytics'
/// `rules` field maps to api_field `Rules`), comparing `state_fields` values
/// directly against body keys never matches, so every key gets pruned and the
/// PATCH goes out with an empty `{}` body — the server 200s a no-op while
/// wxctl reports "updated" (live-proven on TM1: a cube's `rules` update
/// silently no-op'd, `REQ: {}` in the run record).
///
/// For each state_field, resolve it to the schema field with a matching
/// `name`, then take the first dot-segment of that field's `api_field` (or
/// the field name itself if no `api_field`) as the allowed top-level body
/// key — dotted `api_field` paths (`"a.b"`) materialize as a nested object
/// under top-level key `"a"` (`materializer.rs::insert_nested`), so only the
/// first segment is ever a real body key. A state_field with no matching
/// schema field keeps the previous defensive behavior of allowing the raw
/// name through unchanged.
fn prune_body_to_state_fields(map: &mut Map<String, Value>, state_fields: &[String], fields: &[FieldDefinition]) {
    let allowed_keys: std::collections::HashSet<&str> = state_fields
        .iter()
        .map(|s| match fields.iter().find(|f| &f.name == s) {
            Some(f) => f.api_field.as_deref().unwrap_or(s.as_str()).split('.').next().unwrap_or(s.as_str()),
            None => s.as_str(),
        })
        .collect();
    map.retain(|k, _| allowed_keys.contains(k.as_str()));
}

pub(super) async fn execute<'a>(planned_op: &'a crate::reconciliation::types::Operation, state: &'a ExecutionState) -> Result<ExecutionResult> {
    let op = planned_op;
    let local = op.local.as_ref().ok_or_else(|| anyhow!("No local resource for operation"))?;
    let descriptor = &local.descriptor;
    let endpoints = &descriptor.endpoints;
    let operation_id = &state.operation_id;
    let client = state.clients.get(&descriptor.service).ok_or_else(|| anyhow!("No client for service: {}", descriptor.service))?;

    let mut fields = match &op.op_type {
        OperationType::Update { fields } => fields.clone(),
        _ => return Err(anyhow!("Expected Update operation")),
    };

    let mut resolved_data = resolve_dependencies(&local.data, &state.runtime_ids, &descriptor.schema)?;

    // Reconciliation emits `Update { fields: vec![] }` when compare() couldn't
    // run because a dependency wasn't yet created. The DAG executor has since
    // populated runtime_ids, so re-compare here to recover a real diff.
    if fields.is_empty() {
        if let Some(remote) = op.remote.as_ref()
            && let Some(reconciler) = state.registry.get_reconciler(&descriptor.name)
        {
            let resolved_local = wxctl_core::ValidatedResource { data: resolved_data.clone(), ..local.clone() };
            if let StateComparison::Update { fields: recomputed } = reconciler.compare(&resolved_local, remote)
                && !recomputed.is_empty()
            {
                tracing::debug!(target: "wxctl::substage::execution", operation_id = %operation_id, resource_type = %descriptor.name, resource_name = %local.key.name, fields = ?recomputed, "recovered deferred-resolution Update with execution-time diff");
                fields = recomputed;
            }
        }
        if fields.is_empty() {
            let response = op.remote.as_ref().map(|r| r.data.clone());
            return Ok(ExecutionResult { key: op.key.clone(), operation: OperationType::Update { fields }, success: true, response, error: None, attempts: 1 });
        }
    }

    enrich_with_linked_refs(&mut resolved_data, &local.data, &state.runtime_ids, &descriptor.schema, &state.registry);
    let current = op.remote.as_ref().map(|r| &r.data).ok_or_else(|| anyhow!("No remote data for update"))?;

    let resource_id = extract_resource_id(current, &descriptor.id_field).ok_or_else(|| anyhow!("Missing ID field for update"))?;

    let endpoint_template = endpoints.update.as_ref().unwrap_or(&endpoints.get);

    let method = match &endpoints.update_method {
        Some(wxctl_schema::schema::HttpMethod::Put) => Method::PUT,
        Some(wxctl_schema::schema::HttpMethod::Patch) => Method::PATCH,
        Some(wxctl_schema::schema::HttpMethod::Post) => Method::POST,
        _ => Method::PATCH,
    };

    let use_json_patch = descriptor.schema.resource.reconciliation.use_json_patch;
    let body_selector = if method == Method::PATCH && use_json_patch {
        let path_prefix = descriptor.schema.resource.reconciliation.json_patch_path_prefix.as_ref().ok_or_else(|| anyhow!("json_patch_path_prefix required"))?.clone();
        BodyKindSelector::JsonPatch { changed_fields: fields.clone(), path_prefix, fields: &descriptor.schema.resource.schema.fields }
    } else {
        BodyKindSelector::Json
    };

    // Allow handler to modify resolved_data before materialization
    let mut body = resolved_data.clone();
    if let Some(handler) = state.registry.get_handler(&descriptor.name) {
        let payload_before = body.clone();
        let pre_update_span = info_span!(target: "wxctl::substage::hook", "pre_update", operation_id = %operation_id, hook = "pre_update", handler_kind = %descriptor.name, resource_kind = %local.key.kind, resource_name = %local.key.name);
        let pre_update_result = handler.pre_update(current, &mut body, &descriptor.fields, client, endpoint_template, operation_id).instrument(pre_update_span).await?;
        match pre_update_result {
            HookOutcome::Handled(response) => {
                return Ok(ExecutionResult { key: op.key.clone(), operation: OperationType::Update { fields: fields.clone() }, success: true, response: Some(response), error: None, attempts: 1 });
            }
            HookOutcome::Continue => {
                // Handler modified body, continue with materialization
                let sensitive = descriptor.schema.resource.schema.sensitive_paths();
                tracing::debug!(target: "wxctl::substage::hook", operation_id = %operation_id, hook = "pre_update", handler_kind = %descriptor.name, current = %serde_json::to_string(&redact_for_log(current, &sensitive)).unwrap_or_default(), before = %serde_json::to_string(&redact_for_log(&payload_before, &sensitive)).unwrap_or_default(), after = %serde_json::to_string(&redact_for_log(&body, &sensitive)).unwrap_or_default(), "hook payload diff");
            }
        }
    }

    // Materialize from handler-modified body (includes injected fields like input_schema)
    let materializer = RequestMaterializer::new(method.clone(), endpoint_template);
    let mut final_spec = materializer.materialize(&body, &descriptor.schema.resource.schema.fields, body_selector)?;

    // For non-JSON-Patch PATCH/PUT updates, prune the body to schema state_fields so
    // that immutable / computed fields aren't re-sent on update. Without this filter
    // every materialized body field reaches the API, which rejects immutable fields
    // (e.g. database_registration's `connection` ref) on PATCH. See
    // `prune_body_to_state_fields` for why this must translate local field names to
    // api_field-mapped body keys rather than comparing them directly.
    if !use_json_patch
        && (method == Method::PATCH || method == Method::PUT)
        && let BodyKind::Json(Value::Object(ref mut map)) = final_spec.body
        && let Some(allowed) = descriptor.schema.resource.reconciliation.state_fields.as_ref()
    {
        prune_body_to_state_fields(map, allowed, &descriptor.schema.resource.schema.fields);
    }

    final_spec.path_vars.insert(descriptor.id_field.clone(), resource_id.clone());

    let mut response: Value = client.execute(operation_id, final_spec).await?;

    if let Some(handler) = state.registry.get_handler(&descriptor.name) {
        // Inject the resource ID into body so post_update handler has access to it
        // This is necessary because the local resource data doesn't include the remote ID
        let mut body_with_id = body.clone();
        if let Value::Object(ref mut map) = body_with_id {
            map.insert(descriptor.id_field.clone(), Value::String(resource_id));
        }
        let post_update_span = info_span!(target: "wxctl::substage::hook", "post_update", operation_id = %operation_id, hook = "post_update", handler_kind = %descriptor.name, resource_kind = %local.key.kind, resource_name = %local.key.name);
        handler.post_update(&body_with_id, &mut response, client, operation_id).instrument(post_update_span).await?;
    }

    // Merge remote data with response so computed fields (like id) are always
    // available for dependent resource reference resolution.
    // The API may return Null (204 No Content) or a partial response (e.g., just
    // a warning object), so we use remote data as the base and overlay with both
    // resolved local data and the API response.
    let merged = if response.is_null() { merge_request_response(current, &resolved_data) } else { merge_request_response(current, &response) };

    Ok(ExecutionResult { key: op.key.clone(), operation: OperationType::Update { fields: fields.clone() }, success: true, response: Some(merged), error: None, attempts: 1 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_schema::schema::FieldLocation;

    fn make_field(name: &str, api_field: Option<&str>) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: wxctl_schema::schema::FieldType::String,
            required: false,
            immutable: false,
            location: FieldLocation::Body,
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references: None,
            api_field: api_field.map(|s| s.to_string()),
            sensitive: false,
            also_query: false,
            is_path: false,
            synthesize: None,
            synth_shape: None,
            properties: None,
        }
    }

    /// Regression test for the live-proven TM1 bug: state_fields are local
    /// snake_case names but the materialized body is keyed by api_field
    /// (e.g. PascalCase). Pruning must translate `rules` -> `Rules` rather
    /// than comparing the raw names, or the PATCH body goes out empty.
    #[test]
    fn prune_body_to_state_fields_translates_api_field_names() {
        let fields = vec![make_field("name", Some("Name")), make_field("rules", Some("Rules")), make_field("dimensions", None)];
        let state_fields = vec!["name".to_string(), "rules".to_string()];

        let mut body = json!({
            "Name": "x",
            "Rules": "SKIPCHECK;",
            "dimensions": ["a"]
        })
        .as_object()
        .unwrap()
        .clone();

        prune_body_to_state_fields(&mut body, &state_fields, &fields);

        assert_eq!(Value::Object(body), json!({"Name": "x", "Rules": "SKIPCHECK;"}));
    }

    /// A state_field with no matching schema field falls back to allowing the
    /// raw name through unchanged (defensive prior behavior).
    #[test]
    fn prune_body_to_state_fields_keeps_unmatched_state_field_raw() {
        let fields = vec![make_field("name", Some("Name"))];
        let state_fields = vec!["name".to_string(), "ghost_field".to_string()];

        let mut body = json!({"Name": "x", "ghost_field": "keep-me", "other": "drop-me"}).as_object().unwrap().clone();

        prune_body_to_state_fields(&mut body, &state_fields, &fields);

        assert_eq!(Value::Object(body), json!({"Name": "x", "ghost_field": "keep-me"}));
    }

    /// Dotted api_field paths (nested body objects) only expose their first
    /// segment as a real top-level body key (see materializer.rs::insert_nested).
    #[test]
    fn prune_body_to_state_fields_uses_first_segment_of_dotted_api_field() {
        let fields = vec![make_field("icon", Some("additional_properties.icon")), make_field("color", Some("additional_properties.color"))];
        let state_fields = vec!["icon".to_string()];

        let mut body = json!({
            "additional_properties": {"icon": "star", "color": "blue"},
            "unrelated": "drop-me"
        })
        .as_object()
        .unwrap()
        .clone();

        prune_body_to_state_fields(&mut body, &state_fields, &fields);

        // Only the top-level key survives; nested pruning within
        // additional_properties is not attempted (matches prior behavior for
        // non-dotted keys - retain operates at the top level only).
        assert_eq!(Value::Object(body), json!({"additional_properties": {"icon": "star", "color": "blue"}}));
    }
}
