use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec, error_matches, join_url};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct DataAssetHandler;

/// Append optional `space_id` / `project_id` query params from the resource.
fn with_scope(mut spec: RequestSpec, resource: &Value) -> RequestSpec {
    if let Some(s) = resource.get("space_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("space_id", s);
    }
    if let Some(p) = resource.get("project_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("project_id", p);
    }
    spec
}

impl ResourceHandler for DataAssetHandler {
    /// Own the full create sequence: POST the CAMS envelope, upload the attachment,
    /// and return Handled so the engine skips its default POST.
    ///
    /// The materializer only serialises declared schema fields, so any injected
    /// `metadata`/`entity` keys would be silently dropped on the wire. We therefore
    /// build the body ourselves, POST it, then upload the attachment — all before
    /// returning `Handled(resp)`.  On error the engine still calls
    /// `recover_from_create_error`, so idempotency adoption works unchanged.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] data_asset requires name"))?.to_string();
            let description = resource.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let mime_type = resource.get("mime_type").and_then(|v| v.as_str()).unwrap_or("text/csv").to_string();

            let create_body = json!({
                "metadata": {"name": name, "description": description, "asset_type": "data_asset", "origin_country": "us"},
                "entity": {"data_asset": {"mime_type": mime_type}}
            });

            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "data_asset",
                name = %name,
                mime_type = %mime_type,
                "posting CAMS asset-create envelope"
            );

            let spec = with_scope(RequestSpec::new(Method::POST, "/v2/assets").body(BodyKind::Json(create_body)), resource);
            let resp: Value = client.execute(operation_id, spec).await.map_err(|e| anyhow!("[{operation_id}] data_asset create POST failed: {e}"))?;

            // Upload the file attachment; `resource` is untouched so source_path/name/mime_type are still readable.
            upload_data_asset(resource, &resp, client, operation_id).await?;

            Ok(HookOutcome::Handled(resp))
        })
    }

    /// Idempotency (Q2): a fresh apply with an unknown asset_id POSTs a new shell;
    /// if the asset already exists by name in the scope, adopt it. The /v2/assets
    /// POST itself does not 409 on duplicate name, so on any create error we search
    /// by name and return the existing shell rather than fail the apply.
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            // Only attempt recovery on conflicts / bad-request, not auth/5xx.
            if !(error_matches(error, 409, &[]) || error_matches(error, 400, &["already", "exists"])) {
                return Ok(None);
            }
            let Some(name) = resource.get("name").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            match find_asset_by_name(resource, name, client, operation_id).await? {
                Some(existing) => {
                    tracing::debug!(
                        target: "wxctl::substage::provider",
                        operation_id = %operation_id,
                        resource_type = "data_asset",
                        name = %name,
                        "adopt: existing data_asset matched by name"
                    );
                    Ok(Some(existing))
                }
                None => Ok(None),
            }
        })
    }

    /// Expose the asset id top-level on the discovery (re-plan / re-apply) path so
    /// downstream refs `${data_asset.x.asset_id}` resolve — the engine stores the
    /// matched CAMS search result verbatim with the id only at metadata.asset_id,
    /// and the template resolver does a strict top-level lookup. Mirrors the
    /// top-level asset_id the create / recover_from_create_error paths already fold.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(id) = remote_data.pointer("/metadata/asset_id").and_then(|v| v.as_str()).map(|s| s.to_string())
                && let Some(obj) = remote_data.as_object_mut()
            {
                obj.insert("asset_id".to_string(), json!(id));
            }
            Ok(())
        })
    }
}

/// Create the attachment, PUT the file bytes, then finalize via complete.
async fn upload_data_asset(resource: &Value, response: &Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    let source_path = resource.get("source_path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] data_asset requires source_path"))?;
    let mime_type = resource.get("mime_type").and_then(|v| v.as_str()).unwrap_or("text/csv");
    let asset_name = resource.get("name").and_then(|v| v.as_str()).unwrap_or("data");

    // The CAMS create response nests asset_id under metadata.
    let asset_id = response.pointer("/metadata/asset_id").or_else(|| response.get("asset_id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] no asset_id in create response: {}", serde_json::to_string_pretty(response).unwrap_or_default()))?.to_string();

    // 1. Create attachment — the server returns a presigned upload URL.
    let att_body = json!({
        "asset_type": "data_asset",
        "name": asset_name,
        "mime": mime_type
    });
    let att_spec = with_scope(RequestSpec::new(Method::POST, format!("/v2/assets/{asset_id}/attachments")).body(BodyKind::Json(att_body)), resource);
    let att: Value = client.execute(operation_id, att_spec).await.map_err(|e| anyhow!("[{operation_id}] data_asset attachment create failed (asset_id={asset_id}): {e}"))?;

    let attachment_id = att.get("attachment_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] no attachment_id in attachment response: {}", serde_json::to_string_pretty(&att).unwrap_or_default()))?.to_string();

    // The presigned upload URL may be absolute (SaaS COS) or relative (Software gateway).
    let upload_href_raw = att.get("url1").or_else(|| att.get("url")).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] no presigned upload url in attachment response: {}", serde_json::to_string_pretty(&att).unwrap_or_default()))?;
    let is_relative = !(upload_href_raw.starts_with("http://") || upload_href_raw.starts_with("https://"));
    let upload_href_owned = if is_relative { join_url(client.base_url(), "", upload_href_raw) } else { upload_href_raw.to_string() };
    let upload_href = upload_href_owned.as_str();

    // 2. Read file and PUT the bytes.
    let content = std::fs::read(source_path).map_err(|e| anyhow!("[{operation_id}] failed to read '{source_path}': {e}"))?;
    let bytes_len = content.len();

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "data_asset",
        asset_id = %asset_id,
        bytes = bytes_len,
        relative_href = is_relative,
        "uploading data_asset file"
    );

    let token = client.get_token().await?;
    // Software gateway paths want multipart/form-data (same pattern as package_extension).
    // SaaS COS presigned URLs want a binary PUT with Content-Type.
    let put_resp = if is_relative {
        let file_name = std::path::Path::new(source_path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".to_string());
        let part = reqwest::multipart::Part::bytes(content).file_name(file_name).mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("file", part);
        client.raw_client().put(upload_href).bearer_auth(&token).multipart(form).send().await
    } else {
        client.raw_client().put(upload_href).bearer_auth(&token).header("Content-Type", mime_type).body(content).send().await
    }
    .map_err(|e| anyhow!("[{operation_id}] data_asset file PUT failed (asset_id={asset_id}): {e}"))?;

    if !put_resp.status().is_success() {
        let status = put_resp.status();
        let body = put_resp.text().await.unwrap_or_default();
        return Err(anyhow!("[{operation_id}] data_asset file PUT failed ({status}) for asset_id={asset_id}: {body}"));
    }

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "data_asset",
        asset_id = %asset_id,
        "file uploaded; completing attachment"
    );

    // 3. Finalize the attachment.
    let complete_spec = with_scope(RequestSpec::new(Method::POST, format!("/v2/assets/{asset_id}/attachments/{attachment_id}/complete")).body(BodyKind::Json(json!({}))), resource);
    let _: Value = client.execute(operation_id, complete_spec).await.map_err(|e| anyhow!("[{operation_id}] data_asset attachment complete failed (asset_id={asset_id}): {e}"))?;

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "data_asset",
        asset_id = %asset_id,
        bytes = bytes_len,
        "data_asset uploaded and finalized"
    );
    Ok(())
}

/// Search the scope's assets by exact name for idempotency recovery. Returns the
/// matched asset shell or None.
async fn find_asset_by_name(resource: &Value, name: &str, client: &HttpClient, operation_id: &str) -> Result<Option<Value>> {
    let body = json!({"query": format!("asset.name:\"{name}\""), "limit": 1});
    let spec = with_scope(RequestSpec::new(Method::POST, "/v2/asset_types/data_asset/search").body(BodyKind::Json(body)), resource);
    let resp: Value = match client.execute(operation_id, spec).await {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let Some(first) = resp.get("results").and_then(|r| r.as_array()).and_then(|a| a.first()) else {
        return Ok(None);
    };
    let asset_id = first.pointer("/metadata/asset_id").and_then(|v| v.as_str());
    Ok(asset_id.map(|id| json!({"metadata": {"asset_id": id}, "asset_id": id})))
}
