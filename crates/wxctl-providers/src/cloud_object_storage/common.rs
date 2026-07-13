//! Shared helpers for `s3_bucket` / `s3_object` handlers.

use anyhow::{Result, anyhow, bail};
use serde_json::Value;
use wxctl_core::client::HttpClient;

use super::cos_client::{CosAuth, CosClient};
use crate::util::{REF_CONNECTION, require_ref};

pub const AUTH_TYPE_APIKEY: &str = "apikey";

pub fn require_str<'a>(resource: &'a Value, field: &str) -> Result<&'a str> {
    resource.get(field).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("required field '{field}' is missing"))
}

/// Return the resolved `storage_connection` spec enriched by the engine
/// under `__ref__connection`. Absence is a bug in the executor or schema,
/// not a user error.
pub fn require_connection(resource: &Value) -> Result<&Value> {
    require_ref(resource, REF_CONNECTION)
}

/// Build a `CosClient` from a resolved `storage_connection` spec. The
/// connection's `type:` drives auth selection:
///   - `ibm_cos` with an apikey-mode HttpClient: use IAM token, include
///     the connection's `instance_crn` on bucket CREATE / ListBuckets.
///   - Anything else (or when HMAC creds are present): SigV4 with
///     `access_key` / `secret_key` from the connection.
pub fn build_cos_client_from_connection(client: &HttpClient, connection: &Value) -> Result<CosClient> {
    let conn_type = connection.get("type").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("storage_connection is missing required field 'type'"))?;

    let access_key = connection.get("access_key").and_then(|v| v.as_str()).unwrap_or_default();
    let secret_key = connection.get("secret_key").and_then(|v| v.as_str()).unwrap_or_default();
    let instance_crn = connection.get("instance_crn").and_then(|v| v.as_str()).map(String::from);
    // Explicit endpoint for non-IBM-COS S3 backends (minio / ibm_ceph / on-prem
    // COS). The storage_connection schema documents it as required for those
    // types; the COS client falls back to the region-derived IBM COS host when
    // it's absent, so ibm_cos / aws_s3 connections are unaffected.
    let endpoint = connection.get("endpoint").and_then(|v| v.as_str()).map(String::from);

    if !access_key.is_empty() && !secret_key.is_empty() {
        let auth = CosAuth::Hmac { access_key: access_key.to_string(), secret_key: secret_key.to_string() };
        return Ok(CosClient::new(client.clone(), client.capacity(), auth, instance_crn, endpoint));
    }

    if conn_type == "ibm_cos" && client.auth_type() == AUTH_TYPE_APIKEY {
        return Ok(CosClient::new(client.clone(), client.capacity(), CosAuth::Apikey, instance_crn, endpoint));
    }

    bail!("storage_connection (type={conn_type}) missing access_key/secret_key and cannot fall back to profile apikey auth");
}
