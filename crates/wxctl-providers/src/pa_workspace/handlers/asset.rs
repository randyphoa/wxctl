//! `paw_*` shared handler. All six PAW content kinds share one `/pacontent/v1/Assets` CRUD
//! contract (see the module doc in `pa_workspace/mod.rs`). This handler OWNS create/update/delete
//! (all `HookOutcome::Handled`) because the composite-key URL and the opaque, file-loaded
//! `content` document are inexpressible via the default materializer.
//!
//! Each `paw_*` kind binds its own `/pacontent/v1` OData asset `type` as a **constructor arg** at
//! registration (`define_handlers!` in `pa_workspace/mod.rs`), not by sniffing a `kind` field off
//! the resource at hook time: the engine passes only the materialized schema fields into hooks
//! (`wxctl-engine/src/execution/operations/create.rs` clones `resolved_data`, which carries no
//! `kind`), so a runtime `kind` lookup fails on every write. Constructor injection also makes the
//! kind -> type mapping total: an unrecognized `paw_*` kind can no longer reach this handler at
//! all, since `define_handlers!` only binds the six known kinds.
//!
//! It loads `content` from its file in `post_validate`, and hydrates a discovered asset's
//! `content` in `post_discover`. The gateway needs no `User-Agent` (reqwest sends none;
//! `pa-live-gateway-quirks.md` #2) and auths on the `paSession` cookie alone.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

/// Shared handler for all six `paw_*` kinds. Each registration binds its own OData asset `type`
/// (see the module doc for why this is a constructor arg rather than a runtime `kind` sniff).
pub struct AssetHandler(&'static str);

impl AssetHandler {
    pub const fn new(asset_type: &'static str) -> Self {
        Self(asset_type)
    }
}

impl ResourceHandler for AssetHandler {
    fn post_validate<'a>(&'a self, resource: &'a mut Value, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { load_content_file(resource) })
    }

    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // POST /Assets needs the parent's PATH; the id model carries only the parent's id, so
            // resolve it (live-proven: id -> `.path` = "/shared" or "/shared/wxctl-demo"). One extra GET.
            let folder_id = resource.get("folder").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("paw create requires 'folder' (the parent folder id)"))?;
            let parent: Value = client.get(operation_id, &format!("/Assets(id='{folder_id}',type='folder')")).await?;
            let parent_path = parent.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("paw create: parent folder '{folder_id}' returned no 'path'"))?.to_string();

            let body = build_create_body(resource, self.0, &parent_path)?;
            let spec = RequestSpec::new(Method::POST, endpoint).body(BodyKind::Json(body));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // Update is content-ONLY (any other attribute -> 400). id comes from the discovered
            // remote (`current`); type is bound at registration.
            let id = current.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("paw update requires a discovered 'id'"))?;
            let ty = self.0;
            let content = desired.get("content").cloned().ok_or_else(|| anyhow!("paw update requires 'content'"))?;
            let path = format!("/Assets(id='{id}',type='{ty}')");
            let spec = RequestSpec::new(Method::PUT, &path).body(BodyKind::Json(json!({ "content": content })));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }

    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // pre_delete runs before default id extraction and may key on a non-id path variable
            // (delete.rs). Build the composite key from the discovered id + the bound type.
            let id = resource.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("paw delete requires a discovered 'id'"))?;
            let ty = self.0;
            let path = format!("/Assets(id='{id}',type='{ty}')");
            let spec = RequestSpec::new(Method::DELETE, &path).body(BodyKind::None);
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // The folder listing carries id/type/name/path but not `content`; fetch it so
            // `state_fields: [name, content]` compares (precedent: CubeHandler dimension hoist).
            // The handler is bound to a single type by construction, so the discovered item is of
            // that type; only `id` needs to come from the remote data.
            let Some(id) = remote_data.get("id").and_then(|v| v.as_str()).map(str::to_string) else {
                return Ok(());
            };
            let ty = self.0;
            let endpoint = format!("/Assets(id='{id}',type='{ty}')?$expand=content");
            match client.get::<Value>(operation_id, &endpoint).await {
                Ok(response) => {
                    if let Some(content) = response.get("content")
                        && let Some(obj) = remote_data.as_object_mut()
                    {
                        obj.insert("content".to_string(), content.clone());
                    }
                }
                Err(e) => tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, asset_id = %id, error = %e, "failed to fetch paw asset content; content drift comparison may show a phantom update"),
            }
            Ok(())
        })
    }
}

/// Parse the `content` file (path already absolutized by `resolve_file_paths`) into a JSON object
/// in place. Absent content (a paw_folder) or an already-parsed object is left unchanged. A
/// missing or unparseable file is a validation error before any network call (spec Error Handling).
fn load_content_file(resource: &mut Value) -> Result<()> {
    let Some(path) = resource.get("content").and_then(|v| v.as_str()).map(str::to_string) else {
        return Ok(());
    };
    let bytes = std::fs::read(&path).map_err(|e| anyhow!("paw `content` file '{path}' is unreadable: {e}"))?;
    let doc: Value = serde_json::from_slice(&bytes).map_err(|e| anyhow!("paw `content` file '{path}' is not valid JSON: {e}"))?;
    if let Some(obj) = resource.as_object_mut() {
        obj.insert("content".to_string(), doc);
    }
    Ok(())
}

/// Build the create `POST /Assets` body: `{name, path, type, content?, description?}`. `path` is
/// the resolved parent's path VERBATIM (not `parent_path + "/" + name` — live-proven: POST
/// `{name:"wxctlNestBook", path:"/shared/wxctlNestProbe"}` creates the book INSIDE
/// `wxctlNestProbe`). `content` is included only when already parsed to a document
/// (post_validate); a content-less kind (folder) omits it. `asset_type` is the OData type bound to
/// the handler at registration.
fn build_create_body(resource: &Value, asset_type: &'static str, parent_path: &str) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("paw create requires 'name'"))?;
    let mut body = Map::new();
    body.insert("name".to_string(), json!(name));
    body.insert("path".to_string(), json!(parent_path));
    body.insert("type".to_string(), json!(asset_type));
    if let Some(content) = resource.get("content").filter(|v| v.is_object() || v.is_array()) {
        body.insert("content".to_string(), content.clone());
    }
    if let Some(desc) = resource.get("description").and_then(|v| v.as_str()) {
        body.insert("description".to_string(), json!(desc));
    }
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_binds_the_asset_type() {
        assert_eq!(AssetHandler::new("tm1view").0, "tm1view");
    }

    #[test]
    fn create_body_uses_parent_path_verbatim_and_carries_content_and_description() {
        let resource = json!({"name": "Sales", "content": {"widgets": []}, "description": "demo"});
        let body = build_create_body(&resource, "dashboard", "/shared/wxctl-demo").expect("body");
        assert_eq!(body.get("name").and_then(|v| v.as_str()), Some("Sales"));
        assert_eq!(body.get("path").and_then(|v| v.as_str()), Some("/shared/wxctl-demo"));
        assert_eq!(body.get("type").and_then(|v| v.as_str()), Some("dashboard"));
        assert!(body.get("content").and_then(|v| v.as_object()).is_some());
        assert_eq!(body.get("description").and_then(|v| v.as_str()), Some("demo"));
    }

    #[test]
    fn create_body_omits_content_for_content_less_folder() {
        let resource = json!({"name": "wxctl-demo"});
        let body = build_create_body(&resource, "folder", "/shared").expect("body");
        assert_eq!(body.get("type").and_then(|v| v.as_str()), Some("folder"));
        assert!(!body.as_object().unwrap().contains_key("content"));
    }
}
