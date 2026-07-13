use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct NotebookHandler;

/// Append the optional `project_id` query param from the resource.
fn with_project(mut spec: RequestSpec, resource: &Value) -> RequestSpec {
    if let Some(p) = resource.get("project_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("project_id", p);
    }
    spec
}

impl ResourceHandler for NotebookHandler {
    /// Own the full create: PUT the .ipynb bytes to /v2/asset_files, then POST /v2/notebooks.
    /// The materializer only serialises declared fields, so the nested notebook body
    /// (project / file_reference / runtime.environment) is built here and posted directly,
    /// returning Handled so the engine skips its default POST.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] notebook requires name"))?.to_string();
            let project_id = resource.get("project_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] notebook requires project_id"))?.to_string();
            let source_path = resource.get("source_path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] notebook requires source_path"))?.to_string();

            // 1. Upload the .ipynb bytes to the asset_files store as multipart/form-data
            //    with a binary `file` part — the shape the OpenAPI declares and the only
            //    one the gateway accepts (live-pinned 2026-07-06 on CP4D: a raw
            //    octet-stream PUT 400s "Invalid content type"; the multipart PUT to the
            //    same route returns 201). Mirrors script_asset's Software-gateway upload
            //    branch (apply_auth_scheme, not hardcoded Bearer — zenapikey under SAML).
            let file_path = format!("notebook/{name}.ipynb");
            let content = std::fs::read(&source_path).map_err(|e| anyhow!("[{operation_id}] failed to read '{source_path}': {e}"))?;
            let bytes_len = content.len();
            let upload_url = format!("{}/v2/asset_files/{}?project_id={}", client.base_url().trim_end_matches('/'), file_path, project_id);
            let token = client.get_token().await?;
            let part = reqwest::multipart::Part::bytes(content).file_name(format!("{name}.ipynb")).mime_str("application/octet-stream")?;
            let form = reqwest::multipart::Form::new().part("file", part);
            let put_resp = client.apply_auth_scheme(client.raw_client().put(&upload_url), &token)?.multipart(form).send().await.map_err(|e| anyhow!("[{operation_id}] notebook file upload failed: {e}"))?;
            if !put_resp.status().is_success() {
                let status = put_resp.status();
                let body = put_resp.text().await.unwrap_or_default();
                return Err(anyhow!("[{operation_id}] notebook file upload failed ({status}) for '{file_path}': {body}"));
            }

            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "notebook",
                name = %name,
                bytes = bytes_len,
                "uploaded notebook file; registering notebook"
            );

            // 2. Register the notebook.
            let mut runtime = json!({});
            if let Some(env) = resource.get("environment").and_then(|v| v.as_str()) {
                runtime = json!({"environment": env});
            }
            // `project` is a bare uuid string — the object shape 400s with
            // invalid_type "The 'project' field needs to be a string" / "needs to be
            // a uuid v4" (live-pinned 2026-07-06 on CP4D; string shape returns 201).
            let create_body = json!({
                "name": name,
                "project": project_id,
                "file_reference": file_path,
                "runtime": runtime
            });
            let spec = with_project(RequestSpec::new(Method::POST, "/v2/notebooks").body(BodyKind::Json(create_body)), resource);
            let mut resp: Value = client.execute(operation_id, spec).await.map_err(|e| anyhow!("[{operation_id}] notebook create POST failed: {e}"))?;

            // Fold the guid top-level so ${notebook.x.guid} resolves on the create path.
            let notebook_id = resp.pointer("/metadata/guid").or_else(|| resp.pointer("/metadata/asset_id")).and_then(|v| v.as_str()).map(|s| s.to_string());
            if let Some(id) = &notebook_id
                && let Some(obj) = resp.as_object_mut()
            {
                obj.insert("guid".to_string(), json!(id));
            }

            // 3. Create a notebook version (checkpoint). An API-registered notebook has
            //    ZERO versions, and the Jobs service refuses to execute it — run submit
            //    500s "Notebooks API returned no checkpoints" (live-pinned 2026-07-06 on
            //    CP4D). POST an empty-body version (201) and the identical run submit
            //    succeeds; the UI creates this checkpoint implicitly on save, the API
            //    does not.
            if let Some(id) = &notebook_id {
                let version_spec = with_project(RequestSpec::new(Method::POST, format!("/v2/notebooks/{id}/versions")).body(BodyKind::Json(json!({}))), resource);
                let version: Value = client.execute(operation_id, version_spec).await.map_err(|e| anyhow!("[{operation_id}] notebook version create failed (notebook={id}) — jobs cannot execute a version-less notebook: {e}"))?;
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "notebook", notebook_id = %id, version = %version.pointer("/metadata/guid").and_then(|v| v.as_str()).unwrap_or("?"), "created notebook version (checkpoint) so the job runtime can execute it");
            }

            Ok(HookOutcome::Handled(resp))
        })
    }

    /// Expose the guid top-level on the discovery (re-plan / re-apply) path so downstream
    /// refs resolve — mirrors the data_asset asset_id hoist.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(id) = remote_data.pointer("/metadata/guid").or_else(|| remote_data.pointer("/metadata/asset_id")).and_then(|v| v.as_str()).map(|s| s.to_string())
                && let Some(obj) = remote_data.as_object_mut()
            {
                obj.insert("guid".to_string(), json!(id));
            }
            Ok(())
        })
    }
}
