use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::engine_lifecycle;

pub struct MilvusServiceHandler;

const BASE_PATH: &str = "/v3/milvus_services";

impl ResourceHandler for MilvusServiceHandler {
    // The API reports milvus status as `running`/`pending`/`stopped`; our YAML uses
    // `running`/`paused`. Collapse `stopped` -> `paused` (and any casing) so drift
    // detection doesn't flag a spurious Update after a pause.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            engine_lifecycle::normalize_status(remote_data);
            Ok(())
        })
    }

    // The milvus create body (MilvusServicePrototype) has no `status` field; `status`
    // is wxctl's pause/resume desired-state marker, not a create input. Strip it.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            engine_lifecycle::strip_status(resource);
            Ok(HookOutcome::Continue)
        })
    }

    // Create returns 202 with the service `id` while provisioning is still `pending`.
    // Poll until it reaches `running` so a follow-up plan reads it as ready (not
    // absent) and re-plans no-change.
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let Some(service_id) = response.get("id").and_then(|v| v.as_str()) else {
                return Ok(());
            };
            engine_lifecycle::wait_for_engine_ready(client, BASE_PATH, service_id, operation_id).await
        })
    }

    // milvus has no `associated_catalogs`; the only desired-state transition is
    // pause/resume, fired against `/v3/milvus_services/{id}/{pause|resume}`.
    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { engine_lifecycle::handle_status_transition(current, desired, client, BASE_PATH, operation_id).await })
    }

    // `storage_name` is a template ref, so reconciliation can't list at plan time;
    // a re-apply after a partial failure collides on display_name. Adopt the existing
    // service instead of forcing a rename.
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(engine_lifecycle::adopt_on_create_error(resource, error, client, BASE_PATH, "milvus_service", operation_id, engine_lifecycle::is_recoverable_create_error))
    }
}
