use super::error_codes;
use serde_json::Value;

/// Classify HTTP status code to WXCTL error code
pub fn classify_http_error(status: u16) -> &'static str {
    match status {
        401 | 403 => error_codes::H004,
        400..=499 => error_codes::H001,
        500..=599 => error_codes::H002,
        _ => error_codes::H003,
    }
}

/// Generate remediation text based on HTTP status and API response body
pub fn suggest_http_fix(status: u16, body: &Value) -> String {
    match status {
        401 => "Authentication failed. Check that WXCTL_API_KEY is set and valid.".to_string(),
        403 => "Access denied. Verify IAM permissions for this resource.".to_string(),
        404 => "Resource not found. Verify the resource ID or endpoint path.".to_string(),
        409 => {
            let msg = extract_api_error_message(body);
            format!("Conflict: {}. Delete the existing resource with 'wxctl destroy' or use a different name.", msg)
        }
        429 => "Rate limited. Retry after a delay or reduce parallelism with --parallelism flag.".to_string(),
        500..=599 => format!("Server error (HTTP {}). This is a transient issue — retry the operation.", status),
        _ => format!("HTTP {} error. Check the response body for details.", status),
    }
}

/// Extract error message from common IBM API error response formats
pub fn extract_api_error_message(body: &Value) -> String {
    if let Some(msg) = body.pointer("/errors/0/message").and_then(|v| v.as_str()) {
        return msg.to_string();
    }
    if let Some(msg) = body.pointer("/error/message").and_then(|v| v.as_str()) {
        return msg.to_string();
    }
    if let Some(msg) = body.get("message").and_then(|v| v.as_str()) {
        return msg.to_string();
    }
    if let Some(msg) = body.get("error_message").and_then(|v| v.as_str()) {
        return msg.to_string();
    }
    if let Some(msg) = body.get("error").and_then(|v| v.as_str()) {
        return msg.to_string();
    }
    if let Some(msg) = body.get("exception").and_then(|v| v.as_str()) {
        return msg.to_string();
    }
    // FastAPI/pydantic validation errors arrive as {"detail":[{"loc":[...],"msg":...,"type":...}, ...]}.
    // Extract the loc/msg/type tuples so the user sees which field failed without staring at a wall
    // of echoed input. Falls back to the raw body snippet if the shape is unfamiliar.
    if let Some(detail) = body.get("detail").and_then(|v| v.as_array()) {
        let items: Vec<String> = detail
            .iter()
            .filter_map(|d| {
                let loc = d.get("loc").and_then(|v| v.as_array()).map(|arr| arr.iter().filter_map(|p| p.as_str().map(String::from).or_else(|| p.as_u64().map(|n| n.to_string()))).collect::<Vec<_>>().join(".")).unwrap_or_default();
                let msg = d.get("msg").and_then(|v| v.as_str()).unwrap_or("");
                let ty = d.get("type").and_then(|v| v.as_str()).unwrap_or("");
                if msg.is_empty() && ty.is_empty() && loc.is_empty() { None } else { Some(format!("{loc}: {msg} ({ty})")) }
            })
            .collect();
        if !items.is_empty() {
            return format!("validation error: {}", items.join("; "));
        }
    }
    // Redaction happens at log emission (see `redact_sensitive`); this path just surfaces
    // the shape. Without it, novel error formats degrade to a blind "Unknown error".
    fallback_body_snippet(body)
}

fn fallback_body_snippet(body: &Value) -> String {
    const MAX_LEN: usize = 500;
    let raw = match body {
        Value::String(s) => s.clone(),
        _ => serde_json::to_string(body).unwrap_or_else(|_| "Unknown error".to_string()),
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return "Unknown error (empty response body)".to_string();
    }
    let collapsed: String = trimmed.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    if collapsed.chars().count() > MAX_LEN {
        let truncated: String = collapsed.chars().take(MAX_LEN).collect();
        format!("{truncated}…")
    } else {
        collapsed
    }
}

/// Extract a trace/correlation identifier from an IBM API error body so the
/// user can cite it when opening a support ticket. Watsonx APIs use at least
/// three spellings (`trace`, `trace_id`, `traceId`) and sometimes nest it
/// inside `errors[0]`. Falls back to `None` if nothing is found.
pub fn extract_trace_id(body: &Value) -> Option<String> {
    for path in ["/trace", "/trace_id", "/traceId", "/errors/0/trace", "/errors/0/trace_id"] {
        if let Some(s) = body.pointer(path).and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn classify_http_error_buckets_status_to_code() {
        // 401/403 → auth (H004); other 4xx → client (H001); 5xx → server (H002).
        let cases = [
            (401u16, "WXCTL-H004"), // auth
            (403, "WXCTL-H004"),    // auth
            (400, "WXCTL-H001"),    // client
            (404, "WXCTL-H001"),    // client
            (409, "WXCTL-H001"),    // client
            (500, "WXCTL-H002"),    // server
            (503, "WXCTL-H002"),    // server
        ];
        for (status, expected) in cases {
            assert_eq!(classify_http_error(status), expected, "status={status}");
        }
    }

    #[test]
    fn extract_api_error_message_walks_known_shapes() {
        // Each IBM/FastAPI error envelope shape resolves to its human message,
        // in the precedence order encoded by `extract_api_error_message`.
        let cases = [
            // errors[0].message (IBM standard error array)
            (json!({"errors": [{"code": "already_exists", "message": "Connection exists"}]}), "Connection exists"),
            // error.message (nested object)
            (json!({"error": {"message": "Not found"}}), "Not found"),
            // flat top-level message
            (json!({"message": "Bad request"}), "Bad request"),
            // top-level `exception` surfaces when the errors array is empty:
            // watsonx.data /v3/spark_engines 500 shape carries extra context here.
            (json!({"errors": [], "exception": "validation failure list: bad field"}), "validation failure list: bad field"),
            // unknown shape → caller still sees the raw payload, not a blind "Unknown error".
            (json!({"status": 500}), r#"{"status":500}"#),
            // plain string body passes through verbatim
            (json!("Internal Server Error"), "Internal Server Error"),
            // empty body → explicit sentinel
            (json!(""), "Unknown error (empty response body)"),
        ];
        for (body, expected) in cases {
            assert_eq!(extract_api_error_message(&body), expected, "body={body:?}");
        }
    }

    #[test]
    fn extract_fastapi_detail() {
        let body = json!({
            "detail": [
                {"loc": ["body", "tools", 0], "msg": "Input should be a valid string", "type": "string_type"},
                {"loc": ["body", "style"], "msg": "Field required", "type": "missing"}
            ]
        });
        let msg = extract_api_error_message(&body);
        assert!(msg.starts_with("validation error:"), "msg={msg}");
        assert!(msg.contains("body.tools.0"));
        assert!(msg.contains("body.style"));
        assert!(msg.contains("Field required"));
    }

    #[test]
    fn extract_fallback_truncates_long_body() {
        let long = "x".repeat(2000);
        let body = json!(long);
        let msg = extract_api_error_message(&body);
        assert!(msg.ends_with('…'));
        assert!(msg.chars().count() <= 501);
    }

    #[test]
    fn suggest_fix_409() {
        let body = json!({"errors": [{"message": "resource already exists"}]});
        let fix = suggest_http_fix(409, &body);
        assert!(fix.contains("resource already exists"));
        assert!(fix.contains("wxctl destroy"));
    }

    #[test]
    fn extract_trace_id_across_spellings_and_nesting() {
        // watsonx APIs spell the correlation id at least three ways and sometimes
        // nest it inside errors[0]; absent → None.
        let cases = [
            (json!({"errors": [{"message": "x"}], "trace": "abc-123"}), Some("abc-123".to_string())),        // top-level `trace`
            (json!({"trace_id": "xyz-789"}), Some("xyz-789".to_string())),                                   // snake_case
            (json!({"traceId": "cam-456"}), Some("cam-456".to_string())),                                    // camelCase
            (json!({"errors": [{"message": "x", "trace_id": "nested-42"}]}), Some("nested-42".to_string())), // nested in errors[0]
            (json!({"errors": [{"message": "x"}]}), None),                                                   // absent
        ];
        for (body, expected) in cases {
            assert_eq!(extract_trace_id(&body), expected, "body={body:?}");
        }
    }
}
