use serde_json::{Value, json};

/// Extract input_schema from OpenAPI parameters and requestBody.
/// Parameters are prefixed by location: query_{name}, header_{name}, path_{name}.
/// Request body becomes __requestBody__ with in: body.
/// Matches ADK openapi_tool.py lines 162-185.
pub fn extract_input_schema(parameters: &[Value], request_body: Option<&Value>) -> Value {
    let mut properties = serde_json::Map::new();
    let mut required = Vec::new();

    for param in parameters {
        let name = param.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let location = param.get("in").and_then(|v| v.as_str()).unwrap_or("query");
        let is_required = param.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
        let schema = param.get("schema").cloned().unwrap_or(json!({"type": "string"}));

        let prefixed_name = format!("{}_{}", location, name);
        let mut prop = schema;
        prop["title"] = json!(name);
        if let Some(desc) = param.get("description") {
            prop["description"] = desc.clone();
        }
        prop["in"] = json!(location);
        prop["aliasName"] = json!(name);

        properties.insert(prefixed_name.clone(), prop);

        if is_required {
            required.push(json!(prefixed_name));
        }
    }

    if let Some(body) = request_body {
        let body_schema = body.pointer("/content/application~1json/schema").cloned().unwrap_or(json!({"type": "object"}));

        let mut request_body_prop = body_schema;
        request_body_prop["title"] = json!("RequestBody");
        request_body_prop["in"] = json!("body");
        if let Some(desc) = body.get("description") {
            request_body_prop["description"] = desc.clone();
        } else {
            request_body_prop["description"] = json!("The html request body used to satisfy this user utterance.");
        }

        let body_required = body.get("required").and_then(|v| v.as_bool()).unwrap_or(false);
        properties.insert("__requestBody__".to_string(), request_body_prop);
        if body_required {
            required.push(json!("__requestBody__"));
        }
    }

    json!({
        "type": "object",
        "properties": properties,
        "required": required
    })
}

/// Extract output_schema from an OpenAPI response object.
/// If `success_status_code` is provided, check that code first before falling back to
/// the standard 2xx codes. Matches ADK openapi_tool.py lines 187-194.
pub fn extract_output_schema(responses: &Value, success_status_code: Option<u16>) -> Value {
    let mut codes: Vec<String> = Vec::new();
    if let Some(code) = success_status_code {
        codes.push(code.to_string());
    }
    for code in &["200", "201", "202", "204"] {
        if !codes.iter().any(|c| c == *code) {
            codes.push(code.to_string());
        }
    }

    for code in &codes {
        if let Some(response) = responses.get(code.as_str()) {
            let description = response.get("description").and_then(|v| v.as_str()).unwrap_or("");
            if let Some(schema) = response.pointer("/content/application~1json/schema") {
                let mut output = schema.clone();
                if !description.is_empty() {
                    output["description"] = json!(description);
                }
                return output;
            }
            return json!({"type": "object", "description": description});
        }
    }
    json!({"type": "object"})
}

/// Extract security schemes from an operation or fall back to global security.
/// Matches ADK openapi_tool.py lines 199-221.
pub fn extract_security(operation: &Value, spec: &Value) -> Vec<Value> {
    if let Some(security) = operation.get("security").and_then(|v| v.as_array()) {
        return resolve_security_refs(security, spec);
    }

    if let Some(security) = spec.get("security").and_then(|v| v.as_array()) {
        return resolve_security_refs(security, spec);
    }

    Vec::new()
}

fn resolve_security_refs(security: &[Value], spec: &Value) -> Vec<Value> {
    let schemes = spec.pointer("/components/securitySchemes");
    let mut result = Vec::new();

    for requirement in security {
        if let Some(obj) = requirement.as_object() {
            for scheme_name in obj.keys() {
                if let Some(scheme) = schemes.and_then(|s| s.get(scheme_name)) {
                    result.push(scheme.clone());
                }
            }
        }
    }

    result
}

/// Derive permission from x-ibm-operation.action field.
/// Actions starting with create/update/delete → read_write, otherwise read_only.
/// Matches ADK openapi_tool.py lines 350-355.
pub fn derive_permission(operation: &Value, default: Option<&str>) -> String {
    if let Some(action) = operation.pointer("/x-ibm-operation/action").and_then(|v| v.as_str()) {
        let lower = action.to_lowercase();
        if lower.starts_with("create") || lower.starts_with("update") || lower.starts_with("delete") {
            return "read_write".to_string();
        }
        return "read_only".to_string();
    }
    default.unwrap_or("read_only").to_string()
}

/// Extract server URL from spec. Returns first server URL.
pub fn extract_servers(spec: &Value) -> Vec<String> {
    spec.get("servers").and_then(|v| v.as_array()).map(|servers| servers.iter().filter_map(|s| s.get("url").and_then(|u| u.as_str()).map(|s| s.to_string())).collect()).unwrap_or_default()
}

/// Detect if an operation uses callbacks (async pattern).
/// NOTE: This only detects presence of callbacks. Full callback/acknowledgement
/// binding extraction (ADK openapi_tool.py lines 223-300) is deferred to a follow-up.
/// The tool schema does not yet have an `is_async` field, so this value is currently
/// unused in the generated tool data. Kept for future async tool support.
pub fn is_async_operation(operation: &Value) -> bool {
    operation.get("callbacks").is_some()
}
