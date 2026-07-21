use super::super::ExecutionState;
use super::super::resolution::{enrich_with_linked_refs, extract_resource_id, resolve_dependencies};
use super::super::types::ExecutionResult;
use super::create::execute_create;
use crate::reconciliation::types::OperationType;
use anyhow::{Context, Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use wxctl_core::client::{BodyKindSelector, RequestMaterializer, error_has_status};

pub(super) async fn execute<'a>(planned_op: &'a crate::reconciliation::types::Operation, state: &'a ExecutionState) -> Result<ExecutionResult> {
    let op = planned_op;
    let local = op.local.as_ref().ok_or_else(|| anyhow!("No local resource for operation"))?;
    let descriptor = &local.descriptor;
    let endpoints = &descriptor.endpoints;
    let operation_id = &state.operation_id;
    let client = state.clients.get(&descriptor.service).ok_or_else(|| anyhow!("No client for service: {}", descriptor.service))?;

    let mut resolved_data = resolve_dependencies(&local.data, &state.runtime_ids, descriptor.schema)?;
    enrich_with_linked_refs(&mut resolved_data, &local.data, &state.runtime_ids, descriptor.schema, &state.registry);

    // Step 1: delete existing resource. Materialize so `also_query: true` fields
    // (e.g. wml_deployment.space_id) reach the bodyless DELETE as query params.
    if let Some(remote) = &op.remote
        && remote.exists
        && let Some(resource_id) = extract_resource_id(&remote.data, &descriptor.id_field)
    {
        let materializer = RequestMaterializer::new(Method::DELETE, &endpoints.delete);
        let mut spec = materializer.materialize(&resolved_data, descriptor.schema.resource.schema.fields, BodyKindSelector::None)?;
        spec.path_vars.insert(descriptor.id_field.clone(), resource_id);
        // Mirror delete.rs: a 404 means the remote vanished between plan and execute
        // (already gone) — tolerate it and proceed to the create step.
        let spec = spec.not_found_ok();
        match client.execute::<Value>(operation_id, spec).await {
            Ok(_) => {}
            Err(e) if error_has_status(&e, 404) => {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_kind = %local.key.kind, resource_name = %local.key.name, "recreate: resource already absent (404 on delete step is idempotent)");
            }
            Err(e) => return Err(e).context("Recreate failed: could not delete existing resource"),
        }
    }

    // Step 2: Create new resource (reuses Create logic with hooks)
    let merged = execute_create(&resolved_data, descriptor, client, &state.registry, operation_id).await?;

    Ok(ExecutionResult { key: op.key.clone(), operation: OperationType::Recreate, success: true, response: Some(merged), error: None, attempts: 1 })
}
