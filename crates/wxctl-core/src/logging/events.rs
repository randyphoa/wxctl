use serde::Serialize;
use serde_json::Value;

/// Stage lifecycle event
#[derive(Debug, Clone, Serialize)]
pub struct StageEvent {
    pub operation_id: String,
    pub stage: String,
    pub status: String,
    pub resource_count: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Reconciliation decision event
#[derive(Debug, Clone, Serialize)]
pub struct DecisionEvent {
    pub operation_id: String,
    pub resource_type: String,
    pub resource_name: String,
    pub decision: String,
    pub reason: String,
    pub field_diffs: Vec<FieldDiff>,
}

/// Field difference in reconciliation
#[derive(Debug, Clone, Serialize)]
pub struct FieldDiff {
    pub path: String,
    pub local: serde_json::Value,
    pub remote: serde_json::Value,
}

/// Dependency resolution event
#[derive(Debug, Clone, Serialize)]
pub struct DependencyEvent {
    pub operation_id: String,
    pub resource_type: String,
    pub resource_name: String,
    pub depends_on_type: String,
    pub depends_on_name: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resolved_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deferred_reason: Option<String>,
}

/// Error event with context
#[derive(Debug, Clone, Serialize)]
pub struct ErrorEvent {
    pub operation_id: String,
    pub stage: String,
    pub error_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
    pub message: String,
    pub fix: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
}

/// Builder for [`ErrorEvent`]
pub struct ErrorEventBuilder {
    error_code: String,
    stage: String,
    message: String,
    resource_type: Option<String>,
    resource_name: Option<String>,
    field_path: Option<String>,
    fix: String,
    cause: Option<String>,
    caused_by: Option<String>,
    expected: Option<String>,
    actual: Option<String>,
    context: Option<Value>,
}

impl ErrorEventBuilder {
    pub fn new(error_code: impl Into<String>, stage: impl Into<String>, message: impl Into<String>) -> Self {
        Self { error_code: error_code.into(), stage: stage.into(), message: message.into(), resource_type: None, resource_name: None, field_path: None, fix: String::new(), cause: None, caused_by: None, expected: None, actual: None, context: None }
    }

    pub fn resource(mut self, kind: impl Into<String>, name: impl Into<String>) -> Self {
        self.resource_type = Some(kind.into());
        self.resource_name = Some(name.into());
        self
    }

    pub fn field(mut self, path: impl Into<String>) -> Self {
        self.field_path = Some(path.into());
        self
    }

    pub fn cause(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(cause.into());
        self
    }

    pub fn caused_by(mut self, upstream_code: impl Into<String>) -> Self {
        self.caused_by = Some(upstream_code.into());
        self
    }

    pub fn expected(mut self, expected: impl Into<String>) -> Self {
        self.expected = Some(expected.into());
        self
    }

    pub fn actual(mut self, actual: impl Into<String>) -> Self {
        self.actual = Some(actual.into());
        self
    }

    pub fn context(mut self, ctx: Value) -> Self {
        self.context = Some(ctx);
        self
    }

    pub fn fix(mut self, fix: impl Into<String>) -> Self {
        self.fix = fix.into();
        self
    }

    pub fn build(self) -> ErrorEvent {
        ErrorEvent {
            operation_id: String::new(),
            stage: self.stage,
            error_code: self.error_code,
            resource_type: self.resource_type,
            resource_name: self.resource_name,
            field_path: self.field_path,
            message: self.message,
            fix: self.fix,
            cause: self.cause,
            caused_by: self.caused_by,
            expected: self.expected,
            actual: self.actual,
            context: self.context,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_event_builder_threads_setters_into_event() {
        // Minimal: only the required new() args + fix; optional setters stay None.
        let minimal = ErrorEventBuilder::new("WXCTL-E001", "execution", "Create failed").fix("Delete existing resource first").build();
        assert_eq!(minimal.error_code, "WXCTL-E001");
        assert_eq!(minimal.stage, "execution");
        assert_eq!(minimal.message, "Create failed");
        assert_eq!(minimal.fix, "Delete existing resource first");
        assert!(minimal.cause.is_none(), "unset optional stays None");

        // Full chain: every optional setter threads through to the built event.
        let full = ErrorEventBuilder::new("WXCTL-H001", "execution", "HTTP 409")
            .resource("orchestrate_connection", "my-conn")
            .cause("Resource already exists")
            .expected("HTTP 201")
            .actual("HTTP 409")
            .context(serde_json::json!({"status": 409}))
            .caused_by("WXCTL-E001")
            .fix("Delete existing resource or use a different name")
            .build();
        assert_eq!(full.resource_type, Some("orchestrate_connection".to_string()));
        assert_eq!(full.cause, Some("Resource already exists".to_string()));
        assert_eq!(full.caused_by, Some("WXCTL-E001".to_string()));

        // field() sets the field_path (validation-stage error).
        let with_field = ErrorEventBuilder::new("WXCTL-V003", "validation", "Schema failed").resource("catalog", "my-cat").field("name").fix("Add the required field").build();
        assert_eq!(with_field.field_path, Some("name".to_string()));
    }
}
