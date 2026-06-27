use super::super::ExecutionState;
use super::super::resolution::{enrich_with_linked_refs, extract_resource_id, merge_request_response, resolve_dependencies};
use super::super::types::ExecutionResult;
use crate::reconciliation::types::OperationType;
use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use tracing::{Instrument, info_span};
use wxctl_core::client::{BodyKind, BodyKindSelector, RequestMaterializer};
use wxctl_core::logging::redact_sensitive;
use wxctl_core::traits::{HookOutcome, StateComparison};

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
        Some(wxctl_core::schema::HttpMethod::Put) => Method::PUT,
        Some(wxctl_core::schema::HttpMethod::Patch) => Method::PATCH,
        Some(wxctl_core::schema::HttpMethod::Post) => Method::POST,
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
                tracing::debug!(target: "wxctl::substage::hook", operation_id = %operation_id, hook = "pre_update", handler_kind = %descriptor.name, current = %serde_json::to_string(&redact_sensitive(current)).unwrap_or_default(), before = %serde_json::to_string(&redact_sensitive(&payload_before)).unwrap_or_default(), after = %serde_json::to_string(&redact_sensitive(&body)).unwrap_or_default(), "hook payload diff");
            }
        }
    }

    // Materialize from handler-modified body (includes injected fields like input_schema)
    let materializer = RequestMaterializer::new(method.clone(), endpoint_template);
    let mut final_spec = materializer.materialize(&body, &descriptor.schema.resource.schema.fields, body_selector)?;

    // For non-JSON-Patch PATCH/PUT updates, prune the body to schema state_fields so
    // that immutable / computed fields aren't re-sent on update. Without this filter
    // every materialized body field reaches the API, which rejects immutable fields
    // (e.g. database_registration's `connection` ref) on PATCH.
    if !use_json_patch
        && (method == Method::PATCH || method == Method::PUT)
        && let BodyKind::Json(Value::Object(ref mut map)) = final_spec.body
    {
        let allowed: Option<&Vec<String>> = descriptor.schema.resource.reconciliation.state_fields.as_ref();
        if let Some(allowed) = allowed {
            map.retain(|k, _| allowed.iter().any(|s| s == k));
        }
    }

    final_spec.path_vars.insert(descriptor.id_field.clone(), resource_id.to_string());

    let mut response: Value = client.execute(operation_id, final_spec).await?;

    if let Some(handler) = state.registry.get_handler(&descriptor.name) {
        // Inject the resource ID into body so post_update handler has access to it
        // This is necessary because the local resource data doesn't include the remote ID
        let mut body_with_id = body.clone();
        if let Value::Object(ref mut map) = body_with_id {
            map.insert(descriptor.id_field.clone(), Value::String(resource_id.to_string()));
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
