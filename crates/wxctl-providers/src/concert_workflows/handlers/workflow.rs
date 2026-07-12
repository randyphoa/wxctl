//! `concert_workflow` handler — multipart zip import + computed flow_uri hoist.
//!
//! Pliant has no JSON create for flows: a flow is IMPORTED from a zip via multipart
//! POST {path_prefix}/v1/flows/{userName}/import?folder=/{userName}{folder}&overwrite=…
//! (precedent: watsonx_data sal_glossary). The folder QUERY PARAM is interpreted (with any zip
//! entry paths) as /{user}/{folders…}/{name} — the FIRST segment must be an existing username or
//! every per-file import fails with code 424, so the handler always addresses it as
//! "/{user}{folder}" (folder is already slash-wrapped, e.g. "/wxctl/" → "/admin/wxctl/"); import
//! auto-creates any missing folder beyond that. `overwrite` is an `OverwriteOptions` string enum
//! (SKIP | ERROR | OVERWRITE | COPY, default SKIP) — NOT a boolean; sending true/false
//! live-fails with HTTP 400 (type-conversion error). `request_multipart` builds base_url+path
//! and does NOT apply the client path_prefix, so we prepend it. pre_create imports with
//! overwrite=ERROR (fail if the flow already exists); pre_update re-imports with
//! overwrite=OVERWRITE (replace it). Both own the write (HookOutcome::Handled) — the default
//! JSON POST/PUT path is never reached. Per-file import status codes are HTTP-style, not the
//! legacy 0=ok convention: 200/201 = success (overwritten/created), 409 = already exists (ERROR
//! mode), 424 = target user unknown or the addressing above is wrong.
//!
//! The API has no surrogate id: a flow is keyed by (user_name, folder, name) and its server
//! content `hash` is the drift signal. Downstream kinds (concert_workflow_exposure.flow_uri,
//! concert_workflow_schedule.flow_urn) reference a computed flow_uri, composed by straight
//! concatenation of the natural key: "{user}{folder}{name}" (folder already slash-wrapped, so no
//! separator insertion is needed). Because a Handled pre_create returns BEFORE post_create runs
//! (create.rs), the create-side hoist is embedded in the returned object (config shape:
//! user_name/folder/name); the discovery-side hoist runs in post_discover over the folder-list
//! item (server shape: username/path/name, where `path` = folder+name with no user prefix, e.g.
//! "/wxctl/wxctl-hello"). `hoist_flow_uri` composes from whichever shape is complete and is a
//! no-op otherwise — best-effort only, since apply-time refs resolve from the pre_create Handled
//! response, which always carries the complete config shape.

use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct WorkflowHandler;

/// Insert a computed flow_uri if absent (no-clobber), composed by straight concatenation of
/// whichever of the two real key shapes is complete:
/// - Config shape: user_name + folder (already slash-wrapped, e.g. "/wxctl/") + name →
///   "{user_name}{folder}{name}".
/// - Server (discovered folder-list item) shape: username + path (folder+name, no user prefix,
///   e.g. "/wxctl/wxctl-hello") → "{username}{path}".
///
/// Best-effort only: if neither shape is complete (e.g. a discovered flow GET body carrying
/// `path` but no `username`), this is a no-op — nothing fabricated. Apply-time refs always
/// resolve from the pre_create Handled response, which always carries the complete config shape.
fn hoist_flow_uri(value: &mut Value) {
    if value.get("flow_uri").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
        return;
    }
    let user_name = value.get("user_name").and_then(|v| v.as_str()).map(str::to_string);
    let folder = value.get("folder").and_then(|v| v.as_str()).map(str::to_string);
    let name = value.get("name").and_then(|v| v.as_str()).map(str::to_string);
    let username = value.get("username").and_then(|v| v.as_str()).map(str::to_string);
    let path = value.get("path").and_then(|v| v.as_str()).map(str::to_string);

    let uri = match (user_name, folder, name) {
        (Some(user_name), Some(folder), Some(name)) => Some(format!("{user_name}{folder}{name}")),
        _ => match (username, path) {
            (Some(username), Some(path)) => Some(format!("{username}{path}")),
            _ => None,
        },
    };

    if let (Some(uri), Some(obj)) = (uri, value.as_object_mut()) {
        obj.insert("flow_uri".to_string(), Value::String(uri));
    }
}

/// Percent-encode a value for safe embedding in a URL path segment or query value
/// (`&`, `#`, `?`, spaces, unicode). `form_urlencoded` emits `+` for space, which is
/// only valid in query strings — normalize to `%20` so path segments stay correct too.
fn urlencode_component(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect::<String>().replace('+', "%20")
}

/// Multipart-import the flow zip and return a synthesized flow object carrying the natural key +
/// computed flow_uri. `overwrite` is the Pliant `OverwriteOptions` enum string (SKIP | ERROR |
/// OVERWRITE | COPY) — create passes "ERROR" (fail if the flow already exists), update passes
/// "OVERWRITE" (replace it). Per-file `FlowImportStatusDto.code` is HTTP-style, not the legacy
/// 0=ok convention: any code outside 200..=299 fails the op (spec Error Handling).
async fn import_flow(resource: &Value, client: &HttpClient, operation_id: &str, overwrite: &str) -> Result<Value> {
    let user = resource.get("user_name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_workflow requires user_name"))?;
    let folder = resource.get("folder").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_workflow requires folder"))?;
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_workflow requires name"))?;
    let src = resource.get("source_path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_workflow requires source_path"))?;
    let path = Path::new(src);
    if !path.exists() {
        bail!("flow zip not found: {src} (path should be relative to the config file or absolute)");
    }
    if !folder.starts_with('/') || !folder.ends_with('/') {
        bail!("concert_workflow folder must be a slash-wrapped path like /wxctl/ (got {folder})");
    }

    // request_multipart = base_url + path (no path_prefix) → prepend it. The folder query param
    // is addressed as /{user}{folder} (folder already slash-wrapped): the import endpoint
    // interprets the combined folder value (plus any zip entry paths) as /{user}/{folders…}/{name},
    // and the FIRST segment must be an existing username or every per-file status comes back 424 —
    // so we always place the flow under the importing user's own namespace. Import auto-creates
    // any folder segment beyond that.
    // Percent-encode the user-controlled values: an `&` or `#` in a folder/user would
    // otherwise truncate the query string.
    let endpoint = format!("{}/v1/flows/{}/import?folder={}&overwrite={}", client.path_prefix(), urlencode_component(user), urlencode_component(&format!("/{user}{folder}")), overwrite);
    let form: HashMap<String, Value> = HashMap::new();

    tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "concert_workflow", flow = %name, folder = %folder, overwrite = %overwrite, "importing Pliant flow zip (multipart)");

    let statuses: Value = client.request_multipart(operation_id, Method::POST, &endpoint, form, vec![path], "file").await?;

    if let Some(arr) = statuses.as_array() {
        // Per-file status codes are HTTP-style: 200/201 = overwritten/created (success), 409 =
        // already exists (ERROR mode), 424 = target user unknown or the addressing above is wrong.
        // Anything outside 2xx is a failure.
        let failed: Vec<String> = arr
            .iter()
            .filter_map(|s| {
                let code = s.get("code").and_then(|c| c.as_i64()).unwrap_or(0);
                if (200..=299).contains(&code) {
                    return None;
                }
                let hint = match code {
                    409 => " (409 = flow already exists under ERROR mode)",
                    424 => " (424 = target user missing, or the folder/user addressing is wrong)",
                    _ => "",
                };
                Some(format!("{s}{hint}"))
            })
            .collect();
        if !failed.is_empty() {
            bail!("flow import failed for {name}: {}", failed.join(", "));
        }
    } else {
        // A non-array body (e.g. `{"error": ...}` with HTTP 200) must not pass as a
        // green import with zero validated files.
        anyhow::bail!("unexpected flow-import response shape: {statuses}");
    }

    let mut created = json!({ "name": name, "user_name": user, "folder": folder });
    hoist_flow_uri(&mut created);
    Ok(created)
}

impl ResourceHandler for WorkflowHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let created = import_flow(resource, client, operation_id, "ERROR").await?;
            Ok(HookOutcome::Handled(created))
        })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let updated = import_flow(desired, client, operation_id, "OVERWRITE").await?;
            Ok(HookOutcome::Handled(updated))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Discovered folder-list item (ItemDto) carries username/path/name → compute flow_uri
            // so a pre-existing flow's ${concert_workflow.<ref>.flow_uri} resolves on re-apply/replan.
            hoist_flow_uri(remote_data);
            Ok(())
        })
    }
}
