//! Handler for `storage_connection` — local-only credential container.
//!
//! No remote API, so all CRUD hooks return `HookOutcome::Handled` with the
//! resource's own spec. The kind exists in the pipeline solely to
//!   1. participate in the DAG as a reference target (`s3_bucket`,
//!      `adls_container`, etc. depend on it),
//!   2. centralise credential storage behind `sensitive: true`, and
//!   3. carry the `type:` discriminator that drives downstream behaviour.
//!
//! For `type: ibm_cos` under IAM apikey auth, `pre_create` performs a
//! one-shot auto-discovery of `instance_crn` when the field is absent —
//! relocated from the retired `common_core/handlers/cos_discovery.rs`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::cloud_object_storage::common::AUTH_TYPE_APIKEY;
use crate::common_core::handlers::cos_discovery;

pub struct StorageConnectionHandler;

impl ResourceHandler for StorageConnectionHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { normalize_and_echo(resource, client, operation_id).await })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { normalize_and_echo(desired, client, operation_id).await })
    }

    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { Ok(HookOutcome::Handled(resource.clone())) })
    }
}

async fn normalize_and_echo(resource: &mut Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    // ibm_cos under apikey auth: auto-discover instance_crn when missing.
    let is_ibm_cos = resource.get("type").and_then(|v| v.as_str()) == Some("ibm_cos");
    let crn_missing = resource.get("instance_crn").and_then(|v| v.as_str()).is_none_or(str::is_empty);
    if is_ibm_cos && crn_missing && client.auth_type() == AUTH_TYPE_APIKEY {
        let (crn, _guid) = cos_discovery::discover_cos_instance(client, operation_id).await.context("COS instance auto-discovery failed for storage_connection (type: ibm_cos)")?;
        if let Some(obj) = resource.as_object_mut() {
            obj.insert("instance_crn".to_string(), Value::String(crn));
        }
    }
    Ok(HookOutcome::Handled(resource.clone()))
}
