use anyhow::{Result, anyhow};
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec, join_url};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct PackageExtensionHandler;

impl ResourceHandler for PackageExtensionHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // The WML API requires interpreter: "mamba" for conda_yml extensions
            let ext_type = resource.get("type").and_then(|v| v.as_str()).unwrap_or("conda_yml").to_string();
            if ext_type == "conda_yml" || ext_type == "pip_zip" {
                resource["interpreter"] = serde_json::json!("mamba");
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "package_extension",
                    ext_type = %ext_type,
                    "set interpreter=mamba"
                );
            }
            Ok(HookOutcome::Continue)
        })
    }

    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { upload_package_file(resource, response, client, operation_id).await })
    }
}

async fn upload_package_file(resource: &Value, response: &Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    let source_path = resource.get("source_path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] package_extension requires source_path"))?;

    let upload_href_raw = response.pointer("/entity/package_extension/href").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] No upload href in response. Response: {}", serde_json::to_string_pretty(response).unwrap_or_default()))?;
    // SaaS returns an absolute presigned COS URL; Software returns a relative
    // path on the CPD gateway (e.g. `/v2/asset_files/...`). Resolve relatives
    // against the client's base URL so reqwest can PUT to it.
    let is_relative = !(upload_href_raw.starts_with("http://") || upload_href_raw.starts_with("https://"));
    let upload_href_owned = if is_relative { join_url(client.base_url(), "", upload_href_raw) } else { upload_href_raw.to_string() };
    let upload_href = upload_href_owned.as_str();

    let asset_id = response.pointer("/metadata/asset_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] No asset_id in response"))?;

    let content = std::fs::read(source_path).map_err(|e| anyhow!("[{operation_id}] Failed to read '{source_path}': {e}"))?;

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "package_extension",
        asset_id = %asset_id,
        bytes = content.len(),
        relative_href = is_relative,
        "uploading package_extension file"
    );

    let token = client.get_token().await?;
    let ext_type = resource.get("type").and_then(|v| v.as_str()).unwrap_or("conda_yml");

    // SaaS hands back an absolute presigned COS URL — binary PUT for conda_yml,
    // multipart for pip_zip. Software hands back a relative `/v2/asset_files/...`
    // gateway path that always wants multipart/form-data with field name "file"
    // and `application/octet-stream` part mime — same shape the Python SDK uses
    // for the non-CLOUD_PLATFORM_SPACES path (pkg_extn._upload_pkg_extn_file).
    let upload_resp = if is_relative || ext_type == "pip_zip" {
        let file_name = std::path::Path::new(source_path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".to_string());
        let part = reqwest::multipart::Part::bytes(content).file_name(file_name).mime_str("application/octet-stream")?;
        let form = reqwest::multipart::Form::new().part("file", part);
        // apply_auth_scheme (not hardcoded Bearer): the Software gateway path needs the
        // client's real scheme (zenapikey under SAML SSO); SaaS presigned URLs ignore it.
        client.apply_auth_scheme(client.raw_client().put(upload_href), &token)?.multipart(form).send().await
    } else {
        client.apply_auth_scheme(client.raw_client().put(upload_href), &token)?.header("Content-Type", "application/octet-stream").body(content).send().await
    }
    .map_err(|e| anyhow!("[{operation_id}] File upload request failed: {e}"))?;

    if !upload_resp.status().is_success() {
        let status = upload_resp.status();
        let err = upload_resp.text().await.unwrap_or_default();
        return Err(anyhow!("[{operation_id}] File upload failed ({}): {}", status, err));
    }

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "package_extension",
        asset_id = %asset_id,
        "file uploaded; marking upload complete"
    );

    let mut spec = RequestSpec::new(Method::POST, format!("/v2/package_extensions/{asset_id}/upload_complete")).body(BodyKind::Json(serde_json::json!({})));
    if let Some(space_id) = resource.get("space_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("space_id", space_id);
    }
    if let Some(project_id) = resource.get("project_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("project_id", project_id);
    }
    let _: Value = client.execute(operation_id, spec).await?;

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "package_extension",
        asset_id = %asset_id,
        "package extension upload complete"
    );
    Ok(())
}
