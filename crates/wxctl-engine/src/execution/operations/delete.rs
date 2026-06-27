use super::super::ExecutionState;
use super::super::resolution::{enrich_with_linked_refs, extract_resource_id, resolve_dependencies};
use super::super::types::ExecutionResult;
use crate::reconciliation::types::OperationType;
use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use tracing::{Instrument, info_span};
use wxctl_core::client::{BodyKindSelector, RequestMaterializer, error_has_status};
use wxctl_core::traits::HookOutcome;

pub(super) async fn execute<'a>(planned_op: &'a crate::reconciliation::types::Operation, state: &'a ExecutionState) -> Result<ExecutionResult> {
    let op = planned_op;
    let local = op.local.as_ref().ok_or_else(|| anyhow!("No local resource for operation"))?;
    let descriptor = &local.descriptor;
    let endpoints = &descriptor.endpoints;
    let operation_id = &state.operation_id;
    let client = state.clients.get(&descriptor.service).ok_or_else(|| anyhow!("No client for service: {}", descriptor.service))?;

    let current = op.remote.as_ref().map(|r| &r.data).ok_or_else(|| anyhow!("No remote data for delete"))?;

    let resource_id = extract_resource_id(current, &descriptor.id_field).ok_or_else(|| anyhow!("Missing ID field for delete"))?;

    let endpoint_template = &endpoints.delete;

    // Destroy-mode reconciliation intentionally stores the ORIGINAL unresolved
    // resource as `local` so enrich_with_linked_refs can extract ref names from
    // `${...}` templates below. Resolve those refs now; tolerate failures (an
    // orphaned resource may reference an already-deleted parent) by falling back
    // to the raw local — delete is best-effort, not state-restoring.
    let mut local_enriched = resolve_dependencies(&local.data, &state.runtime_ids, &descriptor.schema).unwrap_or_else(|_| local.data.clone());
    enrich_with_linked_refs(&mut local_enriched, &local.data, &state.runtime_ids, &descriptor.schema, &state.registry);

    // Materialize from the resolved data so Path/Query fields carry literal
    // values — otherwise `${...}` templates leak into the delete URL.
    let materializer = RequestMaterializer::new(Method::DELETE, endpoint_template);
    let mut spec = materializer.materialize(&local_enriched, &descriptor.schema.resource.schema.fields, BodyKindSelector::None)?;

    spec.path_vars.insert(descriptor.id_field.clone(), resource_id.to_string());

    // Destroy is idempotent: a 404 means the resource is already gone (e.g. a create
    // that failed mid-apply, or an out-of-band delete). Tolerate it so this DELETE
    // doesn't fail the destroy-DAG node and skip — leaking — its dependents.
    let spec = spec.not_found_ok();

    if let Some(handler) = state.registry.get_handler(&descriptor.name) {
        let pre_delete_span = info_span!(target: "wxctl::substage::hook", "pre_delete", operation_id = %operation_id, hook = "pre_delete", handler_kind = %descriptor.name, resource_kind = %local.key.kind, resource_name = %local.key.name);
        let pre_delete_result = handler.pre_delete(&local_enriched, &descriptor.fields, client, endpoint_template, operation_id).instrument(pre_delete_span).await?;
        if let HookOutcome::Handled(_) = pre_delete_result {
            return Ok(ExecutionResult { key: op.key.clone(), operation: OperationType::Delete, success: true, response: None, error: None, attempts: 1 });
        }
    }

    match client.execute::<Value>(operation_id, spec).await {
        Ok(_) => {}
        Err(e) if error_has_status(&e, 404) => {
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_kind = %local.key.kind, resource_name = %local.key.name, "delete: resource already absent (404 during destroy is idempotent)");
        }
        Err(e) => return Err(e),
    }

    if let Some(handler) = state.registry.get_handler(&descriptor.name) {
        let post_delete_span = info_span!(target: "wxctl::substage::hook", "post_delete", operation_id = %operation_id, hook = "post_delete", handler_kind = %descriptor.name, resource_kind = %local.key.kind, resource_name = %local.key.name);
        handler.post_delete(&local_enriched, client, operation_id).instrument(post_delete_span).await?;
    }

    Ok(ExecutionResult { key: op.key.clone(), operation: OperationType::Delete, success: true, response: None, error: None, attempts: 1 })
}
