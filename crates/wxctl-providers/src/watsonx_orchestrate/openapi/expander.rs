use anyhow::{Context, Result};
use serde_json::json;
use std::path::PathBuf;

use super::spec_parser::parse_spec_file;
use wxctl_core::RawResource;

/// Expand OpenAPI tool resources into individual tool resources (one per endpoint).
/// Scans `resources` for tools with `binding.openapi` and a `spec_path` but no `http_method`,
/// indicating they need expansion. Returns a new list with expanded resources replacing the originals.
pub fn expand_openapi_resources(resources: Vec<RawResource>) -> Result<Vec<RawResource>> {
    let mut result = Vec::new();

    for resource in resources {
        if should_expand(&resource) {
            let expanded = expand_single_resource(&resource)?;
            result.extend(expanded);
        } else {
            result.push(resource);
        }
    }

    Ok(result)
}

/// A resource needs OpenAPI expansion if it has binding.openapi but no http_method
/// (meaning the user wants all endpoints auto-discovered from the spec).
fn should_expand(resource: &RawResource) -> bool {
    resource.kind == "tool" && resource.data.pointer("/binding/openapi").is_some() && resource.data.pointer("/binding/openapi/http_method").is_none() && resource.data.get("spec_path").and_then(|v| v.as_str()).is_some()
}

fn expand_single_resource(resource: &RawResource) -> Result<Vec<RawResource>> {
    let data = &resource.data;

    let spec_path_str = data.get("spec_path").and_then(|v| v.as_str()).context("spec_path is required for OpenAPI expansion")?;
    let spec_path = PathBuf::from(spec_path_str);

    let openapi_binding = data.pointer("/binding/openapi").context("binding.openapi is required")?;

    let connection_id = openapi_binding.get("connection_id").and_then(|v| v.as_str());

    let default_permission = openapi_binding.get("permission").and_then(|v| v.as_str());

    let tools_filter: Option<Vec<String>> = openapi_binding.get("tools").and_then(|v| v.as_array()).map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect());

    let parent_ref_name = data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("openapi");

    let parsed_tools = parse_spec_file(&spec_path, tools_filter.as_deref(), connection_id, default_permission)?;

    let mut expanded = Vec::new();

    for tool in parsed_tools {
        let ref_name = format!("{}_{}", parent_ref_name, tool.name);

        let mut tool_data = json!({
            "ref_name": ref_name,
            "name": tool.name,
            "description": tool.description,
            "permission": tool.permission,
            "input_schema": tool.input_schema,
            "output_schema": tool.output_schema,
            "spec_path": spec_path_str,
            "binding": {
                "openapi": tool.binding
            }
        });

        // Carry over tags from parent and add toolset tag for lifecycle tracking.
        // The toolset tag lets destroy know which expanded tools belong together.
        // Tags are an array of strings (matching set_source_hash_tag format in tool.rs).
        let mut tags: Vec<serde_json::Value> = data.get("tags").and_then(|v| v.as_array()).cloned().unwrap_or_default();
        tags.push(json!(format!("toolset:{}", parent_ref_name)));
        tool_data["tags"] = json!(tags);

        expanded.push(RawResource { kind: "tool".to_string(), data: tool_data });
    }

    Ok(expanded)
}
