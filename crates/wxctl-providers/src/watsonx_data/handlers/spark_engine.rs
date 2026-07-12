use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{HttpClient, error_matches};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::engine_lifecycle;

pub struct SparkEngineHandler;

const BASE_PATH: &str = "/v3/spark_engines";

impl ResourceHandler for SparkEngineHandler {
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            engine_lifecycle::normalize_status(remote_data);
            Ok(())
        })
    }

    // `status` is our desired-state marker (pause/resume), not a create-payload input.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            engine_lifecycle::strip_status(resource);
            Ok(HookOutcome::Continue)
        })
    }

    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let engine_id = response.get("id").and_then(|v| v.as_str());
            let Some(engine_id) = engine_id else {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "spark_engine",
                    reason = "missing_id_in_create_response",
                    "skipping readiness poll"
                );
                return Ok(());
            };
            engine_lifecycle::wait_for_engine_ready(client, BASE_PATH, engine_id, operation_id).await
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { engine_lifecycle::run_update_hooks(current, desired, client, BASE_PATH, operation_id).await })
    }

    // Spark surfaces a display-name collision as "Cannot create the engine as
    // one already exists" (no "display name" phrase, unlike presto). Adopt on
    // re-apply rather than forcing a rename.
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move { engine_lifecycle::adopt_on_create_error(resource, error, client, BASE_PATH, "spark_engine", operation_id, is_engine_already_exists).await })
    }
}

fn is_engine_already_exists(err: &anyhow::Error) -> bool {
    error_matches(err, 400, &["already exists"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    // Spark surfaces a display-name collision as a 400 "...already exists" (no "display
    // name" phrase, unlike presto). A 500 carrying the phrase, or an unrelated 400, must
    // not match.
    #[test]
    fn is_engine_already_exists_cases() {
        let cases: &[(&str, bool)] = &[("WXCTL-H001 HTTP 400 POST: create engine failed with error: Cannot create the engine as one already exists", true), ("WXCTL-H002 HTTP 500 POST: already exists", false), ("WXCTL-H001 HTTP 400 POST: invalid node_type", false)];
        for (msg, expected) in cases {
            assert_eq!(is_engine_already_exists(&anyhow!("{msg}")), *expected, "{msg}");
        }
    }
}
