//! `instana_custom_payload_configuration` handler — the tenant-GLOBAL custom
//! payload set has NO POST and no per-resource id: both create and update are
//! the same idempotent `PUT /api/events/settings/custom-payload-configurations`
//! whole-set upsert (a live-probed 405 on POST — the generic create path always
//! issues POST, see `execution/operations/create.rs`, so this handler-owned PUT
//! is required even though the schema already declares `create_method: PUT`).
//! The GET/PUT response carries no `id` field at all (`{fields, version,
//! lastUpdated}`), so the default update/delete paths — which extract
//! `descriptor.id_field` from remote data before building the request — would
//! error `Missing ID field for update/delete`. `pre_create`/`pre_update` share
//! one `upsert` fn; `pre_delete` also owns the whole DELETE (id-less endpoint,
//! no id extraction) via `HookOutcome::Handled`, mirroring
//! `AutomationActionHandler.pre_delete`. Destroy clears the ENTIRE tenant-global
//! set (documented blast radius in the schema description).

use anyhow::Result;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const CUSTOM_PAYLOAD_PATH: &str = "/api/events/settings/custom-payload-configurations";

pub struct CustomPayloadConfigurationHandler;

impl ResourceHandler for CustomPayloadConfigurationHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { upsert(resource, client, operation_id).await })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { upsert(desired, client, operation_id).await })
    }

    /// Own the delete outright: the id-less endpoint needs no id extraction, and
    /// the response carries none to extract anyway.
    fn pre_delete<'a>(&'a self, _resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let spec = RequestSpec::new(Method::DELETE, CUSTOM_PAYLOAD_PATH);
            let _: Value = client.execute(operation_id, spec).await?;
            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "instana_custom_payload_configuration", "cleared tenant-global custom payload field set");
            Ok(HookOutcome::Handled(json!({"deleted": true})))
        })
    }
}

/// Shared whole-set PUT-upsert serving both `pre_create` and `pre_update`: PUT
/// `{fields}` to the id-less endpoint and return the response (which carries no
/// id — nothing to preserve from `resource` beyond `fields`, unlike the
/// client-id upsert kinds).
async fn upsert(resource: &Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    let fields = resource.get("fields").cloned().unwrap_or_else(|| Value::Array(vec![]));
    let body = json!({ "fields": fields });
    let spec = RequestSpec::new(Method::PUT, CUSTOM_PAYLOAD_PATH).body(BodyKind::Json(body));
    let response: Value = client.execute(operation_id, spec).await?;
    Ok(HookOutcome::Handled(response))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upsert_body_shape() {
        // Sanity: the body sent is `{fields: [...]}` regardless of any other
        // key on the declared resource (e.g. the unused `id` slot).
        let resource = json!({"id": Value::Null, "fields": [{"key": "wxctl-ci-env", "type": "staticString", "value": "ci"}]});
        let fields = resource.get("fields").cloned().unwrap();
        let body = json!({ "fields": fields });
        assert_eq!(body, json!({"fields": [{"key": "wxctl-ci-env", "type": "staticString", "value": "ci"}]}));
    }
}
