use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::path::Path;

use super::ref_resolver::resolve_refs;
use super::schema_extractor::{derive_permission, extract_input_schema, extract_output_schema, extract_security, extract_servers, is_async_operation};

/// A single tool definition parsed from an OpenAPI spec endpoint.
#[derive(Debug, Clone)]
pub struct ParsedTool {
    pub name: String,
    pub description: String,
    pub permission: String,
    #[allow(dead_code)]
    pub is_async: bool,
    pub input_schema: Value,
    pub output_schema: Value,
    pub binding: Value,
}

/// Parse an OpenAPI spec file and return a list of tool definitions.
/// One tool per path+method combination, filtered by operation IDs.
pub fn parse_spec_file(spec_path: &Path, tools_filter: Option<&[String]>, connection_id: Option<&str>, default_permission: Option<&str>) -> Result<Vec<ParsedTool>> {
    let content = std::fs::read_to_string(spec_path).with_context(|| format!("Failed to read spec file: {}", spec_path.display()))?;

    let spec: Value = if spec_path.extension().is_some_and(|e| e == "json") { serde_json::from_str(&content)? } else { serde_norway::from_str(&content)? };

    let resolved = resolve_refs(&spec)?;
    parse_spec(&resolved, tools_filter, connection_id, default_permission)
}

/// Parse an already-loaded and ref-resolved OpenAPI spec.
pub fn parse_spec(spec: &Value, tools_filter: Option<&[String]>, connection_id: Option<&str>, default_permission: Option<&str>) -> Result<Vec<ParsedTool>> {
    let paths = spec.get("paths").and_then(|v| v.as_object()).context("OpenAPI spec must have a 'paths' object")?;

    let servers = extract_servers(spec);
    let filter_all = tools_filter.is_none() || tools_filter.is_some_and(|f| f.is_empty() || f.iter().any(|t| t == "*"));

    let mut tools = Vec::new();

    for (path, path_item) in paths {
        let path_obj = match path_item.as_object() {
            Some(obj) => obj,
            None => continue,
        };

        let path_params: Vec<Value> = path_obj.get("parameters").and_then(|v| v.as_array()).cloned().unwrap_or_default();

        let methods = ["get", "post", "put", "patch", "delete"];

        for method in &methods {
            let operation = match path_obj.get(*method) {
                Some(op) => op,
                None => continue,
            };

            let operation_id = match operation.get("operationId").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => {
                    continue;
                }
            };

            if !filter_all && !tools_filter.unwrap().iter().any(|f| f == operation_id) {
                continue;
            }

            let name = sanitize_operation_id(operation_id);

            let description = operation.get("description").or_else(|| operation.get("summary")).and_then(|v| v.as_str()).unwrap_or("").to_string();

            let mut all_params = path_params.clone();
            if let Some(op_params) = operation.get("parameters").and_then(|v| v.as_array()) {
                all_params.extend(op_params.clone());
            }

            let request_body = operation.get("requestBody");
            let input_schema = extract_input_schema(&all_params, request_body);

            let responses = operation.get("responses").cloned().unwrap_or(json!({}));
            // success_status_code will be wired through from the binding when available
            let output_schema = extract_output_schema(&responses, None);

            let security = extract_security(operation, spec);
            let permission = derive_permission(operation, default_permission);
            let is_async = is_async_operation(operation);

            let mut binding = json!({
                "http_method": method.to_uppercase(),
                "http_path": path,
                "servers": servers,
                "security": security,
            });

            if let Some(conn) = connection_id {
                binding["connection_id"] = json!(conn);
            }

            tools.push(ParsedTool { name, description, permission, is_async, input_schema, output_schema, binding });
        }
    }

    Ok(tools)
}

fn sanitize_operation_id(id: &str) -> String {
    let mut result = String::with_capacity(id.len());
    for c in id.chars() {
        if c.is_alphanumeric() || c == '_' {
            result.push(c);
        } else {
            result.push('_');
        }
    }
    result
}
