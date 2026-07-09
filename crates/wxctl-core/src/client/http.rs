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
    /// When present, sent as an instance-id header on every request. The header
    /// name is service-specific (`InstanceId` for `concert`, otherwise the
    /// watsonx/CP4D convention `AuthInstanceId`); see `instance_id_header_name`.
    /// Required by watsonx.data lakehouse APIs and the Concert core API.
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
        let builder = Client::builder().timeout(Duration::from_secs(request_timeout_secs));
        let client = with_optional_root_ca(builder)?.build().context("Failed to create HTTP client")?;

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
            "basic" => match token.split_once(':') {
                // First colon delimits — passwords may themselves contain ':'.
                Some((user, pass)) => Ok(req.basic_auth(user, Some(pass))),
                None => Err(HttpError::without_status("Invalid basic auth credentials format".to_string())),
            },
            "zenapikey" => Ok(req.header("Authorization", format!("ZenApiKey {}", token))),
            "c_api_key" => Ok(req.header("Authorization", format!("C_API_KEY {}", token))),
            "api_token" => Ok(req.header("Authorization", format!("apiToken {}", token))),
            // Planning Analytics TM1 REST: the paSession cookie authenticates every call.
            "pa_session" => Ok(req.header("Cookie", format!("paSession={}", token))),
            _ => Ok(req.bearer_auth(token)),
        }
    }

    /// Apply this client's configured auth scheme to a raw request builder.
    ///
    /// Public counterpart of `apply_auth` for callers that build requests via
    /// `raw_client()` (wxctl-providers, wxctl-sdk) — one scheme switch
    /// (`basic` / `zenapikey` / `c_api_key` / `api_token` / `pa_session` /
    /// Bearer default) instead of hand-rolled `bearer_auth` calls. Errors when
    /// `basic` credentials are not `username:password`.
    pub fn apply_auth_scheme(&self, req: reqwest::RequestBuilder, token: &str) -> Result<reqwest::RequestBuilder> {
        self.apply_auth(req, token).map_err(|e| anyhow!(e.message))
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
                    req = req.header(instance_id_header_name(&self.service), instance_id);
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
                    BodyKind::None => {}
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
                        // Schema-driven pass first (precise, catches fields like `webhookUrls`
                        // that the generic keyword list below doesn't recognize as sensitive),
                        // then the generic keyword pass — same composition as the request-body
                        // redaction above, via redact_for_log.
                        let redacted_resp = crate::logging::redact_for_log(rv, &spec.sensitive_paths);
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
                let redacted_error_body = crate::logging::redact_for_log(&error_body, &spec.sensitive_paths);
                crate::log_http_request!(operation_id, &request_id, spec.method.as_str(), &final_url, status.as_u16(), &redacted_req_body, &redacted_error_body);

                let http_error_code = crate::logging::classify_http_error(status.as_u16());
                let api_message = crate::logging::extract_api_error_message(&redacted_error_body);
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
                // Instana 3.319 quirk: an empty synthetics list arrives as 403 with a bare `[]` body
                // (a real denial carries an {"errors":[...]} body). The engine's list-discovery arms
                // convert that error to an empty list (schema_reconciler), so suppress the ERROR event
                // for this exact signature too — otherwise the output collector counts the handled
                // probe as a run failure. The HttpError itself still propagates to callers.
                let is_quirk_empty_list_403 = status.as_u16() == 403 && error_body.as_array().is_some_and(|a| a.is_empty());
                if is_final && !is_expected && !is_quirk_empty_list_403 {
                    let fix = crate::logging::suggest_http_fix(status.as_u16(), &redacted_error_body);
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
    ///
    /// `sensitive_paths` carries the schema's `sensitive: true` field paths
    /// (see `SchemaDefinition::sensitive_paths`) through to the HTTP client so
    /// the LIST response — which echoes credential-shaped fields (e.g. an
    /// alerting channel's `webhookUrls`) straight back from the API — gets the
    /// same schema-driven redaction as create/update request bodies. Pass an
    /// empty vec for callers with no schema in scope.
    pub async fn list_with_params_absent_ok<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, endpoint: &'a str, params: Option<HashMap<String, String>>, sensitive_paths: Vec<String>) -> Result<Vec<T>> {
        use super::request::{BodyKind, RequestSpec};
        use reqwest::Method;

        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;

        let mut spec = RequestSpec::new(Method::GET, endpoint).body(BodyKind::None).not_found_ok().stage("reconciliation").sensitive_paths(sensitive_paths);

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
    // IBM Concert core-API list envelopes.
    "environments",
    "source_repos",
    "credentials",
    "ingestion_jobs",
    "automation_rules",
    // IBM Concert compliance-API list envelope.
    "profiles",
    // Planning Analytics (TM1 Database 12) OData list envelope — {"value": [...]}.
    "value",
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
    // IBM Concert core-API list envelopes (unique keys — kept late so serde's
    // untagged linear probe hits the common shapes above first).
    Environments { environments: Vec<T> },
    SourceRepos { source_repos: Vec<T> },
    Credentials { credentials: Vec<T> },
    IngestionJobs { ingestion_jobs: Vec<T> },
    AutomationRules { automation_rules: Vec<T> },
    Profiles { profiles: Vec<T> },
    // Planning Analytics (TM1 Database 12) OData list envelope. Named `ValueEnvelope`
    // (not `Value`) to avoid shadowing `serde_json::Value`; kept late so serde's
    // untagged linear probe hits the common shapes above first.
    ValueEnvelope { value: Vec<T> },
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
            ListEnvelope::Environments { environments } => environments,
            ListEnvelope::SourceRepos { source_repos } => source_repos,
            ListEnvelope::Credentials { credentials } => credentials,
            ListEnvelope::IngestionJobs { ingestion_jobs } => ingestion_jobs,
            ListEnvelope::AutomationRules { automation_rules } => automation_rules,
            ListEnvelope::Profiles { profiles } => profiles,
            ListEnvelope::ValueEnvelope { value } => value,
            ListEnvelope::Direct(items) => items,
        }
    }
}

/// Optionally pin the client to an extra root CA from the `WXCTL_TLS_CA_FILE` env
/// var (a path to a PEM certificate, possibly a bundle of several). OFF BY
/// DEFAULT: unset/empty → the builder is returned unchanged (system roots only,
/// normal platform verification). When set, the client trusts ONLY the
/// certificate(s) in that file — system/native roots are NOT consulted — and
/// verification (chain, expiry, hostname/SAN) still runs in full via rustls'
/// standard webpki verifier against those pinned roots; nothing is disabled.
///
/// This deliberately does NOT use `ClientBuilder::add_root_certificate`, which
/// on reqwest 0.13's default rustls backend still routes through
/// rustls-platform-verifier: even an explicitly-added root gets re-checked by
/// `Verifier::new_with_extra_roots`, which on macOS delegates to Apple's
/// Security framework and applies Apple's own policy on top of chain
/// validation — including a hard 825-day certificate-validity cap. A
/// self-hosted backend's long-lived self-signed cert (e.g. Instana, 10-year
/// validity) blows past that cap and is rejected with "certificate is not
/// standards compliant: -67901" no matter how it's trusted. `tls_certs_only`
/// bypasses native/built-in root stores entirely (and the Apple policy layer
/// that comes with them), so this failure mode doesn't apply.
///
/// The file may hold multiple PEM blocks (a bundle) — e.g. append public CAs
/// alongside a private cluster CA if the same process must also reach
/// publicly-signed hosts, since pinning replaces rather than extends the trust
/// store. Generic — no service- or host-specific logic (carve-out invariant I3).
fn with_optional_root_ca(builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder> {
    match std::env::var("WXCTL_TLS_CA_FILE") {
        Ok(path) if !path.trim().is_empty() => add_root_ca_from_file(builder, path.trim()),
        _ => Ok(builder),
    }
}

/// Read PEM certificate(s) from `path` and pin `builder` to trust only those
/// roots. Uses `Certificate::from_pem_bundle` (rather than `from_pem`) so a
/// file with more than one `-----BEGIN CERTIFICATE-----` block (a CA chain or
/// bundle) is fully honored, and so malformed input is caught eagerly here:
/// under the rustls backend `Certificate::from_pem` alone defers PEM parsing to
/// `ClientBuilder::build()`, and content with no recognizable PEM markers at
/// all parses there as zero certificates *without* an error, silently
/// no-opting the extra-CA request. Checking `is_empty()` below closes that gap.
/// The parsed certs are then applied via `ClientBuilder::tls_certs_only`,
/// which trusts exactly this set (see `with_optional_root_ca` for why plain
/// `add_root_certificate` doesn't work here).
fn add_root_ca_from_file(builder: reqwest::ClientBuilder, path: &str) -> Result<reqwest::ClientBuilder> {
    let pem = std::fs::read(path).with_context(|| format!("WXCTL_TLS_CA_FILE: cannot read CA file '{path}'"))?;
    let certs = reqwest::Certificate::from_pem_bundle(&pem).with_context(|| format!("WXCTL_TLS_CA_FILE: '{path}' is not a valid PEM certificate"))?;
    if certs.is_empty() {
        return Err(anyhow!("WXCTL_TLS_CA_FILE: '{path}' is not a valid PEM certificate (no certificates found)"));
    }
    Ok(builder.tls_certs_only(certs))
}

/// HTTP header under which a service's `instance_id` is sent. IBM Concert's core
/// API expects the id in an `InstanceId` header; every other wxctl service uses
/// the watsonx/CP4D convention `AuthInstanceId`.
fn instance_id_header_name(service: &str) -> &'static str {
    match service {
        "concert" => "InstanceId",
        _ => "AuthInstanceId",
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
/// HTTP status. Lives here because it relies on the two error-message formats
/// the client emits:
/// - the main path (`"{code} HTTP {status} {method}: {api_message}"`, above), and
/// - the multipart uploader (`"HTTP {status} - {body}"`, `multipart.rs`).
///
/// The status token is anchored to the message *header* (the multipart prefix,
/// or the segment before the first `:` on the main path) so a status quoted
/// inside the upstream `{api_message}` body — e.g. "…returned HTTP 500" on a
/// 404 error — no longer spuriously matches. Keeping parser and emitter
/// co-located means a format change updates both at once.
pub fn error_has_status(err: &anyhow::Error, status: u16) -> bool {
    let msg = err.to_string();
    // Multipart uploader: "HTTP {status} - {body}".
    if msg.contains(&format!("HTTP {status} - ")) {
        return true;
    }
    // Main path: "{code} HTTP {status} {method}: {api_message}". Only the header
    // (before the first ':') carries the real status; the body may quote others.
    let header = msg.split_once(':').map_or(msg.as_str(), |(head, _)| head);
    header.contains(&format!("HTTP {status} "))
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
        // Main path format: "{code} HTTP {status} {method}: {msg}".
        let err = anyhow::anyhow!("WXCTL-H001 HTTP 404 DELETE: not found");
        assert!(error_has_status(&err, 404));
        assert!(!error_has_status(&err, 500), "different status must not match");

        let err = anyhow::anyhow!("something else entirely");
        assert!(!error_has_status(&err, 404), "unrelated error must not match");
    }

    #[test]
    fn rejects_status_quoted_in_the_message_body() {
        // A different status quoted in the api_message body must NOT match — the
        // real status (404) is anchored to the header before the first ':'.
        let err = anyhow::anyhow!("WXCTL-H001 HTTP 404 GET: upstream returned HTTP 500 internally");
        assert!(error_has_status(&err, 404), "real header status still matches");
        assert!(!error_has_status(&err, 500), "body-quoted status must not match");
    }

    #[test]
    fn matches_multipart_error_format() {
        // Multipart uploader format: "HTTP {status} - {body}".
        let err = anyhow::anyhow!("HTTP 413 - payload too large");
        assert!(error_has_status(&err, 413));
        assert!(!error_has_status(&err, 500), "different status must not match");

        let err = anyhow::anyhow!("HTTP 502 - Bad Gateway");
        assert!(error_has_status(&err, 502));
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

    #[test]
    fn instance_id_header_name_is_service_specific() {
        use super::instance_id_header_name;
        assert_eq!(instance_id_header_name("concert"), "InstanceId");
        assert_eq!(instance_id_header_name("watsonx_data"), "AuthInstanceId");
        assert_eq!(instance_id_header_name("common_core"), "AuthInstanceId");
    }
}

#[cfg(test)]
mod tls_ca_tests {
    use super::add_root_ca_from_file;

    // A throwaway self-signed PEM (CN=wxctl-test-ca.example.invalid). Generic test
    // fixture — carries no service/host identity (invariant I3).
    const TEST_CA_PEM: &str = "-----BEGIN CERTIFICATE-----\nMIIDMTCCAhmgAwIBAgIUNlLyXCSkJ+Z3JLDKXv2yiVANhfQwDQYJKoZIhvcNAQEL\nBQAwKDEmMCQGA1UEAwwdd3hjdGwtdGVzdC1jYS5leGFtcGxlLmludmFsaWQwHhcN\nMjYwNzAyMDkwNTQ5WhcNMzYwNjI5MDkwNTQ5WjAoMSYwJAYDVQQDDB13eGN0bC10\nZXN0LWNhLmV4YW1wbGUuaW52YWxpZDCCASIwDQYJKoZIhvcNAQEBBQADggEPADCC\nAQoCggEBALbhgSBMPqAz3GvX1HekPH13SU8VfWUcB5p76KdUEU5rXD90m+5bNTjg\nSrajvpbC365XKHonIC4W8/TJhPbJETsG0xAelg/w7eibcFyWMGG4aeBi6YpU6VRB\nfc6SW5INvd8fjCNu5/syr+ssTwPv9jSB5ilbf4gjbq1rm08nws8b2W/VW6QUNB10\nXVao40J/RW4u4xU5obZgsfS6y86IH9k7Gz4j2jxKFavTN1P0TO9UvfcUKuIWXpv3\ngzh2cBNudBiFP1Ud21TYbonToI6E5JHUT5b/kfB204nz10DTxVTO8ey0QbV06A9X\n6NUnjOFttnzog6THnk7mVlZwNJPzBSsCAwEAAaNTMFEwHQYDVR0OBBYEFOSBAhYN\nNHAcFzNnyoWdpUkMjvanMB8GA1UdIwQYMBaAFOSBAhYNNHAcFzNnyoWdpUkMjvan\nMA8GA1UdEwEB/wQFMAMBAf8wDQYJKoZIhvcNAQELBQADggEBACKdeLETqAXy7h8E\nNZFruEdzx8MJ91YOjaIyayWATGavUixGJSR5DyIqldxMcQ65jycx/EcIcsquf3sB\nbb55dlrrQmOeK9huA1ZDlFkjfH//COazyO2yKg9kpDeUFUio6OzSnuOc1JgySMBn\nMyQq059pr/ow4RchfYdwe24hAR+3tPh/uQnwIxWXetMm80faYg662H2LG6+nck8i\nHCMnpeq/0u9vYlNawWd1Iwc9ou13DqPsSso+C+5BjGdX1cMWphVFFxClFsvbV+u3\nafuaNeEeCdgmV8h4lafYMLxZOBbM81Iu3o93ixDsjLdzsaSjRwhGiLbTEYKQpbNP\nZAWbu40=\n-----END CERTIFICATE-----\n";

    fn builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder()
    }

    #[test]
    fn valid_pem_is_accepted_and_builds() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wxctl-ca-ok-{}.pem", std::process::id()));
        std::fs::write(&path, TEST_CA_PEM).expect("write pem");
        let b = add_root_ca_from_file(builder(), path.to_str().unwrap()).expect("valid PEM accepted");
        assert!(b.build().is_ok(), "client with extra root CA builds");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn missing_file_errors_with_read_context() {
        let err = add_root_ca_from_file(builder(), "/nonexistent/wxctl-ca.pem").unwrap_err();
        assert!(err.to_string().contains("cannot read CA file"), "unexpected error: {err}");
    }

    #[test]
    fn garbage_file_errors_as_invalid_pem() {
        let dir = std::env::temp_dir();
        let path = dir.join(format!("wxctl-ca-bad-{}.pem", std::process::id()));
        std::fs::write(&path, b"not a pem").expect("write garbage");
        let err = add_root_ca_from_file(builder(), path.to_str().unwrap()).unwrap_err();
        assert!(err.to_string().contains("not a valid PEM certificate"), "unexpected error: {err}");
        let _ = std::fs::remove_file(&path);
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
    fn apply_auth_emits_scheme_specific_authorization_header() {
        // (auth_type, token, expected Authorization header). Any scheme other than
        // zenapikey/c_api_key/api_token/basic falls through to the default Bearer branch.
        let cases = [
            // base64("alice:KEY-123") == "YWxpY2U6S0VZLTEyMw=="
            ("zenapikey", "YWxpY2U6S0VZLTEyMw==", "ZenApiKey YWxpY2U6S0VZLTEyMw=="),
            ("c_api_key", "sample-concert-token", "C_API_KEY sample-concert-token"),
            ("api_token", "sample-instana-token", "apiToken sample-instana-token"),
            ("cp4d", "TOKEN-abc", "Bearer TOKEN-abc"),
            ("apikey", "TOKEN-abc", "Bearer TOKEN-abc"),
            // base64("alice:secret") == "YWxpY2U6c2VjcmV0"
            ("basic", "alice:secret", "Basic YWxpY2U6c2VjcmV0"),
        ];
        for (auth_type, token, expected) in cases {
            let client = make_client(auth_type);
            assert_eq!(header_value(&client, token), expected, "auth_type={auth_type}");
        }
    }

    #[test]
    fn pa_session_emits_pasession_cookie_header() {
        let client = make_client("pa_session");
        let req = client.client.get("https://example.com/x");
        let req = client.apply_auth(req, "SESSIONVALUE-123").expect("apply_auth");
        let built = req.build().expect("build request");
        let cookie = built.headers().get("Cookie").expect("Cookie header set").to_str().expect("ascii header");
        assert_eq!(cookie, "paSession=SESSIONVALUE-123");
    }
}
