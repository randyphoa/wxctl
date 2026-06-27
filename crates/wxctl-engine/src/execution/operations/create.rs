use super::super::ExecutionState;
use super::super::resolution::{enrich_with_linked_refs, merge_request_response, resolve_dependencies};
use super::super::types::ExecutionResult;
use crate::reconciliation::types::OperationType;
use anyhow::Result;
use reqwest::Method;
use serde_json::Value;
use tracing::{Instrument, info_span};
use wxctl_core::client::{BodyKindSelector, RequestMaterializer};
use wxctl_core::logging::redact_sensitive;
use wxctl_core::registry::ResourceDescriptor;
use wxctl_core::traits::HookOutcome;
use wxctl_core::{HttpClient, ResourceRegistry};

/// Shared create logic used by both Create and Recreate operations.
/// Runs pre_create hook, materializes the request, executes it, runs post_create hook,
/// and returns the merged response.
pub(in crate::execution) async fn execute_create<'a>(resolved_data: &'a Value, descriptor: &'a ResourceDescriptor, client: &'a HttpClient, registry: &'a ResourceRegistry, operation_id: &'a str) -> Result<Value> {
    let endpoint = &descriptor.endpoints.create;
    let materializer = RequestMaterializer::new(Method::POST, endpoint);

    let mut body = resolved_data.clone();
    if let Some(handler) = registry.get_handler(&descriptor.name) {
        let payload_before = body.clone();
        let pre_create_span = info_span!(target: "wxctl::substage::hook", "pre_create", operation_id = %operation_id, hook = "pre_create", handler_kind = %descriptor.name);
        let pre_create_result = {
            let fut = handler.pre_create(&mut body, &descriptor.fields, client, endpoint, operation_id);
            fut.instrument(pre_create_span).await
        };
        match pre_create_result {
            Ok(HookOutcome::Handled(response)) => {
                return Ok(merge_request_response(resolved_data, &response));
            }
            Ok(HookOutcome::Continue) => {
                tracing::debug!(target: "wxctl::substage::hook", operation_id = %operation_id, hook = "pre_create", handler_kind = %descriptor.name, before = %serde_json::to_string(&redact_sensitive(&payload_before)).unwrap_or_default(), after = %serde_json::to_string(&redact_sensitive(&body)).unwrap_or_default(), "hook payload diff");
            }
            // Handlers that own the POST inside pre_create never reach the default-POST
            // recover path below, so adopt-on-conflict must also fire here.
            Err(e) => {
                let recover_span = info_span!(target: "wxctl::substage::hook", "recover_from_create_error", operation_id = %operation_id, hook = "recover_from_create_error", handler_kind = %descriptor.name);
                let recover_result = handler.recover_from_create_error(&body, &e, client, operation_id).instrument(recover_span).await?;
                if let Some(existing) = recover_result {
                    return Ok(merge_request_response(resolved_data, &existing));
                }
                return Err(e);
            }
        }
    }

    let final_spec = materializer.materialize(&body, &descriptor.schema.resource.schema.fields, BodyKindSelector::Json)?;

    let mut response: Value = match client.execute(operation_id, final_spec).await {
        Ok(response) => response,
        Err(e) => {
            if let Some(handler) = registry.get_handler(&descriptor.name) {
                let recover_span = info_span!(target: "wxctl::substage::hook", "recover_from_create_error", operation_id = %operation_id, hook = "recover_from_create_error", handler_kind = %descriptor.name);
                let existing = handler.recover_from_create_error(&body, &e, client, operation_id).instrument(recover_span).await?;
                if let Some(existing) = existing {
                    return Ok(merge_request_response(resolved_data, &existing));
                }
            }
            return Err(e);
        }
    };

    if let Some(handler) = registry.get_handler(&descriptor.name) {
        let post_create_span = info_span!(target: "wxctl::substage::hook", "post_create", operation_id = %operation_id, hook = "post_create", handler_kind = %descriptor.name);
        handler.post_create(&body, &mut response, client, operation_id).instrument(post_create_span).await?;
    }

    Ok(merge_request_response(resolved_data, &response))
}

pub(super) async fn execute<'a>(planned_op: &'a crate::reconciliation::types::Operation, state: &'a ExecutionState) -> Result<ExecutionResult> {
    let op = planned_op;
    let local = op.local.as_ref().ok_or_else(|| anyhow::anyhow!("No local resource for operation"))?;
    let descriptor = &local.descriptor;
    let client = state.clients.get(&descriptor.service).ok_or_else(|| anyhow::anyhow!("No client for service: {}", descriptor.service))?;

    let mut resolved_data = resolve_dependencies(&local.data, &state.runtime_ids, &descriptor.schema)?;
    enrich_with_linked_refs(&mut resolved_data, &local.data, &state.runtime_ids, &descriptor.schema, &state.registry);
    let merged = execute_create(&resolved_data, descriptor, client, &state.registry, &state.operation_id).await?;

    Ok(ExecutionResult { key: op.key.clone(), operation: OperationType::Create, success: true, response: Some(merged), error: None, attempts: 1 })
}
