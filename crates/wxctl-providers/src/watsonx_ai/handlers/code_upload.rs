//! Shared code upload helper for AI services and functions.
//!
//! Both ai_service and wml_function upload gzipped Python code after creation
//! via a PUT to `<base>/<id>/code?version=2024-01-01`.

use anyhow::{Result, anyhow};
use flate2::{Compression, write::GzEncoder};
use reqwest::Method;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};

/// Fields patchable via PATCH on WML functions and ai_services.
/// Must match the `state_fields` in wml_function.yaml / ai_service.yaml.
const PATCHABLE_FIELDS: &[&str] = &["name", "description", "tags", "custom"];

/// Append optional `space_id` and `project_id` query params from resource data.
fn with_scope_params(mut spec: RequestSpec, resource: &Value) -> RequestSpec {
    if let Some(space_id) = resource.get("space_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("space_id", space_id);
    }
    if let Some(project_id) = resource.get("project_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("project_id", project_id);
    }
    spec
}

/// Wrap a `software_spec` string value as `{"id": "..."}` object.
/// The ML API expects this format for both ai_service and wml_function.
pub fn wrap_software_spec(resource: &mut Value, operation_id: &str) {
    if let Some(spec_id) = resource.get("software_spec").and_then(|v| v.as_str()).map(|s| s.to_string()) {
        resource["software_spec"] = json!({"id": spec_id});
        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            software_spec_id = %spec_id,
            "wrapped software_spec as id object"
        );
    }
}

/// Compute source hash and inject as a `source-hash:<hash>` tag.
/// No-op if `source_path` is absent. Used by both WmlFunctionHandler and AiServiceHandler.
pub fn hash_and_tag_source(resource: &mut Value) -> Result<()> {
    if let Some(source_path) = resource.get("source_path").and_then(|v| v.as_str()) {
        let hash = crate::util::hash_file_blake3(Path::new(source_path))?;
        crate::util::set_source_hash_tag(resource, &hash[..16]);
    }
    Ok(())
}

/// PATCH metadata + upload code for a WML asset update.
///
/// The WML PATCH endpoint requires `space_id`/`project_id` as query parameters
/// and uses RFC 6902 JSON Patch body format with `application/json` content type.
/// The default engine update path doesn't handle these WML-specific requirements,
/// so this handler performs the full update via `RequestSpec`.
pub async fn patch_and_upload(current: &Value, desired: &Value, client: &HttpClient, api_path: &str, operation_id: &str) -> Result<Value> {
    let id = crate::util::resource_id(current).ok_or_else(|| anyhow!("[{operation_id}] Could not find resource ID for update"))?;

    // Build RFC 6902 JSON Patch ops.
    // WML paths are under /metadata/ (name, description, tags) or /custom/.
    // Only include fields that differ between local and remote to avoid patching unchanged fields.
    // The remote response nests fields under "metadata" — check both locations.
    let mut patch_ops = Vec::new();
    for field in PATCHABLE_FIELDS {
        if let Some(local_val) = desired.get(*field) {
            let remote_val = current.get(*field).or_else(|| current.pointer(&format!("/metadata/{field}")));
            if remote_val == Some(local_val) {
                continue;
            }
            let path = if *field == "custom" { format!("/{field}") } else { format!("/metadata/{field}") };
            let op = if remote_val.is_some() { "replace" } else { "add" };
            patch_ops.push(json!({"op": op, "path": path, "value": local_val}));
        }
    }

    let endpoint = format!("{api_path}/{id}");
    let spec = RequestSpec::new(Method::PATCH, endpoint).query_param("version", "2024-01-01").body(BodyKind::Json(json!(patch_ops)));
    let response_body: Value = client.execute(operation_id, with_scope_params(spec, desired)).await?;

    // Upload code if source_path is present
    let id_holder = json!({"id": id});
    upload_code(desired, &id_holder, client, api_path, operation_id).await?;

    // Merge response onto current remote data so computed fields remain available
    let mut merged = current.clone();
    if let (Some(base), Some(overlay)) = (merged.as_object_mut(), response_body.as_object()) {
        base.extend(overlay.iter().map(|(k, v)| (k.clone(), v.clone())));
    }
    Ok(merged)
}

/// Upload gzipped source code to a WML asset's `/code` endpoint.
///
/// `api_path` is the base API path (e.g. `/ml/v4/ai_services` or `/ml/v4/functions`).
pub async fn upload_code(resource: &Value, response: &Value, client: &HttpClient, api_path: &str, operation_id: &str) -> Result<()> {
    let source_path = match resource.get("source_path").and_then(|v| v.as_str()) {
        Some(p) => p.to_string(),
        None => {
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                api_path = %api_path,
                reason = "no_source_path",
                "skipping code upload"
            );
            return Ok(());
        }
    };

    let content = std::fs::read(&source_path).map_err(|e| anyhow!("[{operation_id}] Failed to read source file '{source_path}': {e}"))?;

    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(&content).map_err(|e| anyhow!("[{operation_id}] Failed to compress content: {e}"))?;
    let gzipped = encoder.finish().map_err(|e| anyhow!("[{operation_id}] Failed to finish gzip encoding: {e}"))?;

    let id = crate::util::resource_id(response).ok_or_else(|| anyhow!("[{operation_id}] Could not find resource ID in response"))?;

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        api_path = %api_path,
        resource_id = %id,
        bytes = gzipped.len(),
        "uploading gzipped code"
    );

    let endpoint = format!("{api_path}/{id}/code");
    let spec = RequestSpec::new(Method::PUT, endpoint).query_param("version", "2024-01-01").header("Content-Type", "application/gzip").body(BodyKind::OctetStream(gzipped));
    let _: Value = client.execute(operation_id, with_scope_params(spec, resource)).await?;
    Ok(())
}
