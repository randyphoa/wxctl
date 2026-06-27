use super::request::BodyKind;
use super::retry::{self, HttpError, status_method_is_retryable};
use super::token::TokenManager;
use crate::concurrency::CapacityManager;
use anyhow::{Context, Result, anyhow};
use futures::future::join_all;
use reqwest::Client;
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tracing::Instrument;
use uuid::Uuid;

#[derive(Clone)]
pub struct HttpClient {
    pub(crate) client: Client,
    pub(crate) base_url: String,
    pub(crate) service: String,
    pub(crate) token_manager: Arc<TokenManager>,
    pub(crate) auth_type: String,
    pub(crate) max_retries: u32,
    pub(crate) capacity: Arc<CapacityManager>,
    /// When present, sent as `AuthInstanceId` header on every request.
    /// Required by watsonx.data lakehouse APIs.
    pub(crate) instance_id: Option<String>,
    /// Leading URL-path segment prepended after `base_url` and before the
    /// resolved schema path (e.g. `/zen-data-api` for IBM Software Hub
    /// gateway routing). Empty string means no prefix.
    pub(crate) path_prefix: String,
    pub(crate) deployment: crate::types::Deployment,
}

impl HttpClient {
    pub fn new(base_url: String, service: String, auth_token: String, auth_type: String, capacity: Arc<CapacityManager>, request_timeout_secs: u64) -> Result<Self> {
        let token_manager = Arc::new(TokenManager::with_base_url(auth_token, auth_type.clone(), base_url.clone()));
        Self::with_token_manager(base_url, service, auth_type, capacity, token_manager, request_timeout_secs, None, String::new(), crate::types::Deployment::Saas)
    }

    /// Create an HttpClient with a shared TokenManager.
    ///
    /// Used by ClientFactory to reuse token caches across pipeline operations,
    /// avoiding redundant IAM token requests.
    #[allow(clippy::too_many_arguments)]
    pub fn with_token_manager(base_url: String, service: String, auth_type: String, capacity: Arc<CapacityManager>, token_manager: Arc<TokenManager>, request_timeout_secs: u64, instance_id: Option<String>, path_prefix: String, deployment: crate::types::Deployment) -> Result<Self> {
        let client = Client::builder().timeout(Duration::from_secs(request_timeout_secs)).build().context("Failed to create HTTP client")?;

        Ok(Self { client, token_manager, base_url, service, auth_type, max_retries: 3, capacity, instance_id, path_prefix, deployment })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn auth_type(&self) -> &str {
        &self.auth_type
    }

    /// Expose the underlying reqwest::Client for external API calls
    /// that bypass the base_url (e.g., IBM Cloud Resource Controller).
    pub fn raw_client(&self) -> &Client {
        &self.client
    }

    /// Expose the shared `CapacityManager` for adapters that wrap this
    /// client and still need to route through the workspace-wide
    /// rate-limiting semaphore (e.g. `CosClient`).
    pub fn capacity(&self) -> Arc<CapacityManager> {
        self.capacity.clone()
    }

    /// Service identifier this client was created for (e.g.
    /// `cloud_object_storage`, `watsonx_data`). Used by wrappers that
    /// need to acquire service-scoped permits from `CapacityManager`.
    pub fn service(&self) -> &str {
        &self.service
    }

    /// Leading URL-path segment prepended after `base_url`. Empty when not configured.
    pub fn path_prefix(&self) -> &str {
        &self.path_prefix
    }

    /// Active deployment for this client. Used by handlers to gate SaaS-only
    /// discovery hooks (cos_discovery, wml_discovery, etc.).
    pub fn deployment(&self) -> &crate::types::Deployment {
        &self.deployment
    }

    pub async fn get_token(&self) -> Result<String> {
        self.token_manager.get_token(&self.client).await
    }

    /// Get the raw auth credential (API key for apikey auth, token for bearer, etc.)
    /// Used by handlers that need to create task credentials or similar account-level resources.
    pub fn get_auth_credential(&self) -> &str {
        &self.token_manager.auth_token
    }

    /// Apply authentication to a request builder
    pub(crate) fn apply_auth(&self, req: reqwest::RequestBuilder, token: &str) -> Result<reqwest::RequestBuilder, HttpError> {
        match self.auth_type.as_str() {
            "basic" => {
                let parts: Vec<&str> = token.split(':').collect();
                if parts.len() == 2 { Ok(req.basic_auth(parts[0], Some(parts[1]))) } else { Err(HttpError::without_status("Invalid basic auth credentials format".to_string())) }
            }
            "zenapikey" => Ok(req.header("Authorization", format!("ZenApiKey {}", token))),
            _ => Ok(req.bearer_auth(token)),
        }
    }

    /// Execute HTTP request from RequestSpec with proper URL composition
    /// This is the primary entry point for HTTP requests
    /// Automatically acquires a concurrency permit before executing
    pub async fn execute<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, spec: super::request::RequestSpec) -> Result<T> {
        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;
        self.execute_internal(operation_id, spec).await
    }

    /// Internal execute without permit acquisition (used by public methods after acquiring permit)
    async fn execute_internal<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, spec: super::request::RequestSpec) -> Result<T> {
        use url::Url;

        // Interpolate path variables BEFORE URL parsing to avoid encoding issues
        // /v2/connections/{id} -> /v2/connections/abc-123
        let mut resolved_path = spec.path_template.clone();
        for (key, value) in &spec.path_vars {
            let placeholder = format!("{{{}}}", key);
            resolved_path = resolved_path.replace(&placeholder, value);
        }

        // Build base URL with resolved path (path_prefix prepended when present)
        let base_with_path = join_url(&self.base_url, &self.path_prefix, &resolved_path);
        let mut url = Url::parse(&base_with_path).context("Failed to parse base URL with path")?;

        // Append query parameters (preserves existing query params, proper encoding)
        if !spec.query.is_empty() {
            let mut query_pairs = url.query_pairs_mut();
            for (key, value) in &spec.query {
                query_pairs.append_pair(key, value);
            }
            // Drop query_pairs to release mutable borrow
            drop(query_pairs);
        }

        let server_address = url.host_str().map(|h| h.to_string()).unwrap_or_default();
        let final_url = url.to_string();
        let request_id = Uuid::new_v4().to_string();

        let redacted_req_body = {
            let raw = spec.body.as_json().cloned().unwrap_or(serde_json::Value::Null);
            let by_schema = if spec.sensitive_paths.is_empty() { raw } else { crate::logging::redact_by_schema(&raw, &spec.sensitive_paths) };
            crate::logging::redact_sensitive(&by_schema)
        };

        retry::with_retry(self.max_retries, async |attempt| {
            let span = tracing::trace_span!(
                target: "wxctl::substage::http",
                "http_request",
                operation_id = %operation_id,
                request_id = %request_id,
                method = %spec.method.as_str(),
                url = %final_url,
                content_type = ?spec.body.content_type(),
                attempt = attempt + 1,
                max_retries = self.max_retries,
                status = tracing::field::Empty,
                "http.request.method" = %spec.method.as_str(),
                "url.full" = %final_url,
                "server.address" = %server_address,
                "http.response.status_code" = tracing::field::Empty,
                "trace_id" = tracing::field::Empty,
            );

            async {
                let token = self.token_manager.get_token(&self.client).await.map_err(|e| HttpError::without_status(e.to_string()))?;

                let mut req = self.client.request(spec.method.clone(), &final_url);

                // Set Content-Type from BodyKind
                if let Some(ct) = spec.body.content_type() {
                    req = req.header("Content-Type", ct);
                }

                // Add custom headers from spec
                for (key, value) in &spec.headers {
                    req = req.header(key, value);
                }

                if let Some(instance_id) = &self.instance_id {
                    req = req.header("AuthInstanceId", instance_id);
                }

                // Add authentication
                req = self.apply_auth(req, &token)?;

                // Add body based on type
                match &spec.body {
                    BodyKind::Json(json_value) | BodyKind::JsonPatch(json_value) => {
                        req = req.json(json_value);
                    }
                    BodyKind::OctetStream(data) => {
                        req = req.body(data.clone());
                    }
                    BodyKind::None | BodyKind::Multipart(_) => {}
                }

                let response = req.send().await.map_err(|e| HttpError::without_status(e.to_string()))?;
                let status = response.status();

                // Record status on current span
                tracing::Span::current().record("status", status.as_u16());
                tracing::Span::current().record("http.response.status_code", status.as_u16());

                if status.is_success() {
                    if let Some(t) = response.headers().get("x-global-transaction-id").or_else(|| response.headers().get("x-request-id")).and_then(|v| v.to_str().ok()) {
                        tracing::Span::current().record("trace_id", t);
                    }
                    // 204 carries no body; reading .text() on it yields "" anyway, so both fall through the shared empty-body path.
                    let text = if status.as_u16() == 204 { String::new() } else { response.text().await.map_err(|e| HttpError::without_status(format!("Failed to read response body: {}", e)))? };
                    if text.is_empty() {
                        crate::log_http_request!(operation_id, &request_id, spec.method.as_str(), &final_url, status.as_u16(), &redacted_req_body, &serde_json::Value::Null);

                        return serde_json::from_value(Value::Null).map_err(|e| HttpError::without_status(format!("Failed to create empty response: {}", e)));
                    }

                    // Parse raw text as JSON Value for logging before deserializing to T
                    let response_value = serde_json::from_str::<Value>(&text);
                    if let Ok(ref rv) = response_value {
                        let redacted_resp = crate::logging::redact_sensitive(rv);
                        crate::log_http_request!(operation_id, &request_id, spec.method.as_str(), &final_url, status.as_u16(), &redacted_req_body, &redacted_resp);
                    }

                    let response_json: T = serde_json::from_str(&text).map_err(|e| HttpError::without_status(format!("Failed to parse response: {}", e)))?;
                    return Ok(response_json);
                }

                // Capture trace/correlation headers BEFORE consuming the body with .text().
                // IBM services set at least one of these on every response.
                let header_trace = response.headers().get("x-global-transaction-id").or_else(|| response.headers().get("x-request-id")).and_then(|v| v.to_str().ok()).map(|s| s.to_string());

                let error_text = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());

                let error_body: serde_json::Value = serde_json::from_str(&error_text).unwrap_or_else(|_| serde_json::Value::String(error_text.clone()));
                let redacted_error_body = crate::logging::redact_sensitive(&error_body);
                crate::log_http_request!(operation_id, &request_id, spec.method.as_str(), &final_url, status.as_u16(), &redacted_req_body, &redacted_error_body);

                let http_error_code = crate::logging::classify_http_error(status.as_u16());
                let api_message = crate::logging::extract_api_error_message(&error_body);
                let trace_id = crate::logging::extract_trace_id(&error_body).or(header_trace);
                if let Some(ref t) = trace_id {
                    tracing::Span::current().record("trace_id", t.as_str());
                }

                // Promote to ERROR only when this failure is final — i.e. no further
                // retry will occur.  Two conditions both mean "final":
                //   1. Exhausted retries (attempt reached max_retries - 1).
                //   2. Error is non-retryable for this (status, method) — with_retry
                //      returns immediately without sleeping, so attempt is still 0.
                // The retryability check reuses the exact same predicate as with_retry
                // to prevent the conditions from drifting.
                // Additionally, suppress the ERROR event when the status is in
                // `spec.expected_statuses` — callers that probe for absent resources
                // (404-means-not-found) opt in via `RequestSpec::not_found_ok()` so
                // that discovery misses don't pollute the failure counter.  The
                // trace-level `log_http_request!` exchange above is still recorded.
                let is_final = attempt >= self.max_retries - 1 || !status_method_is_retryable(Some(status), Some(&spec.method));
                let is_expected = spec.expected_statuses.contains(&status.as_u16());
                if is_final && !is_expected {
                    let fix = crate::logging::suggest_http_fix(status.as_u16(), &error_body);
                    let redacted_req = redacted_req_body.clone();
                    let redacted_resp = redacted_error_body.clone();

                    tracing::error!(
                        target: "wxctl::error",
                        operation_id = %operation_id,
                        stage = %spec.stage,
                        error_code = %http_error_code,
                        message = %format!("HTTP {} {} returned {}", spec.method.as_str(), &final_url, status),
                        fix = %fix,
                        cause = %api_message,
                        trace_id = ?trace_id,
                        expected = "HTTP 2xx",
                        actual = %format!("HTTP {}", status.as_u16()),
                        context = %serde_json::json!({
                            "request_body": redacted_req,
                            "response_body": redacted_resp
                        }),
                        "HTTP {} {} returned {}", spec.method.as_str(), &final_url, status
                    );
                }

                use std::fmt::Write as _;
                let mut message = format!("{} HTTP {} {}: {}", http_error_code, status.as_u16(), spec.method, api_message);
                if let Some(ref t) = trace_id {
                    let _ = write!(message, " [trace_id={t}]");
                }
                Err(HttpError::with_status(status, spec.method.clone(), message))
            }
            .instrument(span)
            .await
        })
        .await
    }

    /// Get single resource (compatibility wrapper for reconciler)
    /// Automatically acquires a concurrency permit before executing
    pub async fn get<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, endpoint: &'a str) -> Result<T> {
        use super::request::{BodyKind, RequestSpec};
        use reqwest::Method;

        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;

        let spec = RequestSpec::new(Method::GET, endpoint).body(BodyKind::None);

        self.execute_internal(operation_id, spec).await
    }

    /// List resources with query parameters (compatibility wrapper for reconciler)
    /// Automatically acquires a concurrency permit before executing
    pub async fn list_with_params<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, endpoint: &'a str, params: Option<HashMap<String, String>>) -> Result<Vec<T>> {
        use super::request::{BodyKind, RequestSpec};
        use reqwest::Method;

        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;

        let mut spec = RequestSpec::new(Method::GET, endpoint).body(BodyKind::None);

        if let Some(p) = params {
            for (key, value) in p {
                spec = spec.query_param(key, value);
            }
        }

        let response: Value = self.execute_internal(operation_id, spec).await?;

        let list = ListEnvelope::<T>::from_value(response)?;
        Ok(list.into_items())
    }

    /// Variant of `list_with_params` that treats 404 as an absent parent
    /// container rather than an error.  Discovery callers use this so that a
    /// missing space/project (404 on the list endpoint) does not emit a
    /// `wxctl::error` tracing event and does not count as a plan failure.
    pub async fn list_with_params_absent_ok<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, endpoint: &'a str, params: Option<HashMap<String, String>>) -> Result<Vec<T>> {
        use super::request::{BodyKind, RequestSpec};
        use reqwest::Method;

        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;

        let mut spec = RequestSpec::new(Method::GET, endpoint).body(BodyKind::None).not_found_ok().stage("reconciliation");

        if let Some(p) = params {
            for (key, value) in p {
                spec = spec.query_param(key, value);
            }
        }

        let response: Value = self.execute_internal(operation_id, spec).await?;

        let list = ListEnvelope::<T>::from_value(response)?;
        Ok(list.into_items())
    }

    /// Create resource (compatibility wrapper for executor recreate path)
    /// Automatically acquires a concurrency permit before executing
    pub async fn create<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, endpoint: &'a str, body: Value) -> Result<T> {
        use super::request::{BodyKind, RequestSpec};
        use reqwest::Method;

        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;

        let spec = RequestSpec::new(Method::POST, endpoint).body(BodyKind::Json(body));

        self.execute_internal(operation_id, spec).await
    }

    /// Delete resource (compatibility wrapper for executor recreate path)
    /// Automatically acquires a concurrency permit before executing
    pub async fn delete<'a>(&'a self, operation_id: &'a str, endpoint: &'a str) -> Result<()> {
        use super::request::{BodyKind, RequestSpec};
        use reqwest::Method;

        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;

        let spec = RequestSpec::new(Method::DELETE, endpoint).body(BodyKind::None);

        let _: Value = self.execute_internal(operation_id, spec).await?;
        Ok(())
    }

    // =========================================================================
    // Batch Methods - Centralized parallel execution
    // =========================================================================

    /// Execute multiple GET requests in parallel
    ///
    /// Rate limiting is handled automatically - each request acquires its own permit.
    /// This is the recommended way to fetch multiple resources concurrently.
    ///
    pub async fn get_many<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, endpoints: &'a [&'a str]) -> Vec<Result<T>> {
        let futures = endpoints.iter().map(|endpoint| {
            let client = self.clone();
            let op_id = operation_id.to_string();
            let ep = endpoint.to_string();
            async move { client.get(&op_id, &ep).await }
        });
        join_all(futures).await
    }

    /// Execute multiple requests in parallel using RequestSpecs
    ///
    /// Rate limiting is handled automatically - each request acquires its own permit.
    /// Use this for mixed HTTP methods or when you need full control over request configuration.
    ///
    pub async fn execute_batch<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, specs: Vec<super::request::RequestSpec>) -> Vec<Result<T>> {
        let futures = specs.into_iter().map(|spec| {
            let client = self.clone();
            let op_id = operation_id.to_string();
            async move { client.execute(&op_id, spec).await }
        });
        join_all(futures).await
    }

    /// Execute multiple requests in parallel, returning results paired with original indices
    ///
    /// Useful when you need to correlate results back to input data.
    /// Returns (index, Result<T>) pairs in input order (preserved by join_all).
    pub async fn get_many_indexed<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, endpoints: &'a [&'a str]) -> Vec<(usize, Result<T>)> {
        let futures = endpoints.iter().enumerate().map(|(idx, endpoint)| {
            let client = self.clone();
            let op_id = operation_id.to_string();
            let ep = endpoint.to_string();
            async move { (idx, client.get::<T>(&op_id, &ep).await) }
        });
        join_all(futures).await
    }
}

/// Wrapper keys recognized as list envelopes. Each entry MUST correspond to
/// a struct variant on `ListEnvelope<T>`; the `every_envelope_key_deserializes`
/// test iterates this slice to guard against drift.
const ENVELOPE_KEYS: &[&str] = &[
    "resources",
    "applications",
    "catalogs",
    "categories",
    "connections",
    "engines",
    "presto_engines",
    "spark_engines",
    "prestissimo_engines",
    "db2_engines",
    "other_engines",
    "milvus_services",
    "storage_registrations",
    "database_registrations",
    "rules",
    "schemas",
    "jobs",
    "buckets",
    "results",
    "items",
    "data",
    "integrations",
    "service_providers",
    "data_marts",
    "subscriptions",
    "monitor_instances",
    "data_sets",
    "integrated_systems",
    "monitor_definitions",
    "policies",
];

/// CRITICAL: List envelope detection for different API response formats
/// DO_NOT_REMOVE: All enum variants required for API compatibility
///
/// Every schema's `discovery.list_field` must have a matching variant here,
/// otherwise the untagged deserializer bails with `data did not match any
/// variant of untagged enum ListEnvelope`. Add a new variant *and* append
/// the key to `ENVELOPE_KEYS` when a new resource type surfaces a new key.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum ListEnvelope<T> {
    Resources { resources: Vec<T> },
    Applications { applications: Vec<T> },
    Catalogs { catalogs: Vec<T> },
    Categories { categories: Vec<T> },
    Connections { connections: Vec<T> },
    Engines { engines: Vec<T> },
    PrestoEngines { presto_engines: Vec<T> },
    SparkEngines { spark_engines: Vec<T> },
    Jobs { jobs: Vec<T> },
    Buckets { buckets: Vec<T> },
    Results { results: Vec<T> },
    Items { items: Vec<T> },
    Data { data: Vec<T> },
    // Lower-frequency v3 watsonx.data envelopes kept late so serde's
    // untagged linear probe hits the common shapes above first.
    StorageRegistrations { storage_registrations: Vec<T> },
    DatabaseRegistrations { database_registrations: Vec<T> },
    PrestissimoEngines { prestissimo_engines: Vec<T> },
    Db2Engines { db2_engines: Vec<T> },
    OtherEngines { other_engines: Vec<T> },
    MilvusServices { milvus_services: Vec<T> },
    Rules { rules: Vec<T> },
    Schemas { schemas: Vec<T> },
    Integrations { integrations: Vec<T> },
    // OpenScale (watsonx.governance) envelope keys.
    ServiceProviders { service_providers: Vec<T> },
    DataMarts { data_marts: Vec<T> },
    Subscriptions { subscriptions: Vec<T> },
    MonitorInstances { monitor_instances: Vec<T> },
    DataSets { data_sets: Vec<T> },
    IntegratedSystems { integrated_systems: Vec<T> },
    MonitorDefinitions { monitor_definitions: Vec<T> },
    Policies { policies: Vec<T> },
    Direct(Vec<T>),
}

impl<T: DeserializeOwned> ListEnvelope<T> {
    fn from_value(mut value: Value) -> Result<Self> {
        // Some services (e.g. watsonx.data spark_engines) return the envelope key
        // with `null` when empty instead of `[]`. Serde's Vec<T> deserializer
        // rejects null, so coerce any recognized envelope key from null to an
        // empty array before deserialization.
        if let Value::Object(map) = &mut value {
            for key in ENVELOPE_KEYS {
                if let Some(slot) = map.get_mut(*key)
                    && slot.is_null()
                {
                    *slot = Value::Array(Vec::new());
                }
            }
            // Paginated envelopes can omit the items key entirely when empty
            // (watsonx.data IngestionJobCollection marks it `required` with
            // `minItems: 1`). Servers don't reliably return every "required"
            // pagination field either, so accept any of the standard markers
            // as evidence this is a list response rather than a nested doc.
            const PAGINATION_MARKERS: &[&str] = &["total_count", "offset", "limit", "first", "last", "next"];
            let has_envelope_key = ENVELOPE_KEYS.iter().any(|k| map.contains_key(*k));
            let looks_paginated = PAGINATION_MARKERS.iter().any(|k| map.contains_key(*k));
            if !has_envelope_key && looks_paginated {
                map.insert("items".to_string(), Value::Array(Vec::new()));
            }
        }
        serde_json::from_value(value).context("Failed to parse list response")
    }

    fn into_items(self) -> Vec<T> {
        match self {
            ListEnvelope::Resources { resources } => resources,
            ListEnvelope::Applications { applications } => applications,
            ListEnvelope::Catalogs { catalogs } => catalogs,
            ListEnvelope::Categories { categories } => categories,
            ListEnvelope::Connections { connections } => connections,
            ListEnvelope::Engines { engines } => engines,
            ListEnvelope::PrestoEngines { presto_engines } => presto_engines,
            ListEnvelope::SparkEngines { spark_engines } => spark_engines,
            ListEnvelope::Jobs { jobs } => jobs,
            ListEnvelope::Buckets { buckets } => buckets,
            ListEnvelope::Results { results } => results,
            ListEnvelope::Items { items } => items,
            ListEnvelope::Data { data } => data,
            ListEnvelope::StorageRegistrations { storage_registrations } => storage_registrations,
            ListEnvelope::DatabaseRegistrations { database_registrations } => database_registrations,
            ListEnvelope::PrestissimoEngines { prestissimo_engines } => prestissimo_engines,
            ListEnvelope::Db2Engines { db2_engines } => db2_engines,
            ListEnvelope::OtherEngines { other_engines } => other_engines,
            ListEnvelope::MilvusServices { milvus_services } => milvus_services,
            ListEnvelope::Rules { rules } => rules,
            ListEnvelope::Schemas { schemas } => schemas,
            ListEnvelope::Integrations { integrations } => integrations,
            ListEnvelope::ServiceProviders { service_providers } => service_providers,
            ListEnvelope::DataMarts { data_marts } => data_marts,
            ListEnvelope::Subscriptions { subscriptions } => subscriptions,
            ListEnvelope::MonitorInstances { monitor_instances } => monitor_instances,
            ListEnvelope::DataSets { data_sets } => data_sets,
            ListEnvelope::IntegratedSystems { integrated_systems } => integrated_systems,
            ListEnvelope::MonitorDefinitions { monitor_definitions } => monitor_definitions,
            ListEnvelope::Policies { policies } => policies,
            ListEnvelope::Direct(items) => items,
        }
    }
}

/// Join `base_url`, `path_prefix`, and a resolved request `path` into a single URL.
/// - Strips any trailing `/` from `base_url` and `path_prefix`.
/// - Ensures `path_prefix` (when non-empty) starts with `/`.
/// - Ensures `path` starts with `/`.
/// - Empty `path_prefix` is a no-op.
pub fn join_url(base_url: &str, path_prefix: &str, path: &str) -> String {
    let base = base_url.trim_end_matches('/');
    let prefix = path_prefix.trim_end_matches('/');
    let path = if path.starts_with('/') { path.to_string() } else { format!("/{}", path) };
    if prefix.is_empty() {
        format!("{}{}", base, path)
    } else {
        let prefix = if prefix.starts_with('/') { prefix.to_string() } else { format!("/{}", prefix) };
        format!("{}{}{}", base, prefix, path)
    }
}

/// Returns true when the error surfaced from `HttpClient` carries the given
/// HTTP status. Lives here because it relies on the error-message format
/// produced by the error path above (the `"{code} HTTP {status} {method}: {msg}"`
/// template). Keeping parser and emitter co-located means a format change
/// updates both at once.
pub fn error_has_status(err: &anyhow::Error, status: u16) -> bool {
    err.to_string().contains(&format!("HTTP {status}"))
}

/// Returns true when the error has the given HTTP status AND its message
/// contains every phrase in `phrases`. Used by handler-level recovery hooks
/// that need to match specific "already exists" / "does not exist" shapes
/// from watsonx.data's human-readable error bodies.
pub fn error_matches(err: &anyhow::Error, status: u16, phrases: &[&str]) -> bool {
    if !error_has_status(err, status) {
        return false;
    }
    let msg = err.to_string();
    phrases.iter().all(|p| msg.contains(p))
}

#[cfg(test)]
mod list_envelope_tests {
    use super::{ENVELOPE_KEYS, ListEnvelope};
    use serde_json::{Value, json};

    /// Drift guard: a deserializer round-trip through each key must succeed.
    /// If a variant is added without updating `ENVELOPE_KEYS` (or vice versa),
    /// the affected key stops working and this test fails on that key.
    #[test]
    fn every_envelope_key_deserializes() {
        for &key in ENVELOPE_KEYS {
            let payload = json!({ key: [] });
            let env: ListEnvelope<Value> = serde_json::from_value(payload).unwrap_or_else(|e| panic!("envelope key '{key}' failed to deserialize: {e}"));
            assert!(env.into_items().is_empty(), "key '{key}' did not dispatch to the expected variant");
        }
    }

    #[test]
    fn null_envelope_value_is_coerced_to_empty() {
        let v = json!({"storage_registrations": Value::Null});
        let env: ListEnvelope<Value> = ListEnvelope::from_value(v).expect("null envelope value should coerce to empty array");
        assert!(env.into_items().is_empty());
    }

    #[test]
    fn paginated_response_with_omitted_items_key_parses_as_empty() {
        // A paginated envelope that omits the items key entirely parses as empty.
        // watsonx.data IngestionJobCollection marks `jobs` required with minItems:1,
        // so the server omits the key when there are zero jobs. And servers don't
        // reliably honor their own "required" lists either — the ingestion_jobs
        // endpoint was observed returning only a subset of the pagination markers
        // when empty, so a single marker is enough evidence of a list response.
        for v in [json!({"offset": 0, "limit": 100, "total_count": 0}), json!({"limit": 100})] {
            let env: ListEnvelope<Value> = ListEnvelope::from_value(v.clone()).unwrap_or_else(|e| panic!("paginated envelope {v:?} should parse as empty: {e}"));
            assert!(env.into_items().is_empty(), "case={v:?}");
        }
    }

    #[test]
    fn paginated_response_with_items_key_still_parses_normally() {
        // Guard against the "inject items: []" fallback overriding a real payload.
        let v = json!({"offset": 0, "limit": 100, "total_count": 2, "jobs": [{"id": "a"}, {"id": "b"}]});
        let env: ListEnvelope<Value> = ListEnvelope::from_value(v).expect("non-empty paginated envelope should parse");
        let items = env.into_items();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0]["id"], "a");
    }
}

#[cfg(test)]
mod error_has_status_tests {
    use super::error_has_status;

    #[test]
    fn matches_formatted_status_and_rejects_others() {
        // Matches the status embedded in the formatted error message, rejects a
        // different status, and rejects an unrelated error that carries no status.
        let err = anyhow::anyhow!("WXCTL-H001 HTTP 404 DELETE: not found");
        assert!(error_has_status(&err, 404));
        assert!(!error_has_status(&err, 500), "different status must not match");

        let err = anyhow::anyhow!("something else entirely");
        assert!(!error_has_status(&err, 404), "unrelated error must not match");
    }
}

#[cfg(test)]
mod tests {
    use super::join_url;

    #[test]
    fn join_url_composes_and_normalizes() {
        // (base, prefix, path, expected)
        let cases = [
            ("https://h.example", "", "/v2/x", "https://h.example/v2/x"),                           // no prefix
            ("https://h.example", "/zen-data-api", "/v2/x", "https://h.example/zen-data-api/v2/x"), // with prefix
            ("https://h.example/", "zen-data-api/", "v2/x", "https://h.example/zen-data-api/v2/x"), // normalizes trailing/leading slashes
            ("https://h.example/", "", "/v2/x", "https://h.example/v2/x"),                          // empty prefix → no double slash
        ];
        for (base, prefix, path, expected) in cases {
            assert_eq!(join_url(base, prefix, path), expected, "base={base:?} prefix={prefix:?} path={path:?}");
        }
    }
}

#[cfg(test)]
mod zenapikey_http_tests {
    use super::*;
    use crate::concurrency::{CapacityManager, ConcurrencyConfig};

    fn make_client(auth_type: &str) -> HttpClient {
        let cfg = ConcurrencyConfig::default();
        let capacity = Arc::new(CapacityManager::new(&cfg));
        HttpClient::new("https://example.com".to_string(), "common_core".to_string(), "ignored-token".to_string(), auth_type.to_string(), capacity, 30).expect("client construct")
    }

    fn header_value(client: &HttpClient, token: &str) -> String {
        let req = client.client.get("https://example.com/x");
        let req = client.apply_auth(req, token).expect("apply_auth");
        let built = req.build().expect("build request");
        built.headers().get("Authorization").expect("Authorization header set").to_str().expect("ascii header").to_string()
    }

    #[test]
    fn zenapikey_emits_zenapikey_authorization_header() {
        // base64("alice:KEY-123") == "YWxpY2U6S0VZLTEyMw=="
        let client = make_client("zenapikey");
        assert_eq!(header_value(&client, "YWxpY2U6S0VZLTEyMw=="), "ZenApiKey YWxpY2U6S0VZLTEyMw==");
    }

    #[test]
    fn non_zen_non_basic_auth_types_fall_through_to_bearer() {
        // Every auth_type other than zenapikey/basic hits the default Bearer branch.
        for auth_type in ["cp4d", "apikey"] {
            let client = make_client(auth_type);
            assert_eq!(header_value(&client, "TOKEN-abc"), "Bearer TOKEN-abc", "auth_type={auth_type}");
        }
    }

    #[test]
    fn basic_still_uses_basic_auth() {
        // base64("alice:secret") == "YWxpY2U6c2VjcmV0"
        let client = make_client("basic");
        assert_eq!(header_value(&client, "alice:secret"), "Basic YWxpY2U6c2VjcmV0");
    }
}
