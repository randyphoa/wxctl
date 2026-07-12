//! `vault_policy` handler.
//!
//! `pre_create`/`pre_update`: read the resolved `policy_file` (an absolute path, already
//! resolved against the config dir by `resolve_file_paths`) and write its contents into
//! the `policy` body field. Path fields resolve the path but never read it — the handler
//! does, mirroring `watsonx_ai::handlers::code_upload`. No-op when `policy_file` is absent
//! (an inline `policy` is used as-is).
//!
//! `post_discover`: unwrap Vault's top-level `data` envelope via the shared helper.

use anyhow::{Result, anyhow};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct PolicyHandler;

/// Read `policy_file` (if present and non-empty) into `policy`. Absolute path already
/// resolved by `resolve_file_paths`. Errors if the file cannot be read.
fn load_policy_file(resource: &mut Value, operation_id: &str) -> Result<()> {
    let Some(path) = resource.get("policy_file").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string) else {
        return Ok(());
    };
    let contents = std::fs::read_to_string(&path).map_err(|e| anyhow!("[{operation_id}] Failed to read vault_policy policy_file '{path}': {e}"))?;
    if let Value::Object(map) = resource {
        map.insert("policy".to_string(), Value::String(contents));
    }
    Ok(())
}

impl ResourceHandler for PolicyHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            load_policy_file(resource, operation_id)?;
            Ok(HookOutcome::Continue)
        })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            load_policy_file(desired, operation_id)?;
            Ok(HookOutcome::Continue)
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            super::envelope::unwrap_data_envelope(remote_data);
            Ok(())
        })
    }
}
