//! Handler for `gcs_bucket` — register-only. Validates that the linked
//! `storage_connection` is a GCS connection (`type: google_cs`) and
//! returns passthrough location state that `storage_registration` reads
//! off the DAG edge. Never calls GCS; all CRUD runs in `pre_*` hooks
//! returning `HookOutcome::Handled`.

use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::cloud_object_storage::common::require_connection;

const GCS_TYPE: &str = "google_cs";

pub struct GcsBucketHandler;

impl ResourceHandler for GcsBucketHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { register_only_state(resource) })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { register_only_state(desired) })
    }

    fn pre_delete<'a>(&'a self, _resource: &'a Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { Ok(HookOutcome::Handled(json!({"deleted": true}))) })
    }
}

/// Validate the linked connection is `google_cs`, then echo the resource's
/// own location fields as passthrough state for drift comparison.
fn register_only_state(resource: &Value) -> Result<HookOutcome> {
    let connection = require_connection(resource)?;
    let conn_type = connection.get("type").and_then(|v| v.as_str()).unwrap_or_default();
    if conn_type != GCS_TYPE {
        bail!("[{}] gcs_bucket linked storage_connection has type '{conn_type}', expected '{GCS_TYPE}'", error_codes::H711);
    }
    Ok(HookOutcome::Handled(resource.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Accepts a `google_cs` linked connection; a wrong type is the H711 error; a missing
    // connection errors (via require_connection).
    #[test]
    fn register_only_state_cases() {
        assert!(register_only_state(&json!({"name": "b1", "location": "us", "__ref__connection": {"type": "google_cs"}})).is_ok(), "google_cs accepted");

        // Wrong type → H711 error code is load-bearing.
        let err = register_only_state(&json!({"name": "b1", "location": "us", "__ref__connection": {"type": "adls_gen2"}})).unwrap_err().to_string();
        assert!(err.contains("WXCTL-H711"), "wrong type: {err}");

        assert!(register_only_state(&json!({"name": "b1", "location": "us"})).is_err(), "missing connection errors");
    }
}
