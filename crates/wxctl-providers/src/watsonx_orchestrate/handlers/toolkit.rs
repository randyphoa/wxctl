use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use tracing::Instrument;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::util::{extract_artifact_path, set_source_hash_tag};

pub struct ToolkitHandler;

impl ResourceHandler for ToolkitHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            build_artifact_if_local(resource)?;
            ensure_mcp_defaults(resource);
            Ok(HookOutcome::Continue)
        })
    }

    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let toolkit_id = response.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No toolkit ID in create response"))?.to_string();
            finalize_toolkit(resource, response, client, operation_id, &toolkit_id).await
        })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            build_artifact_if_local(resource)?;
            ensure_mcp_defaults(resource);
            Ok(HookOutcome::Continue)
        })
    }

    fn post_update<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let toolkit_id = response.get("id").or_else(|| resource.get("id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No toolkit ID in update response or resource"))?.to_string();
            finalize_toolkit(resource, response, client, operation_id, &toolkit_id).await
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            enrich_toolkit_tools(remote_data, client, operation_id).await?;
            Ok(())
        })
    }
}

/// Shared finalize tail for `post_create` / `post_update` (they differ only in how
/// `toolkit_id` is sourced): upload the local artifact if present, re-fetch the
/// toolkit to get full state (the POST/PATCH response may omit fields like `name`
/// that enrichment needs), merge it into `response`, then enrich the tools map.
async fn finalize_toolkit(resource: &Value, response: &mut Value, client: &HttpClient, operation_id: &str, toolkit_id: &str) -> Result<()> {
    if let Some(artifact_path) = extract_artifact_path(resource) {
        upload_toolkit_artifact(client, toolkit_id, &artifact_path, operation_id).await?;
    }

    // Re-fetch toolkit to get full state including populated tools array
    // (the initial POST response may omit fields like `name` that enrichment needs)
    let refreshed: Value = client.get(operation_id, &format!("/v1/orchestrate/toolkits/{}", toolkit_id)).await?;
    if let Value::Object(refreshed_obj) = refreshed {
        for (key, value) in refreshed_obj {
            response[&key] = value;
        }
    }

    // Enrich tools: convert UUID array to name-to-UUID map
    enrich_toolkit_tools(response, client, operation_id).await
}

/// ADK always sends `connections: {}` in the mcp payload; ensure wxctl does too.
fn ensure_mcp_defaults(resource: &mut Value) {
    if let Some(mcp) = resource.get_mut("mcp").and_then(|v| v.as_object_mut()) {
        mcp.entry("connections").or_insert_with(|| json!({}));
    }
}

/// Fetch tool details for each tool UUID in the toolkit response and convert
/// `tools: ["uuid-1", "uuid-2"]` to `tools: {"hello": "uuid-1", "goodbye": "uuid-2"}`.
/// Tool names from the API are in `toolkit_name:tool_name` format; we strip the prefix.
async fn enrich_toolkit_tools(data: &mut Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    let toolkit_name = data.get("name").and_then(|v| v.as_str()).unwrap_or("");
    let prefix = format!("{}:", toolkit_name);

    let tool_uuids: Vec<String> = match data.get("tools").and_then(|v| v.as_array()) {
        Some(arr) => arr.iter().filter_map(|v| v.as_str().map(String::from)).collect(),
        None => return Ok(()),
    };

    if tool_uuids.is_empty() {
        data["tools"] = json!({});
        return Ok(());
    }

    // Fetch all tools in parallel; a failed GET propagates (naming the tool) instead
    // of silently dropping the tool from the name→UUID map, which would otherwise
    // surface later as a misleading downstream ref error.
    let fetches = tool_uuids.iter().map(|uuid| {
        let client = client.clone();
        let op_id = operation_id.to_string();
        async move {
            let endpoint = format!("/v1/orchestrate/tools/{}", uuid);
            let tool_data: Value = client.get::<Value>(&op_id, &endpoint).await.with_context(|| format!("toolkit: failed to fetch tool {uuid} while enriching tools map"))?;
            Ok((uuid.clone(), tool_data))
        }
    });
    let results = crate::util::join_all_ok(fetches).await?;

    let mut tool_map = serde_json::Map::new();
    for (uuid, tool_data) in results {
        if let Some(full_name) = tool_data.get("name").and_then(|v| v.as_str()) {
            let short_name = full_name.strip_prefix(&prefix).unwrap_or(full_name);
            tool_map.insert(short_name.to_string(), json!(uuid));
        } else {
            tracing::warn!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "toolkit",
                tool_uuid = %uuid,
                "tool has no name field in API response; skipping"
            );
        }
    }

    data["tools"] = Value::Object(tool_map);
    Ok(())
}

fn build_artifact_if_local(resource: &mut Value) -> Result<()> {
    let server_path_str = match resource.get("server_path").and_then(|v| v.as_str()) {
        Some(path) => path.to_string(),
        None => return Ok(()),
    };

    let builder = super::super::mcp::McpArtifactBuilder::new(PathBuf::from(&server_path_str))?;
    let (artifact_path, source_hash) = builder.build()?;

    resource["artifact"] = json!({"path": artifact_path.to_string_lossy().to_string()});
    set_source_hash_tag(resource, &source_hash);

    Ok(())
}

fn upload_toolkit_artifact<'a>(client: &'a HttpClient, toolkit_id: &'a str, artifact_path: &'a str, operation_id: &'a str) -> impl Future<Output = Result<()>> + Send + 'a {
    let span = tracing::debug_span!(
        target: "wxctl::substage::provider",
        "upload_toolkit_artifact",
        operation_id = %operation_id,
        toolkit_id = %toolkit_id,
        artifact_path = %artifact_path
    );

    async move {
        let endpoint = format!("/v1/orchestrate/toolkits/{}/upload", toolkit_id);
        crate::util::upload_artifact_and_cleanup(artifact_path, || async { client.upload_file(operation_id, &endpoint, Path::new(artifact_path), "file").await.map(|_| ()).context("Failed to upload toolkit artifact") }).await
    }
    .instrument(span)
}
