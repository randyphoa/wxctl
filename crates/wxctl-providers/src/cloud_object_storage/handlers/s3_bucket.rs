//! Handler for `s3_bucket` — creates, tags, and deletes S3-compatible
//! buckets (IBM COS, AWS S3, MinIO, Ceph) via the S3 REST API. All CRUD
//! runs inside `pre_*` hooks returning `HookOutcome::Handled`, because S3
//! responses are XML and the engine's default execute path assumes JSON.
//!
//! Auth creds flow from the linked `storage_connection` (injected by the
//! engine under `__ref__connection`). The `LocationConstraint` value on
//! CREATE is computed from the connection's `type:` — IBM COS uses
//! `{region}-{storage_class}`, AWS-style backends use just `{region}`.

use anyhow::{Result, bail};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::cloud_object_storage::common::{build_cos_client_from_connection, require_connection, require_str};
use crate::cloud_object_storage::cos_client::{CosClient, CosRequest, ServiceIdPolicy, check_success, endpoint_for_region, parse_s3_error};

const DEFAULT_STORAGE_CLASS: &str = "smart";
const FORCE_DESTROY_CAP: usize = 10_000;
const DELETE_BATCH_SIZE: usize = 1000;
const LIST_PAGE_SIZE: &str = "1000";

pub struct S3BucketHandler;

impl ResourceHandler for S3BucketHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { create_or_adopt_bucket(resource, client, operation_id).await })
    }

    // No pre_update: the schema uses `discovery: skip`, so the engine always plans
    // Create — tag convergence on an existing bucket happens on the adopt path in
    // create_or_adopt_bucket instead.
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { delete_bucket(resource, client, operation_id).await })
    }
}

fn location_constraint(conn_type: &str, region: &str, storage_class: &str) -> String {
    match conn_type {
        "ibm_cos" => format!("{region}-{storage_class}"),
        _ => region.to_string(),
    }
}

async fn create_or_adopt_bucket(resource: &mut Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    let name = require_str(resource, "name")?.to_string();
    let region = require_str(resource, "region")?.to_string();
    let storage_class = resource.get("storage_class").and_then(|v| v.as_str()).unwrap_or(DEFAULT_STORAGE_CLASS).to_string();
    let force_destroy = resource.get("force_destroy").and_then(|v| v.as_bool()).unwrap_or(false);

    let connection = require_connection(resource)?.clone();
    let conn_type = connection.get("type").and_then(|v| v.as_str()).unwrap_or("ibm_cos").to_string();
    let cos = build_cos_client_from_connection(client, &connection)?;

    if head_bucket_exists(&cos, &region, &name, operation_id).await? {
        // Adopt path: the schema uses `discovery: skip`, so every apply plans Create
        // and lands here for an existing bucket. Converge tags with an idempotent
        // PUT so tag edits on an adopted bucket aren't silently dropped.
        if let Some(tags) = resource.get("tags").and_then(|v| v.as_array())
            && !tags.is_empty()
        {
            put_bucket_tagging(&cos, &region, &name, tags, operation_id).await?;
        }
        return Ok(HookOutcome::Handled(bucket_state(&name, &region, &storage_class, force_destroy, &connection)));
    }

    let constraint = location_constraint(&conn_type, &region, &storage_class);
    let body = format!("<CreateBucketConfiguration><LocationConstraint>{constraint}</LocationConstraint></CreateBucketConfiguration>").into_bytes();
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "text/xml".to_string());

    let resp = cos.send(CosRequest { region: &region, method: Method::PUT, path: &format!("/{name}"), extra_headers: headers, body, service_id_policy: ServiceIdPolicy::Include, ..Default::default() }, operation_id).await?;

    match resp.status.as_u16() {
        200 | 201 => {}
        409 => {
            let err = parse_s3_error(&resp.body_str());
            match err.code.as_str() {
                "BucketAlreadyOwnedByYou" => {
                    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, bucket = %name, error_code = %error_codes::H601, "bucket already owned — treating CREATE as idempotent success");
                }
                "BucketAlreadyExists" => bail!("[{}] bucket name '{name}' is already taken by another account — pick a different name", error_codes::H706),
                _ => bail!("[{}] CREATE bucket '{name}' failed: HTTP 409 {} — {}", error_codes::H700, err.code, err.message),
            }
        }
        status => {
            let err = parse_s3_error(&resp.body_str());
            bail!("[{}] CREATE bucket '{name}' failed: HTTP {status} {} — {}", error_codes::H700, err.code, err.message);
        }
    }

    if let Some(tags) = resource.get("tags").and_then(|v| v.as_array())
        && !tags.is_empty()
    {
        put_bucket_tagging(&cos, &region, &name, tags, operation_id).await?;
    }

    Ok(HookOutcome::Handled(bucket_state(&name, &region, &storage_class, force_destroy, &connection)))
}

async fn delete_bucket(resource: &Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    let name = require_str(resource, "name")?.to_string();
    let region = require_str(resource, "region")?.to_string();
    let force_destroy = resource.get("force_destroy").and_then(|v| v.as_bool()).unwrap_or(false);

    let connection = require_connection(resource)?.clone();
    let cos = build_cos_client_from_connection(client, &connection)?;

    if force_destroy {
        let count = empty_bucket(&cos, &region, &name, operation_id).await?;
        tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, bucket = %name, object_count = count, "force_destroy: emptied bucket prior to DELETE");
    }

    let resp = cos.send(CosRequest { region: &region, method: Method::DELETE, path: &format!("/{name}"), ..Default::default() }, operation_id).await?;

    match resp.status.as_u16() {
        200 | 204 => Ok(HookOutcome::Handled(json!({"name": name, "deleted": true}))),
        404 => {
            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, bucket = %name, error_code = %error_codes::H601, "bucket already gone — idempotent DELETE");
            Ok(HookOutcome::Handled(json!({"name": name, "deleted": true})))
        }
        409 => {
            let err = parse_s3_error(&resp.body_str());
            bail!("[{}] DELETE bucket '{name}' rejected: HTTP 409 {} — {} (set `force_destroy: true` to empty-then-delete)", error_codes::H701, err.code, err.message);
        }
        status => {
            let err = parse_s3_error(&resp.body_str());
            bail!("[{}] DELETE bucket '{name}' failed: HTTP {status} {} — {}", error_codes::H700, err.code, err.message);
        }
    }
}

async fn empty_bucket(cos: &CosClient, region: &str, name: &str, operation_id: &str) -> Result<usize> {
    let mut total = 0usize;
    let mut continuation: Option<String> = None;

    loop {
        let mut query = BTreeMap::new();
        query.insert("list-type".to_string(), "2".to_string());
        query.insert("max-keys".to_string(), LIST_PAGE_SIZE.to_string());
        if let Some(token) = &continuation {
            query.insert("continuation-token".to_string(), token.clone());
        }

        let resp = cos.send(CosRequest { region, method: Method::GET, path: &format!("/{name}"), query, ..Default::default() }, operation_id).await?;
        check_success(&resp, "ListObjectsV2")?;

        let body = resp.body_str();
        let keys = extract_keys_from_list(&body);
        if keys.is_empty() {
            break;
        }

        total += keys.len();
        if total > FORCE_DESTROY_CAP {
            bail!("[{}] force_destroy: bucket '{name}' contains more than {FORCE_DESTROY_CAP} objects — refuse to paginate further", error_codes::H702);
        }

        for chunk in keys.chunks(DELETE_BATCH_SIZE) {
            delete_object_batch(cos, region, name, chunk, operation_id).await?;
        }

        continuation = extract_next_continuation_token(&body);
        if continuation.is_none() {
            break;
        }
    }

    Ok(total)
}

async fn delete_object_batch(cos: &CosClient, region: &str, bucket: &str, keys: &[String], operation_id: &str) -> Result<()> {
    let mut body = String::from("<Delete><Quiet>true</Quiet>");
    for key in keys {
        body.push_str(&format!("<Object><Key>{}</Key></Object>", xml_escape(key)));
    }
    body.push_str("</Delete>");
    let body_bytes = body.into_bytes();

    let mut query = BTreeMap::new();
    query.insert("delete".to_string(), String::new());

    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/xml".to_string());
    headers.insert("content-md5".to_string(), md5_b64(&body_bytes));

    let resp = cos.send(CosRequest { region, method: Method::POST, path: &format!("/{bucket}"), query, extra_headers: headers, body: body_bytes, ..Default::default() }, operation_id).await?;
    check_success(&resp, "DeleteObjects")
}

async fn put_bucket_tagging(cos: &CosClient, region: &str, bucket: &str, tags: &[Value], operation_id: &str) -> Result<()> {
    let mut xml = String::from("<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet>");
    for tag in tags {
        if let Some(s) = tag.as_str() {
            let (key, value) = match s.split_once('=') {
                Some((k, v)) => (k, v),
                None => (s, ""),
            };
            xml.push_str(&format!("<Tag><Key>{}</Key><Value>{}</Value></Tag>", xml_escape(key), xml_escape(value)));
        }
    }
    xml.push_str("</TagSet></Tagging>");
    let body = xml.into_bytes();

    let mut query = BTreeMap::new();
    query.insert("tagging".to_string(), String::new());

    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "application/xml".to_string());
    headers.insert("content-md5".to_string(), md5_b64(&body));

    let resp = cos.send(CosRequest { region, method: Method::PUT, path: &format!("/{bucket}"), query, extra_headers: headers, body, ..Default::default() }, operation_id).await?;
    check_success(&resp, "PutBucketTagging")
}

async fn head_bucket_exists(cos: &CosClient, region: &str, name: &str, operation_id: &str) -> Result<bool> {
    let resp = cos.send(CosRequest { region, method: Method::HEAD, path: &format!("/{name}"), ..Default::default() }, operation_id).await?;

    match resp.status.as_u16() {
        200 => Ok(true),
        404 => Ok(false),
        403 => {
            // A HEAD carries no body, so a bare 403 can't distinguish a bucket
            // owned by another account (AccessDenied on a foreign bucket) from
            // rejected credentials (InvalidAccessKeyId / SignatureDoesNotMatch).
            // Probe with a service-level ListBuckets — which does return an error
            // body — to tell the two apart instead of always blaming ownership.
            match probe_credentials(cos, region, operation_id).await {
                CredProbe::Rejected { code, message } => bail!("[{}] COS credentials were rejected ({code}: {message}) — verify the storage_connection's access_key / secret_key (and its endpoint / region)", error_codes::H708),
                CredProbe::Valid => bail!("[{}] bucket '{name}' exists but is owned by a different account", error_codes::H707),
                CredProbe::Inconclusive => bail!("[{}] HEAD bucket '{name}' returned 403 — the bucket exists but is owned by another account, or these COS credentials lack access to it", error_codes::H707),
            }
        }
        301 | 302 | 307 | 308 => {
            let actual = resp.header("x-amz-bucket-region").unwrap_or("unknown");
            bail!("[{}] bucket '{name}' exists in region '{actual}', not '{region}' — update the `region:` field", error_codes::H705);
        }
        status => {
            let err = parse_s3_error(&resp.body_str());
            bail!("[{}] HEAD bucket '{name}' returned unexpected HTTP {status} {} — {}", error_codes::H700, err.code, err.message);
        }
    }
}

/// Outcome of a service-level ListBuckets probe used to disambiguate a 403 on
/// HEAD bucket (which carries no body to read the S3 error code from).
enum CredProbe {
    /// ListBuckets succeeded → the credentials are valid (so a 403 on the bucket
    /// means it's genuinely owned by another account).
    Valid,
    /// ListBuckets was rejected with an unambiguous auth error.
    Rejected { code: String, message: String },
    /// ListBuckets failed for some other / network reason — can't decide.
    Inconclusive,
}

/// Probe whether the connection's credentials are valid via a service-level
/// ListBuckets (`GET /`). Only `InvalidAccessKeyId` / `SignatureDoesNotMatch`
/// count as a definitive credential rejection; anything else (including
/// `AccessDenied`, which a valid-but-scoped key can also return) is reported as
/// inconclusive so a real foreign-owned bucket isn't mislabelled as bad creds.
async fn probe_credentials(cos: &CosClient, region: &str, operation_id: &str) -> CredProbe {
    match cos.send(CosRequest { region, method: Method::GET, path: "/", service_id_policy: ServiceIdPolicy::Include, ..Default::default() }, operation_id).await {
        Ok(resp) if resp.status.is_success() => CredProbe::Valid,
        Ok(resp) => {
            let err = parse_s3_error(&resp.body_str());
            if is_credential_rejection(&err.code) { CredProbe::Rejected { code: err.code, message: err.message } } else { CredProbe::Inconclusive }
        }
        Err(_) => CredProbe::Inconclusive,
    }
}

/// Whether an S3 error code returned by ListBuckets unambiguously means the
/// credentials themselves are bad (vs. a permission/ownership issue). Only these
/// two codes are definitive; `AccessDenied` is intentionally excluded because a
/// valid-but-scoped key can also return it on a foreign bucket.
fn is_credential_rejection(s3_error_code: &str) -> bool {
    matches!(s3_error_code, "InvalidAccessKeyId" | "SignatureDoesNotMatch")
}

fn bucket_state(name: &str, region: &str, storage_class: &str, force_destroy: bool, connection: &Value) -> Value {
    let conn_type = connection.get("type").and_then(|v| v.as_str()).unwrap_or("ibm_cos");
    let endpoint = connection.get("endpoint").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| endpoint_for_region(region));
    json!({
        "name": name,
        "region": region,
        "storage_class": storage_class,
        "force_destroy": force_destroy,
        "endpoint": endpoint,
        "bucket_location": location_constraint(conn_type, region, storage_class),
    })
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
}

fn md5_b64(bytes: &[u8]) -> String {
    use base64::Engine;
    use md5::{Digest, Md5};
    base64::engine::general_purpose::STANDARD.encode(Md5::digest(bytes))
}

fn extract_keys_from_list(body: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find("<Key>") {
        let after = &rest[start + 5..];
        if let Some(end) = after.find("</Key>") {
            keys.push(xml_unescape(&after[..end]));
            rest = &after[end + 6..];
        } else {
            break;
        }
    }
    keys
}

fn extract_next_continuation_token(body: &str) -> Option<String> {
    let truncated = body.find("<IsTruncated>").and_then(|i| {
        let rest = &body[i + 13..];
        rest.find("</IsTruncated>").map(|e| rest[..e].trim().to_string())
    });
    if truncated.as_deref() != Some("true") {
        return None;
    }
    let start = body.find("<NextContinuationToken>")? + 23;
    let rest = &body[start..];
    let end = rest.find("</NextContinuationToken>")?;
    Some(xml_unescape(&rest[..end]))
}

fn xml_unescape(s: &str) -> String {
    s.replace("&apos;", "'").replace("&quot;", "\"").replace("&gt;", ">").replace("&lt;", "<").replace("&amp;", "&")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_b64_matches_empty_string() {
        assert_eq!(md5_b64(b""), "1B2M2Y8AsgTpgAmY7PhCfg==");
    }

    #[test]
    fn extract_keys_pulls_keys_in_order() {
        let body = "<ListBucketResult><Contents><Key>a/foo.txt</Key></Contents><Contents><Key>b.bin</Key></Contents></ListBucketResult>";
        let keys = extract_keys_from_list(body);
        assert_eq!(keys, vec!["a/foo.txt".to_string(), "b.bin".to_string()]);
    }

    #[test]
    fn extract_continuation_only_when_truncated() {
        let body_truncated = "<ListBucketResult><IsTruncated>true</IsTruncated><NextContinuationToken>ABC%3D</NextContinuationToken></ListBucketResult>";
        assert_eq!(extract_next_continuation_token(body_truncated).as_deref(), Some("ABC%3D"));

        let body_not_truncated = "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>";
        assert_eq!(extract_next_continuation_token(body_not_truncated), None);
    }

    #[test]
    fn xml_escape_roundtrip() {
        let s = "<hello & \"world\"/>'";
        let escaped = xml_escape(s);
        assert_eq!(xml_unescape(&escaped), s);
    }

    #[test]
    fn location_constraint_branches_on_connection_type() {
        assert_eq!(location_constraint("ibm_cos", "eu-gb", "smart"), "eu-gb-smart");
        assert_eq!(location_constraint("aws_s3", "us-east-1", "STANDARD"), "us-east-1");
        assert_eq!(location_constraint("minio", "local", "anything"), "local");
    }

    #[test]
    fn credential_rejection_only_for_definitive_auth_codes() {
        // Bad-credential codes → a HEAD-bucket 403 is a credential problem (H708).
        assert!(is_credential_rejection("InvalidAccessKeyId"));
        assert!(is_credential_rejection("SignatureDoesNotMatch"));
        // Ambiguous / ownership codes must NOT be treated as bad creds — a
        // valid-but-scoped key returns AccessDenied on a foreign bucket, which
        // is the genuine H707 "owned by a different account" case.
        assert!(!is_credential_rejection("AccessDenied"));
        assert!(!is_credential_rejection("NoSuchBucket"));
        assert!(!is_credential_rejection(""));
    }
}
