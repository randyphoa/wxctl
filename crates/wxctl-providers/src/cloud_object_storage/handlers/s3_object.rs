//! Handler for `s3_object` — writes and deletes S3 objects inside an
//! `s3_bucket`. Credentials flow from the bucket's linked
//! `storage_connection`, which the engine injects under
//! `resource["__ref__bucket"]["__ref__connection"]`. Drift detection
//! uses `x-amz-meta-wxctl-sha256` rather than ETag equality so
//! server-side encryption changes don't produce false positives.

use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::cloud_object_storage::common::{build_cos_client_from_connection, require_str};
use crate::cloud_object_storage::cos_client::{CosRequest, parse_s3_error, urlencode_path};
use crate::util::{REF_BUCKET, REF_CONNECTION, require_ref};

const MAX_OBJECT_BYTES: u64 = 100 * 1024 * 1024;

pub struct S3ObjectHandler;

impl ResourceHandler for S3ObjectHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { put_object(resource, client, operation_id).await })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { put_object(desired, client, operation_id).await })
    }

    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { delete_object(resource, client, operation_id).await })
    }
}

fn resolve_bucket_connection(resource: &Value) -> Result<&Value> {
    let bucket = require_ref(resource, REF_BUCKET)?;
    require_ref(bucket, REF_CONNECTION)
}

async fn put_object(resource: &mut Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    let bucket = require_str(resource, "bucket")?.to_string();
    let key = require_str(resource, "key")?.to_string();
    let region = resolve_region(resource).context("failed to resolve region for object")?;

    let (body, source_label) = load_body(resource).await?;
    let sha256_hex = hex::encode(Sha256::digest(&body));
    let content_type = content_type_for(resource, &key);
    let connection = resolve_bucket_connection(resource)?.clone();
    let cos = build_cos_client_from_connection(client, &connection)?;

    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), content_type.clone());
    headers.insert("x-amz-meta-wxctl-sha256".to_string(), sha256_hex);

    if let Some(meta) = resource.get("metadata").and_then(|v| v.as_object()) {
        for (k, v) in meta {
            if let Some(vs) = v.as_str() {
                headers.insert(format!("x-amz-meta-{}", k.to_lowercase()), vs.to_string());
            }
        }
    }

    let path = format!("/{bucket}/{}", urlencode_path(&key));
    let resp = cos.send(CosRequest { region: &region, method: Method::PUT, path: &path, extra_headers: headers, body, ..Default::default() }, operation_id).await?;

    if !resp.status.is_success() {
        let err = parse_s3_error(&resp.body_str());
        bail!("[{}] PUT object {bucket}/{key} failed: HTTP {} {} — {}", error_codes::H700, resp.status.as_u16(), err.code, err.message);
    }

    let etag = resp.header("etag").map(|s| s.trim_matches('"').to_string()).unwrap_or_default();

    tracing::info!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        bucket = %bucket,
        key = %key,
        source = %source_label,
        etag = %etag,
        "PUT s3_object"
    );

    Ok(HookOutcome::Handled(json!({
        "bucket": bucket,
        "key": key,
        "content_type": content_type,
        "etag": etag,
        "metadata": resource.get("metadata").cloned().unwrap_or(Value::Null),
    })))
}

async fn delete_object(resource: &Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    let bucket = require_str(resource, "bucket")?.to_string();
    let key = require_str(resource, "key")?.to_string();
    let region = resolve_region(resource).context("failed to resolve region for object")?;

    let connection = resolve_bucket_connection(resource)?.clone();
    let cos = build_cos_client_from_connection(client, &connection)?;
    let path = format!("/{bucket}/{}", urlencode_path(&key));
    let resp = cos.send(CosRequest { region: &region, method: Method::DELETE, path: &path, ..Default::default() }, operation_id).await?;

    match resp.status.as_u16() {
        200 | 204 | 404 => {
            if resp.status.as_u16() == 404 {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, bucket = %bucket, key = %key, error_code = %error_codes::H601, "object already gone — idempotent DELETE");
            }
            Ok(HookOutcome::Handled(json!({"bucket": bucket, "key": key, "deleted": true})))
        }
        status => {
            let err = parse_s3_error(&resp.body_str());
            bail!("[{}] DELETE object {bucket}/{key} failed: HTTP {status} {} — {}", error_codes::H700, err.code, err.message);
        }
    }
}

async fn load_body(resource: &Value) -> Result<(Vec<u8>, String)> {
    let content = resource.get("content").and_then(|v| v.as_str());
    let path = resource.get("path").and_then(|v| v.as_str());

    match (content, path) {
        (Some(content), None) => Ok((content.as_bytes().to_vec(), "inline".to_string())),
        (None, Some(p)) => {
            let pb = Path::new(p);
            let meta = tokio::fs::metadata(pb).await.with_context(|| format!("[{}] object source path '{p}' not readable", error_codes::H703))?;
            if meta.len() > MAX_OBJECT_BYTES {
                bail!("[{}] object source '{p}' is {} bytes (> 100 MB cap); use a multipart-capable tool for large uploads", error_codes::H703, meta.len());
            }
            let bytes = tokio::fs::read(pb).await.with_context(|| format!("[{}] failed to read object source '{p}'", error_codes::H703))?;
            Ok((bytes, format!("file:{p}")))
        }
        (Some(_), Some(_)) => bail!("[{}] s3_object: both 'content' and 'path' set — provide exactly one", error_codes::H703),
        (None, None) => bail!("[{}] s3_object: one of 'content' or 'path' must be set", error_codes::H703),
    }
}

fn content_type_for(resource: &Value, key: &str) -> String {
    if let Some(s) = resource.get("content_type").and_then(|v| v.as_str()) {
        return s.to_string();
    }
    mime_guess::from_path(key).first_or_octet_stream().essence_str().to_string()
}

fn resolve_region(resource: &Value) -> Result<String> {
    if let Some(r) = resource.get("region").and_then(|v| v.as_str())
        && !r.is_empty()
    {
        return Ok(r.to_string());
    }
    // Fall back to the linked bucket's region (from enrichment).
    if let Some(r) = resource.get(REF_BUCKET).and_then(|b| b.get("region")).and_then(|v| v.as_str())
        && !r.is_empty()
    {
        return Ok(r.to_string());
    }
    if let Ok(r) = std::env::var("COS_REGION")
        && !r.is_empty()
    {
        return Ok(r);
    }
    bail!("s3_object: cannot resolve region. Set a `region:` field on the resource or reference one via `${{s3_bucket.<name>.region}}`.");
}

#[cfg(test)]
mod tests {
    use super::*;

    // content_type_for: an explicit `content_type` field wins; otherwise it's guessed
    // from the key's extension, falling back to application/octet-stream.
    #[test]
    fn content_type_for_cases() {
        // Auto-detect from extension (no explicit field).
        let auto = json!({});
        assert_eq!(content_type_for(&auto, "report.json"), "application/json", "known extension");
        assert_eq!(content_type_for(&auto, "unknown.wxctltest"), "application/octet-stream", "unknown extension fallback");
        // Explicit field overrides the extension guess.
        let explicit = json!({"content_type": "text/plain; charset=utf-8"});
        assert_eq!(content_type_for(&explicit, "something.parquet"), "text/plain; charset=utf-8", "explicit wins");
    }
}
