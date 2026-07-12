//! `concert_worker_group` handler — generates a join secret when the config omits one
//! (the API rejects a null `secret` on create) and GETs the group after a bodyless
//! create/update to capture server state.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const WORKER_GROUP_PATH: &str = "/v1/worker-group";

pub struct WorkerGroupHandler;

/// GET /v1/worker-group/{name} after a bodyless write and overwrite `response` with the
/// fetched object, so downstream id extraction / state comparison / refs see real server
/// state. A missing `name` or a failed GET is an error (don't report green on a blind write).
async fn read_back(resource: &Value, response: &mut Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_worker_group requires a 'name' to read back after write"))?;
    let path = format!("{WORKER_GROUP_PATH}/{name}");
    let spec = RequestSpec::new(Method::GET, path).body(BodyKind::None);
    let fetched: Value = client.execute(operation_id, spec).await?;
    *response = fetched;
    Ok(())
}

impl ResourceHandler for WorkerGroupHandler {
    /// The API rejects a null `secret` on create (live-observed 400 "secret: must not be null";
    /// the DTO lists only `name` as required). Generate a join secret when the config omits one —
    /// kept out of configs so cells stay secret-free; auto-redacted downstream.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            if resource.get("secret").and_then(|v| v.as_str()).is_none_or(str::is_empty)
                && let Some(obj) = resource.as_object_mut()
            {
                obj.insert("secret".to_string(), Value::String(format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple())));
            }
            Ok(HookOutcome::Continue)
        })
    }

    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { read_back(resource, response, client, operation_id).await })
    }

    fn post_update<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { read_back(resource, response, client, operation_id).await })
    }
}
