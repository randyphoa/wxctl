use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;

/// Single source of truth for HTTP request construction
/// Contains all information needed to execute an HTTP request
#[derive(Debug, Clone)]
pub struct RequestSpec {
    /// HTTP method (GET, POST, PATCH, PUT, DELETE)
    pub method: Method,

    /// Path template with {placeholders} for variables
    /// Example: "/v2/connections/{id}"
    pub path_template: String,

    /// Values to interpolate into path template
    /// Example: {"id": "abc-123"} transforms "/v2/connections/{id}" to "/v2/connections/abc-123"
    pub path_vars: HashMap<String, String>,

    /// Query parameters as ordered pairs
    /// Ordered to ensure deterministic URL generation
    pub query: Vec<(String, String)>,

    /// HTTP headers to include in request
    pub headers: HashMap<String, String>,

    /// Request body with explicit protocol type
    pub body: BodyKind,

    /// Request body field paths the schema marked `sensitive: true`.
    /// The HTTP client redacts these before any body reaches a span or event.
    pub sensitive_paths: Vec<String>,

    /// HTTP status codes the caller considers non-error outcomes.
    /// When a final failure has a status in this set the HTTP client suppresses
    /// the `wxctl::error` tracing event (the full-exchange trace-level log still
    /// lands).  The returned `HttpError` is unchanged — callers still match on
    /// status for absence / not-found semantics.  Use `not_found_ok()` for the
    /// common 404-means-absent case.
    pub expected_statuses: Vec<u16>,

    /// Pipeline stage this request runs in, stamped onto the `wxctl::error`
    /// tracing event so a reconcile-time failure renders as "reconciliation"
    /// rather than the historical hardcoded "execution".  Defaults to
    /// `"execution"` (apply/destroy path); discovery probes set
    /// `"reconciliation"` via `stage("reconciliation")`.
    pub stage: String,
}

/// Explicit body type that determines Content-Type header
#[derive(Debug, Clone)]
pub enum BodyKind {
    /// No request body
    None,

    /// Standard JSON body (Content-Type: application/json)
    Json(Value),

    /// JSON Patch operations array (Content-Type: application/json-patch+json)
    /// RFC 6902 format: [{"op": "replace", "path": "/field", "value": ...}]
    JsonPatch(Value),

    /// Raw binary data (Content-Type: application/octet-stream)
    /// Multipart uploads don't flow through RequestSpec — they use
    /// `HttpClient::request_multipart`.
    OctetStream(Vec<u8>),
}

impl BodyKind {
    /// Get Content-Type header value for this body kind
    /// Returns None for BodyKind::None
    pub fn content_type(&self) -> Option<&str> {
        match self {
            Self::None => None,
            Self::Json(_) => Some("application/json"),
            Self::JsonPatch(_) => Some("application/json-patch+json"),
            Self::OctetStream(_) => Some("application/octet-stream"),
        }
    }

    /// Get JSON value if body is Json or JsonPatch variant
    pub fn as_json(&self) -> Option<&Value> {
        match self {
            Self::Json(v) | Self::JsonPatch(v) => Some(v),
            _ => None,
        }
    }
}

impl RequestSpec {
    /// Create new RequestSpec with minimal required fields
    pub fn new(method: Method, path_template: impl Into<String>) -> Self {
        Self { method, path_template: path_template.into(), path_vars: HashMap::new(), query: Vec::new(), headers: HashMap::new(), body: BodyKind::None, sensitive_paths: Vec::new(), expected_statuses: Vec::new(), stage: "execution".to_string() }
    }

    /// Add path variable for template interpolation
    pub fn path_var(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.path_vars.insert(key.into(), value.into());
        self
    }

    /// Add query parameter
    pub fn query_param(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.query.push((key.into(), value.into()));
        self
    }

    /// Add HTTP header
    pub fn header(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.insert(key.into(), value.into());
        self
    }

    /// Set request body
    pub fn body(mut self, body: BodyKind) -> Self {
        self.body = body;
        self
    }

    /// Set the schema-derived sensitive field paths for body redaction.
    pub fn sensitive_paths(mut self, paths: Vec<String>) -> Self {
        self.sensitive_paths = paths;
        self
    }

    /// Declare that a 404 response is an expected outcome (absence probe).
    /// The HTTP client will not emit a `wxctl::error` event for 404 on this
    /// request; callers must still inspect the returned error to act on it.
    pub fn not_found_ok(mut self) -> Self {
        if !self.expected_statuses.contains(&404) {
            self.expected_statuses.push(404);
        }
        self
    }

    /// Declare one or more HTTP status codes as expected outcomes.
    /// Like `not_found_ok` but accepts any set of codes.
    pub fn expect_status(mut self, status: u16) -> Self {
        if !self.expected_statuses.contains(&status) {
            self.expected_statuses.push(status);
        }
        self
    }

    /// Set the pipeline stage stamped onto error events for this request.
    /// Discovery probes pass `"reconciliation"`; the default is `"execution"`.
    pub fn stage(mut self, stage: impl Into<String>) -> Self {
        self.stage = stage.into();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Method;

    #[test]
    fn new_has_empty_expected_statuses_and_execution_stage() {
        let spec = RequestSpec::new(Method::GET, "/v1/things");
        assert!(spec.expected_statuses.is_empty());
        assert_eq!(spec.stage, "execution");
    }

    #[test]
    fn expected_status_builders_add_codes_idempotently() {
        // not_found_ok adds 404; expect_status adds an arbitrary code; both are
        // idempotent (a repeat doesn't duplicate the entry); and a code that was
        // never declared (500) is correctly absent.
        let spec = RequestSpec::new(Method::GET, "/v1/things").not_found_ok();
        assert!(spec.expected_statuses.contains(&404));
        assert!(!spec.expected_statuses.contains(&500), "undeclared code must be absent");
        assert_eq!(spec.expected_statuses.len(), 1);

        let spec = RequestSpec::new(Method::GET, "/v1/things").not_found_ok().not_found_ok();
        assert_eq!(spec.expected_statuses.iter().filter(|&&s| s == 404).count(), 1, "not_found_ok idempotent");

        let spec = RequestSpec::new(Method::GET, "/v1/things").expect_status(409);
        assert!(spec.expected_statuses.contains(&409));
        assert_eq!(spec.expected_statuses.len(), 1);

        let spec = RequestSpec::new(Method::GET, "/v1/things").expect_status(409).expect_status(409);
        assert_eq!(spec.expected_statuses.iter().filter(|&&s| s == 409).count(), 1, "expect_status idempotent");
    }

    #[test]
    fn stage_builder_overrides_default() {
        let spec = RequestSpec::new(Method::GET, "/v1/things").stage("reconciliation");
        assert_eq!(spec.stage, "reconciliation");
    }
}
