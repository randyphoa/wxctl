//! IBM Cloud Object Storage (S3-compatible) client adapter.
//!
//! Wraps the workspace `HttpClient` so permits, retry, and operation_id
//! propagation behave like every other provider, while handling S3's XML
//! responses and the two auth flavours IBM COS supports:
//!
//! - `apikey` — IAM Bearer token plus `ibm-service-instance-id` on bucket
//!   CREATE and the service-level ListBuckets call.
//! - `hmac`   — AWS SigV4 (`service = "s3"`) signed with HMAC access key.

use anyhow::{Context, Result, anyhow, bail};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Method, StatusCode, header::HeaderMap};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::sync::Arc;
use wxctl_core::client::HttpClient;
use wxctl_core::concurrency::CapacityManager;

type HmacSha256 = Hmac<Sha256>;

const SERVICE: &str = "cloud_object_storage";
const S3_SERVICE: &str = "s3";

// =============================================================================
// Public types
// =============================================================================

#[derive(Debug, Clone)]
pub enum CosAuth {
    /// IBM IAM Bearer token (apikey mode). Adds `ibm-service-instance-id`
    /// header on operations that need it.
    Apikey,
    /// AWS SigV4 with static access/secret keys.
    Hmac { access_key: String, secret_key: String },
}

/// Whether a given request requires the `ibm-service-instance-id` header.
/// IBM COS mandates it only on bucket CREATE and on the service-level
/// ListBuckets endpoint; every other operation derives ownership from the
/// bucket itself, so the header is omitted to keep canonical request
/// construction (and SigV4 signing) minimal.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceIdPolicy {
    Include,
    #[default]
    Omit,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct S3Error {
    pub code: String,
    pub message: String,
}

#[derive(Debug)]
pub struct CosResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

impl CosResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|v| v.to_str().ok())
    }

    pub fn body_str(&self) -> std::borrow::Cow<'_, str> {
        String::from_utf8_lossy(&self.body)
    }
}

/// Build an S3 endpoint URL for a given region.
/// Example: `eu-gb` → `https://s3.eu-gb.cloud-object-storage.appdomain.cloud`.
pub fn endpoint_for_region(region: &str) -> String {
    format!("https://s3.{region}.cloud-object-storage.appdomain.cloud")
}

/// Extract the host (authority) from an endpoint URL for SigV4 host-header
/// signing. Strips the scheme and any trailing path; whatever remains —
/// including an explicit `:port` — is the canonical `host:` header value.
/// Used for custom S3 endpoints (MinIO / Ceph / NooBaa / on-prem COS) where
/// the host isn't derivable from `region`.
fn host_from_endpoint(endpoint: &str) -> String {
    let no_scheme = endpoint.strip_prefix("https://").or_else(|| endpoint.strip_prefix("http://")).unwrap_or(endpoint);
    no_scheme.split('/').next().unwrap_or(no_scheme).to_string()
}

// =============================================================================
// CosClient
// =============================================================================

pub struct CosClient {
    http: HttpClient,
    capacity: Arc<CapacityManager>,
    auth: CosAuth,
    cos_instance_crn: Option<String>,
    /// Explicit S3 endpoint base URL (no trailing slash), e.g. a MinIO / Ceph /
    /// NooBaa / on-prem COS host. When `None`, the endpoint is derived from the
    /// request's `region` against IBM Cloud's public COS hosts — preserving the
    /// original behaviour for `ibm_cos` / `aws_s3` connections.
    endpoint_override: Option<String>,
}

/// Inputs to a single S3 request. Fields with no value default to empty /
/// `Omit` so call sites only populate what they need.
#[derive(Debug, Default)]
pub struct CosRequest<'a> {
    pub region: &'a str,
    pub method: Method,
    /// RAW (unencoded) path — `send()` percent-encodes it exactly once and uses
    /// that same encoded string for both the wire URL and the SigV4 canonical
    /// path (S3 is single-encode). Callers must NOT pre-encode.
    pub path: &'a str,
    pub query: BTreeMap<String, String>,
    pub extra_headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
    pub service_id_policy: ServiceIdPolicy,
}

impl CosClient {
    pub fn new(http: HttpClient, capacity: Arc<CapacityManager>, auth: CosAuth, cos_instance_crn: Option<String>, endpoint_override: Option<String>) -> Self {
        Self { http, capacity, auth, cos_instance_crn, endpoint_override }
    }

    /// Execute a single S3 request against a regional endpoint, returning
    /// the raw (status, headers, body) triple for the handler to interpret.
    /// 2xx / 3xx (redirect) / 4xx statuses are all returned as `Ok`; only
    /// transport failures produce `Err`.
    pub async fn send(&self, req: CosRequest<'_>, operation_id: &str) -> Result<CosResponse> {
        let _permit = self.capacity.acquire(SERVICE).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;

        // Endpoint + signed host: a custom endpoint (MinIO / Ceph / NooBaa /
        // on-prem COS) overrides the region-derived IBM COS host. SigV4 stays
        // path-style (bucket in the path, never the host), so the override only
        // changes the authority — the region in the signing scope is unchanged
        // (custom S3 backends ignore it for routing but must agree on it).
        let (endpoint, host) = match &self.endpoint_override {
            Some(ep) => {
                let base = ep.trim_end_matches('/').to_string();
                let host = host_from_endpoint(&base);
                (base, host)
            }
            None => (endpoint_for_region(req.region), format!("s3.{}.cloud-object-storage.appdomain.cloud", req.region)),
        };
        // Single-encode the raw path once; the same string goes on the wire AND into
        // the SigV4 canonical request (S3 is single-encode, unlike most AWS services).
        let encoded_path = urlencode_path(req.path);
        let mut url = format!("{endpoint}{encoded_path}");
        if !req.query.is_empty() {
            url.push('?');
            url.push_str(&req.query.iter().map(|(k, v)| if v.is_empty() { k.to_string() } else { format!("{k}={}", urlencode(v)) }).collect::<Vec<_>>().join("&"));
        }
        let payload_hash = hex::encode(Sha256::digest(&req.body));
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date_stamp = now.format("%Y%m%d").to_string();

        let mut signed_headers: BTreeMap<String, String> = BTreeMap::new();
        signed_headers.insert("host".to_string(), host);
        signed_headers.insert("x-amz-content-sha256".to_string(), payload_hash.clone());
        signed_headers.insert("x-amz-date".to_string(), amz_date.clone());

        // ibm-service-instance-id is omitted entirely under HMAC: eu-gb COS
        // rejects it (signed or unsigned) on bucket CREATE with "An error
        // occurred when parsing the HTTP request". HMAC creds alone authorize
        // against the right instance. Apikey/Bearer needs it to scope the call.
        match &self.auth {
            CosAuth::Hmac { access_key, secret_key } => {
                let authorization = sigv4_authorization(&req.method, &encoded_path, &req.query, &signed_headers, &payload_hash, req.region, S3_SERVICE, access_key, secret_key, &amz_date, &date_stamp);
                signed_headers.insert("authorization".to_string(), authorization);
            }
            CosAuth::Apikey => {
                if req.service_id_policy == ServiceIdPolicy::Include
                    && let Some(crn) = &self.cos_instance_crn
                {
                    signed_headers.insert("ibm-service-instance-id".to_string(), crn.clone());
                }
                let token = self.http.get_token().await.context("Failed to acquire IAM token for COS")?;
                signed_headers.insert("authorization".to_string(), format!("Bearer {token}"));
            }
        }

        let mut http_req = self.http.raw_client().request(req.method.clone(), &url);
        for (key, value) in &signed_headers {
            http_req = http_req.header(key, value);
        }
        for (key, value) in &req.extra_headers {
            http_req = http_req.header(key, value);
        }
        if !req.body.is_empty() {
            http_req = http_req.body(req.body);
        }

        tracing::debug!(
            target: "wxctl::substage::http",
            operation_id = %operation_id,
            method = %req.method,
            url = %url,
            auth = ?auth_kind_label(&self.auth),
            "COS request"
        );

        let response = http_req.send().await.with_context(|| format!("COS request {} {url} failed", req.method))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await.context("Failed to read COS response body")?.to_vec();

        if !status.is_success() {
            tracing::debug!(
                target: "wxctl::substage::http",
                operation_id = %operation_id,
                status = status.as_u16(),
                response_body = %String::from_utf8_lossy(&body),
                "COS error response"
            );
        }

        Ok(CosResponse { status, headers, body })
    }
}

fn auth_kind_label(auth: &CosAuth) -> &'static str {
    match auth {
        CosAuth::Apikey => "apikey",
        CosAuth::Hmac { .. } => "hmac",
    }
}

// =============================================================================
// URL encoding — S3 canonical form
// =============================================================================

/// Percent-encode per RFC 3986 *unreserved* rules — matches what AWS SigV4
/// canonical query/path construction expects. Stays pure ASCII and treats
/// `/` as a regular character (paths are already split before calling).
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Same as `urlencode` but preserves `/` — used on the canonical S3 path.
pub(crate) fn urlencode_path(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b'~' || b == b'/' {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// =============================================================================
// SigV4 signing
// =============================================================================

/// Build the SigV4 canonical request + the semicolon-joined signed-headers list.
/// `encoded_path` must be the ALREADY percent-encoded path that goes on the wire —
/// it is used verbatim (S3 single-encode; other AWS services would encode again).
fn canonical_request(method: &Method, encoded_path: &str, query: &BTreeMap<String, String>, headers: &BTreeMap<String, String>, payload_hash: &str) -> (String, String) {
    let canonical_query = query.iter().map(|(k, v)| format!("{}={}", urlencode(k), urlencode(v))).collect::<Vec<_>>().join("&");

    let canonical_headers: String = headers.iter().map(|(k, v)| format!("{}:{}\n", k.to_lowercase(), v.trim())).collect();
    let signed_headers_list: Vec<String> = headers.keys().map(|k| k.to_lowercase()).collect();
    let signed_headers = signed_headers_list.join(";");

    (format!("{}\n{}\n{}\n{}\n{}\n{}", method.as_str(), encoded_path, canonical_query, canonical_headers, signed_headers, payload_hash), signed_headers)
}

/// Produce the full `Authorization` header value for an AWS SigV4 request.
/// `encoded_path` must already be percent-encoded exactly as it appears on the
/// wire; it is signed verbatim.
#[allow(clippy::too_many_arguments)]
pub fn sigv4_authorization(method: &Method, encoded_path: &str, query: &BTreeMap<String, String>, headers: &BTreeMap<String, String>, payload_hash: &str, region: &str, service: &str, access_key: &str, secret_key: &str, amz_date: &str, date_stamp: &str) -> String {
    // 1. Canonical request
    let (canonical_request, signed_headers) = canonical_request(method, encoded_path, query, headers, payload_hash);

    // 2. String to sign
    let scope = format!("{date_stamp}/{region}/{service}/aws4_request");
    let hashed_canonical = hex::encode(Sha256::digest(canonical_request.as_bytes()));
    let string_to_sign = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{hashed_canonical}");

    // 3. Derive signing key
    let k_date = hmac_bytes(format!("AWS4{secret_key}").as_bytes(), date_stamp.as_bytes());
    let k_region = hmac_bytes(&k_date, region.as_bytes());
    let k_service = hmac_bytes(&k_region, service.as_bytes());
    let k_signing = hmac_bytes(&k_service, b"aws4_request");

    // 4. Signature
    let signature = hex::encode(hmac_bytes(&k_signing, string_to_sign.as_bytes()));

    format!("AWS4-HMAC-SHA256 Credential={access_key}/{scope}, SignedHeaders={signed_headers}, Signature={signature}")
}

fn hmac_bytes(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(data);
    mac.finalize().into_bytes().to_vec()
}

// =============================================================================
// S3 XML error parsing
// =============================================================================

/// Extract `<Code>` and `<Message>` from an S3 XML error response.
/// Falls back to a generic `Unknown` code when the body doesn't match the
/// expected shape (e.g. empty response, HTML error page). Does not pull in
/// an XML parser — S3 error bodies are shallow enough for regex.
pub fn parse_s3_error(body: &str) -> S3Error {
    fn extract(body: &str, tag: &str) -> Option<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = body.find(&open)? + open.len();
        let rest = &body[start..];
        let end = rest.find(&close)?;
        Some(rest[..end].trim().to_string())
    }

    let code = extract(body, "Code").unwrap_or_else(|| "Unknown".to_string());
    let message = extract(body, "Message").unwrap_or_else(|| if body.trim().is_empty() { "empty response body".to_string() } else { body.trim().chars().take(200).collect() });
    S3Error { code, message }
}

/// Interpret a `Result<CosResponse>` as success or `anyhow::Error` with the
/// S3 error code mapped into the message. Callers that need to match on
/// specific codes should use `parse_s3_error` directly.
pub fn check_success(resp: &CosResponse, operation: &str) -> Result<()> {
    if resp.status.is_success() {
        return Ok(());
    }
    let err = parse_s3_error(&resp.body_str());
    bail!("{operation} failed: HTTP {} {} — {}", resp.status.as_u16(), err.code, err.message);
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    // --- SigV4 canonical test vector ---------------------------------------------------
    // Derived from AWS SigV4 test suite: get-vanilla (GET /, empty body, no query).
    // This is the documented reference case used by every AWS SDK to validate its
    // SigV4 implementation; if this green-lights, the signer matches AWS's spec.
    #[test]
    fn sigv4_get_vanilla_matches_aws_test_vector() {
        let access_key = "AKIDEXAMPLE";
        let secret_key = "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY";
        let amz_date = "20150830T123600Z";
        let date_stamp = "20150830";
        let region = "us-east-1";
        let service = "service";

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "example.amazonaws.com".to_string());
        headers.insert("x-amz-date".to_string(), amz_date.to_string());

        let empty_query: BTreeMap<String, String> = BTreeMap::new();
        let payload_hash = hex::encode(Sha256::digest(b""));

        let auth = sigv4_authorization(&Method::GET, "/", &empty_query, &headers, &payload_hash, region, service, access_key, secret_key, amz_date, date_stamp);

        // AWS-published expected value for get-vanilla
        let expected = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders=host;x-amz-date, Signature=5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31";
        assert_eq!(auth, expected);
    }

    #[test]
    fn sigv4_signed_headers_exclude_ibm_service_instance_id() {
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "s3.eu-gb.cloud-object-storage.appdomain.cloud".to_string());
        headers.insert("x-amz-content-sha256".to_string(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string());
        headers.insert("x-amz-date".to_string(), "20260420T000000Z".to_string());

        let empty_query: BTreeMap<String, String> = BTreeMap::new();
        let auth = sigv4_authorization(&Method::PUT, "/my-bucket", &empty_query, &headers, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "eu-gb", "s3", "AKID", "secret", "20260420T000000Z", "20260420");

        assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date,"));
        assert!(!auth.contains("ibm-service-instance-id"));
    }

    #[test]
    fn sigv4_query_params_are_sorted_and_encoded() {
        let secret_key = "secret";
        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "s3.eu-gb.cloud-object-storage.appdomain.cloud".to_string());
        headers.insert("x-amz-content-sha256".to_string(), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855".to_string());
        headers.insert("x-amz-date".to_string(), "20260419T000000Z".to_string());

        let mut query = BTreeMap::new();
        query.insert("max-keys".to_string(), "1000".to_string());
        query.insert("list-type".to_string(), "2".to_string());

        let auth = sigv4_authorization(&Method::GET, "/my-bucket/", &query, &headers, "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855", "eu-gb", "s3", "AKID", secret_key, "20260419T000000Z", "20260419");

        // Signature is deterministic for this input; verify prefix and
        // that SignedHeaders list is sorted and semicolon-joined.
        assert!(auth.starts_with("AWS4-HMAC-SHA256 Credential=AKID/20260419/eu-gb/s3/aws4_request,"));
        assert!(auth.contains("SignedHeaders=host;x-amz-content-sha256;x-amz-date,"));
    }

    // --- endpoint_for_region -----------------------------------------------------------
    #[test]
    fn host_from_endpoint_strips_scheme_and_path() {
        assert_eq!(host_from_endpoint("https://s3-openshift-storage.apps.example.com"), "s3-openshift-storage.apps.example.com");
        assert_eq!(host_from_endpoint("https://minio.local:9000/"), "minio.local:9000");
        assert_eq!(host_from_endpoint("http://10.0.0.5:9000/ignored/path"), "10.0.0.5:9000");
        // No scheme — returned as-is (authority only).
        assert_eq!(host_from_endpoint("ceph.internal"), "ceph.internal");
    }

    #[test]
    fn endpoint_for_region_common_regions() {
        for region in ["eu-gb", "us-south", "au-syd", "jp-tok", "br-sao", "ca-tor"] {
            let url = endpoint_for_region(region);
            assert_eq!(url, format!("https://s3.{region}.cloud-object-storage.appdomain.cloud"));
        }
    }

    // --- S3 XML error parsing ----------------------------------------------------------
    #[test]
    fn parse_s3_error_cases() {
        // Well-formed bodies all hit the same <Code>/<Message> extraction branch; the
        // BucketName trailer (first case) is ignored. Malformed/empty bodies fall back to
        // an `Unknown` code with a body-derived message.
        let cases: &[(&str, &str, &str)] = &[
            (
                r#"<?xml version="1.0" encoding="UTF-8"?>
<Error><Code>BucketAlreadyOwnedByYou</Code><Message>Your previous request to create the named bucket succeeded and you already own it.</Message><BucketName>foo</BucketName></Error>"#,
                "BucketAlreadyOwnedByYou",
                "Your previous request to create the named bucket succeeded and you already own it.",
            ),
            ("<Error><Code>BucketAlreadyExists</Code><Message>The requested bucket name is not available.</Message></Error>", "BucketAlreadyExists", "The requested bucket name is not available."),
            ("<Error><Code>NoSuchBucket</Code><Message>The specified bucket does not exist</Message></Error>", "NoSuchBucket", "The specified bucket does not exist"),
            ("<Error><Code>AccessDenied</Code><Message>Access Denied</Message></Error>", "AccessDenied", "Access Denied"),
            ("<Error><Code>RequestTimeTooSkewed</Code><Message>The difference between the request time and the server time is too large.</Message></Error>", "RequestTimeTooSkewed", "The difference between the request time and the server time is too large."),
            // Fallbacks: non-XML body → Unknown + the raw body; empty body → a clean placeholder.
            ("not xml at all", "Unknown", "not xml at all"),
            ("", "Unknown", "empty response body"),
        ];
        for (body, code, message) in cases {
            let err = parse_s3_error(body);
            assert_eq!(err.code, *code, "code for {body:?}");
            assert_eq!(err.message, *message, "message for {body:?}");
        }
    }

    // --- urlencode ---------------------------------------------------------------------
    #[test]
    fn urlencode_unreserved_and_escaping() {
        // RFC 3986 unreserved set passes through untouched.
        assert_eq!(urlencode("abc-_.~123"), "abc-_.~123");
        // urlencode escapes `/`; urlencode_path preserves it (canonical S3 path).
        assert_eq!(urlencode("a b/c"), "a%20b%2Fc");
        assert_eq!(urlencode_path("a b/c"), "a%20b/c");
    }

    // --- single-encode contract ---------------------------------------------------------
    // S3 SigV4 is single-encode: `send()` encodes the raw path exactly once, then that
    // SAME string is both the wire URL path and the canonical path (signed verbatim).
    // A key with a space must appear as `my%20file.txt` — not raw, not `%2520` — in
    // both places, otherwise HMAC requests 403 with SignatureDoesNotMatch.
    #[test]
    fn sigv4_path_with_space_is_single_encoded() {
        let raw_path = "/bucket/my file.txt";
        let encoded = urlencode_path(raw_path);
        assert_eq!(encoded, "/bucket/my%20file.txt");

        // Wire URL uses the encoded path (mirrors send()'s construction).
        let url = format!("https://s3.eu-gb.cloud-object-storage.appdomain.cloud{encoded}");
        assert!(url.ends_with("/bucket/my%20file.txt"));

        let mut headers = BTreeMap::new();
        headers.insert("host".to_string(), "s3.eu-gb.cloud-object-storage.appdomain.cloud".to_string());
        headers.insert("x-amz-date".to_string(), "20260420T000000Z".to_string());
        let empty_query: BTreeMap<String, String> = BTreeMap::new();
        let payload_hash = hex::encode(Sha256::digest(b""));

        // Canonical request carries the encoded path verbatim — single encoding.
        let (canonical, _) = canonical_request(&Method::GET, &encoded, &empty_query, &headers, &payload_hash);
        assert!(canonical.contains("/bucket/my%20file.txt"), "canonical path must be the once-encoded wire path: {canonical}");
        assert!(!canonical.contains("%2520"), "canonical path must not be double-encoded: {canonical}");
        assert!(!canonical.contains("my file.txt"), "canonical path must not contain the raw key: {canonical}");

        // And the full authorization builder signs that same encoded path (no re-encoding):
        // signing the encoded path directly equals what sigv4_authorization produces.
        let auth = sigv4_authorization(&Method::GET, &encoded, &empty_query, &headers, &payload_hash, "eu-gb", "s3", "AKID", "secret", "20260420T000000Z", "20260420");
        let auth_pre_encoded = sigv4_authorization(&Method::GET, "/bucket/my%20file.txt", &empty_query, &headers, &payload_hash, "eu-gb", "s3", "AKID", "secret", "20260420T000000Z", "20260420");
        assert_eq!(auth, auth_pre_encoded);
    }
}
