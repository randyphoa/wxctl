use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::engine_lifecycle;

pub struct PrestoEngineHandler;

const BASE_PATH: &str = "/v3/presto_engines";

impl ResourceHandler for PrestoEngineHandler {
    // The API returns status as `RUNNING`/`PAUSED` (uppercase); our YAML uses
    // lowercase. Normalize so drift detection doesn't flag a spurious Update.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            engine_lifecycle::normalize_status(remote_data);
            Ok(())
        })
    }

    // Presto create rejects unknown fields; `status` is our desired-state marker, not
    // a create-payload input.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            engine_lifecycle::strip_status(resource);
            Ok(HookOutcome::Continue)
        })
    }

    // The list endpoint excludes engines still PROVISIONING, so a follow-up plan
    // would read "no items" and re-plan Create. Poll until RUNNING so subsequent
    // reconciliation sees the engine.
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let engine_id = response.get("id").and_then(|v| v.as_str());
            let Some(engine_id) = engine_id else {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "presto_engine",
                    reason = "missing_id_in_create_response",
                    "skipping readiness poll"
                );
                return Ok(());
            };
            let engine_id = engine_id.to_string();
            engine_lifecycle::wait_for_engine_ready(client, BASE_PATH, &engine_id, operation_id).await?;
            // The external endpoint is assigned during provisioning, so the create
            // POST response carries no host. Re-fetch once RUNNING and merge the
            // endpoint fields into the response so the Computed `external_host_name`
            // is populated and referenceable by downstream consumers (e.g. an
            // Orchestrate Presto connection: ${presto_engine.x.external_host_name}).
            let fresh: Value = client.get(operation_id, &format!("{BASE_PATH}/{engine_id}")).await?;
            engine_lifecycle::merge_computed_fields(response, &fresh, &["external_host_name", "host_name", "status"]);
            Ok(())
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { engine_lifecycle::run_update_hooks(current, desired, client, BASE_PATH, operation_id).await })
    }

    // `associated_catalogs` template refs keep reconciliation from listing
    // at plan time, so re-apply after a partial failure collides on
    // display_name. Adopt the existing engine instead of forcing a rename.
    //
    // Software CPD also exhibits an async-creation quirk: the create POST
    // returns 400 `wxdengines.watsonxdata.ibm.com "lakehouse-" not found`
    // while the underlying CR has actually been queued for creation. List
    // shows the engine PROVISIONING right after. Adopt-by-display_name
    // recovers from that case too.
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move { engine_lifecycle::adopt_on_create_error(resource, error, client, BASE_PATH, "presto_engine", operation_id, engine_lifecycle::is_recoverable_create_error).await })
    }
}
