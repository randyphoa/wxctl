//! `openscale/guardrails_policy` handler — manages watsonx.governance Guardrails
//! Manager policies against the OpenScale ROOT host (NOT the `/openscale/{guid}`
//! service base): `{root}/guardrails-manager/v1/policies`, with the
//! `x-governance-instance-id: {guid}` header and an `inventory_id` query param.
//!
//! The openscale service `url` bakes in `/openscale/{guid}`, which `join_url`
//! always prepends, so `client.execute()` can't reach the root host. This handler
//! derives `(root, guid)` from `client.base_url()` and issues absolute requests via
//! `client.raw_client()` + `client.get_token()` (the cos_discovery / wml_model
//! precedent). The schema sets `discovery: skip`, so reconciliation plans Create on
//! apply and an optimistic Delete on destroy, routing both here (model_tracking
//! precedent). The create body is non-flat (object-array detectors + host swap), so
//! the POST is issued inside `pre_create` returning `HookOutcome::Handled` —
//! mutating `resource` + `Continue` would drop the reshaped body (see
//! docs/troubleshoot/pre-create-body-reshape-dropped-fix.md).
//!
//! Inventories are aigov inventories on `COMMON_CORE_URL`
//! (`https://api.dataplatform.cloud.ibm.com`), NOT the OpenScale host —
//! live-proven: posting to the OpenScale host returns 405. Creating one requires
//! a Cross Region US COS bucket (region `us`, endpoint
//! `https://s3.us.cloud-object-storage.appdomain.cloud`) followed by
//! `POST {COMMON_CORE_URL}/v1/aigov/inventories` with the bucket + HMAC creds.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::cloud_object_storage::aigov_bucket::{aigov_host, cos_config_from_env, cos_instance_guid, ensure_cos_bucket};

/// Fixed name of the wxctl-managed inventory auto-ensured for guardrails policies
/// (OQ3: one recognizable inventory, reused across applies; shared infrastructure,
/// not torn down with a policy).
const AIGOV_INVENTORY_NAME: &str = "wxctl-guardrails";

pub struct GuardrailsPolicyHandler;

impl ResourceHandler for GuardrailsPolicyHandler {
    /// Discover-or-create the policy. Lists existing policies (root host, header,
    /// inventory + `policytype=draft`/`policytype=publish` union) and adopts a name
    /// match idempotently; otherwise auto-ensures the COS-backed aigov inventory
    /// and POSTs the policy. Returns `Handled` so the engine skips the default
    /// (wrong-host) POST.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let (root, guid) = guardrails_base(client.base_url())?;
            let inventory_id = ensure_inventory(client, operation_id).await?;
            let name = require_str(resource, "name")?.to_string();

            if let Some(existing) = find_policy_by_name(client, &root, &guid, &inventory_id, &name).await? {
                let desired = build_create_body(resource);
                if policy_needs_update(&desired, &existing) {
                    let id = policy_id(&existing).ok_or_else(|| anyhow!("[{}] guardrails: matched policy '{name}' has no id", error_codes::H901))?;
                    let token = client.get_token().await.context("guardrails: failed to get token for policy update")?;
                    let url = format!("{root}/guardrails-manager/v1/policies/{id}");
                    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, id = %id, "guardrails policy changed — updating (PUT, full replace)");
                    // Root-host workaround requires raw requests; apply_auth_scheme keeps
                    // zenapikey/CP4D auth working instead of a hardcoded Bearer.
                    let req = client.raw_client().put(&url).query(&[("inventory_id", inventory_id.as_str())]).header("x-governance-instance-id", &guid).json(&desired);
                    let resp = client.apply_auth_scheme(req, &token)?.send().await.context("guardrails: policy update request failed")?;
                    let updated = read_json(resp, "policy update").await?;
                    return Ok(HookOutcome::Handled(updated));
                }
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, "guardrails policy unchanged — adopting (no write)");
                return Ok(HookOutcome::Handled(existing));
            }

            let body = build_create_body(resource);
            let token = client.get_token().await.context("guardrails: failed to get token for policy create")?;
            let url = format!("{root}/guardrails-manager/v1/policies");
            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, inventory_id = %inventory_id, "creating guardrails policy");
            let req = client.raw_client().post(&url).query(&[("inventory_id", inventory_id.as_str())]).header("x-governance-instance-id", &guid).json(&body);
            let resp = client.apply_auth_scheme(req, &token)?.send().await.context("guardrails: policy create request failed")?;
            let created = read_json(resp, "policy create").await?;
            Ok(HookOutcome::Handled(created))
        })
    }

    /// Delete the policy by name. Resolves the inventory + lists to find the id,
    /// DELETEs (tolerating 404 for idempotent teardown), and always returns
    /// `Handled` so the engine skips the default (wrong-host) DELETE. A truly-absent
    /// policy/inventory is a no-op.
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let (root, guid) = guardrails_base(client.base_url())?;
            let name = require_str(resource, "name")?.to_string();

            let Some(inventory_id) = find_inventory_id(client).await? else {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, "guardrails: no wxctl inventory — nothing to delete");
                return Ok(HookOutcome::Handled(json!({ "name": name, "deleted": false })));
            };
            let Some(existing) = find_policy_by_name(client, &root, &guid, &inventory_id, &name).await? else {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, "guardrails policy already absent — idempotent delete");
                return Ok(HookOutcome::Handled(json!({ "name": name, "deleted": false })));
            };
            let id = policy_id(&existing).ok_or_else(|| anyhow!("[{}] guardrails: matched policy '{name}' has no id", error_codes::H901))?;

            let token = client.get_token().await.context("guardrails: failed to get token for policy delete")?;
            let url = format!("{root}/guardrails-manager/v1/policies/{id}");
            let req = client.raw_client().delete(&url).query(&[("inventory_id", inventory_id.as_str())]).header("x-governance-instance-id", &guid);
            let resp = client.apply_auth_scheme(req, &token)?.send().await.context("guardrails: policy delete request failed")?;
            let status = resp.status();
            if !status.is_success() && status.as_u16() != 404 {
                let body = resp.text().await.unwrap_or_default();
                bail!("[{}] guardrails: policy delete returned {status}: {body}", error_codes::H901);
            }
            Ok(HookOutcome::Handled(json!({ "name": name, "id": id, "deleted": true })))
        })
    }
}

/// Split an OpenScale service base URL `https://{host}/openscale/{guid}` into the
/// root host and the instance guid. Errors clearly if the URL lacks the
/// `/openscale/{guid}` segment.
fn guardrails_base(openscale_url: &str) -> Result<(String, String)> {
    const MARKER: &str = "/openscale/";
    let idx = openscale_url.find(MARKER).ok_or_else(|| anyhow!("[{}] guardrails: OPENSCALE_URL '{openscale_url}' is missing the expected '/openscale/{{guid}}' segment", error_codes::H901))?;
    let root = openscale_url[..idx].trim_end_matches('/').to_string();
    let guid = openscale_url[idx + MARKER.len()..].trim_matches('/').split('/').next().unwrap_or_default().to_string();
    if root.is_empty() || guid.is_empty() {
        bail!("[{}] guardrails: could not derive (root, guid) from OPENSCALE_URL '{openscale_url}'", error_codes::H901);
    }
    Ok((root, guid))
}

/// GET `{COMMON_CORE_URL}/v1/aigov/inventories`; return the guid of the
/// wxctl-managed inventory (matched by `entity.name`), if present.
async fn find_inventory_id(client: &HttpClient) -> Result<Option<String>> {
    let host = aigov_host()?;
    let token = client.get_token().await.context("guardrails: failed to get token for inventory list")?;
    let url = format!("{host}/v1/aigov/inventories");
    // Hardcoded Bearer is correct: aigov_host() is the IBM-Cloud-public platform host
    // (COMMON_CORE_URL), not the profile's service deployment.
    let resp = client.raw_client().get(&url).bearer_auth(&token).send().await.context("guardrails: inventory list request failed")?;
    let body = read_json(resp, "inventory list").await?;
    let found = body.get("catalogs").and_then(|v| v.as_array()).into_iter().flatten().find(|c| c.get("entity").and_then(|e| e.get("name")).and_then(|v| v.as_str()) == Some(AIGOV_INVENTORY_NAME)).and_then(|c| c.get("metadata").and_then(|m| m.get("guid")).and_then(|v| v.as_str()).map(str::to_string));
    Ok(found)
}

/// Resolve the wxctl-managed inventory guid, creating it (COS bucket + aigov
/// inventory) if absent. Idempotent: discover-by-name first; never creates a
/// second. On SaaS the inventory must be backed by a Cross Region US COS bucket.
async fn ensure_inventory(client: &HttpClient, operation_id: &str) -> Result<String> {
    if let Some(id) = find_inventory_id(client).await? {
        return Ok(id);
    }
    let host = aigov_host()?;
    let cos = cos_config_from_env()?;
    let bucket = format!("wxctl-guardrails-{}", cos_instance_guid(&cos.crn));
    ensure_cos_bucket(client, &cos, &bucket, operation_id).await?;

    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %AIGOV_INVENTORY_NAME, bucket = %bucket, "guardrails: creating COS-backed aigov inventory");
    let token = client.get_token().await.context("guardrails: failed to get token for inventory create")?;
    let url = format!("{host}/v1/aigov/inventories");
    let body = json!({
        "name": AIGOV_INVENTORY_NAME,
        "description": "wxctl-managed inventory for guardrails policies",
        "generator": "wxctl",
        "is_governed": true,
        "bucket": {
            "bucket_name": bucket,
            "bucket_type": "bmcos_object_storage",
            "resource_instance_id": cos.crn,
            "endpoint_url": cos.endpoint,
            "credentials_rw": { "access_key_id": cos.access_key, "secret_access_key": cos.secret_key }
        }
    });
    // Hardcoded Bearer is correct: IBM-Cloud-public platform host (see find_inventory_id).
    let resp = client.raw_client().post(&url).bearer_auth(&token).json(&body).send().await.context("guardrails: inventory create request failed")?;
    let created = read_json(resp, "inventory create").await?;
    created.get("metadata").and_then(|m| m.get("guid")).and_then(|v| v.as_str()).map(str::to_string).ok_or_else(|| anyhow!("[{}] guardrails: inventory create response has no metadata.guid", error_codes::H901))
}

/// List policies (draft + publish union) and, on a name match, GET the full policy
/// by id — the list endpoint returns only a summary (no `action`/`block_message`/
/// `mask_character`), so the full policy is needed for change detection and adoption.
async fn find_policy_by_name(client: &HttpClient, root: &str, guid: &str, inventory_id: &str, name: &str) -> Result<Option<Value>> {
    let token = client.get_token().await.context("guardrails: failed to get token for policy list")?;
    let list_url = format!("{root}/guardrails-manager/v1/policies");
    // policytype=false is broken (returns empty even when drafts exist); query draft + publish and union.
    for policytype in ["draft", "publish"] {
        let req = client.raw_client().get(&list_url).query(&[("inventory_id", inventory_id), ("policytype", policytype)]).header("x-governance-instance-id", guid);
        let resp = client.apply_auth_scheme(req, &token)?.send().await.context("guardrails: policy list request failed")?;
        let body = read_json(resp, "policy list").await?;
        if let Some(item) = body.get("policies").and_then(|v| v.as_array()).into_iter().flatten().find(|p| policy_name(p) == Some(name))
            && let Some(id) = policy_id(item)
        {
            let get_url = format!("{root}/guardrails-manager/v1/policies/{id}");
            let req = client.raw_client().get(&get_url).query(&[("inventory_id", inventory_id)]).header("x-governance-instance-id", guid);
            let resp = client.apply_auth_scheme(req, &token)?.send().await.context("guardrails: policy get-by-id request failed")?;
            return Ok(Some(read_json(resp, "policy get").await?));
        }
    }
    Ok(None)
}

/// Build the policy create body (PolicyResponsePrototype) from the resource:
/// required `name`/`input`/`output`/`policy_status`, optional `description`/
/// `block_message`/`mask_character`/`tags`. `input`/`output` pass through as-is
/// (object arrays); absent/null keys are dropped.
fn build_create_body(resource: &Value) -> Value {
    let mut body = serde_json::Map::new();
    for key in ["name", "input", "output", "policy_status", "description", "block_message", "mask_character", "tags"] {
        if let Some(v) = resource.get(key).filter(|v| !v.is_null()) {
            body.insert(key.to_string(), v.clone());
        }
    }
    Value::Object(body)
}

/// Decide whether the existing remote policy must be PUT-updated to match the
/// desired body. Compares only the fields that reliably round-trip in the full
/// (GET-by-id) policy — `name` is the match key, and `policy_status` is normalized
/// server-side to `status.state` so it never echoes back and is excluded (a pure
/// draft↔publish status flip is therefore not detected as a change). `remote` MUST
/// be the full policy (see `find_policy_by_name`), not a list summary.
fn policy_needs_update(desired: &Value, remote: &Value) -> bool {
    ["input", "output", "block_message", "mask_character", "description", "tags"].iter().any(|k| desired.get(*k) != remote_field(remote, k))
}

/// Read a writable field from a remote policy: top-level first, then `entity.{k}`.
fn remote_field<'a>(remote: &'a Value, key: &str) -> Option<&'a Value> {
    remote.get(key).or_else(|| remote.get("entity").and_then(|e| e.get(key)))
}

/// The policy's `name`, top-level or under the `entity` envelope.
fn policy_name(p: &Value) -> Option<&str> {
    p.get("name").and_then(|v| v.as_str()).or_else(|| p.get("entity").and_then(|e| e.get("name")).and_then(|v| v.as_str()))
}

/// The policy's id, read from `metadata.id` (also `entity.id` / top-level `id`).
fn policy_id(p: &Value) -> Option<String> {
    p.get("metadata").and_then(|m| m.get("id")).and_then(|v| v.as_str()).map(str::to_string).or_else(|| p.get("entity").and_then(|e| e.get("id")).and_then(|v| v.as_str()).map(str::to_string)).or_else(|| p.get("id").and_then(|v| v.as_str()).map(str::to_string))
}

/// Parse a successful JSON response; turn any non-2xx into an error carrying the
/// status + body (H901). Callers that must tolerate a status (delete's 404) check
/// the status themselves before calling this.
async fn read_json(resp: reqwest::Response, what: &str) -> Result<Value> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("[{}] guardrails: {what} returned {status}: {body}", error_codes::H901);
    }
    resp.json::<Value>().await.with_context(|| format!("guardrails: failed to parse {what} response"))
}

fn require_str<'a>(resource: &'a Value, field: &str) -> Result<&'a str> {
    resource.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{}] guardrails_policy missing required field '{field}'", error_codes::H901))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guardrails_base_splits_root_and_guid() {
        let (root, guid) = guardrails_base("https://api.aiopenscale.cloud.ibm.com/openscale/abc-123").unwrap();
        assert_eq!(root, "https://api.aiopenscale.cloud.ibm.com");
        assert_eq!(guid, "abc-123");
        // Trailing slash and trailing path segments after the guid are tolerated.
        let (root, guid) = guardrails_base("https://h.example/openscale/g-9/").unwrap();
        assert_eq!(root, "https://h.example");
        assert_eq!(guid, "g-9");
        // Missing /openscale/{guid} segment → error.
        assert!(guardrails_base("https://h.example/v2/data_marts").is_err());
    }

    #[test]
    fn policy_id_and_name_read_envelopes() {
        let p = json!({"metadata": {"id": "p-1"}, "entity": {"name": "content-safety"}});
        assert_eq!(policy_id(&p).as_deref(), Some("p-1"));
        assert_eq!(policy_name(&p), Some("content-safety"));
        let flat = json!({"id": "p-2", "name": "flat"});
        assert_eq!(policy_id(&flat).as_deref(), Some("p-2"));
        assert_eq!(policy_name(&flat), Some("flat"));
    }

    #[test]
    fn policy_needs_update_detects_diffs() {
        let desired = json!({"name": "p", "policy_status": "draft", "block_message": "blocked", "description": "d", "input": [{"detector": "pii", "action": "mask"}], "output": []});
        // identical curated values (full policy, extra envelope keys, object-key order differs) → no update.
        let remote_same = json!({"metadata": {"id": "x"}, "name": "p", "block_message": "blocked", "description": "d", "input": [{"action": "mask", "detector": "pii"}], "output": [], "status": {"state": "active"}});
        assert!(!policy_needs_update(&desired, &remote_same));
        // same curated values under the entity envelope → no update.
        let remote_entity = json!({"metadata": {"id": "x"}, "entity": {"name": "p", "block_message": "blocked", "description": "d", "input": [{"detector": "pii", "action": "mask"}], "output": []}});
        assert!(!policy_needs_update(&desired, &remote_entity));
        // only policy_status differs (excluded) → still no update.
        let remote_status = json!({"name": "p", "block_message": "blocked", "description": "d", "input": [{"detector": "pii", "action": "mask"}], "output": [], "status": {"state": "active"}});
        assert!(!policy_needs_update(&desired, &remote_status));
        // a changed curated scalar (block_message) → update.
        let remote_changed = json!({"name": "p", "block_message": "DIFFERENT", "description": "d", "input": [{"detector": "pii", "action": "mask"}], "output": []});
        assert!(policy_needs_update(&desired, &remote_changed));
        // a changed detector action → update.
        let remote_action = json!({"name": "p", "block_message": "blocked", "description": "d", "input": [{"detector": "pii", "action": "block"}], "output": []});
        assert!(policy_needs_update(&desired, &remote_action));
        // a missing curated field (description) → update.
        let remote_missing = json!({"name": "p", "block_message": "blocked", "input": [{"detector": "pii", "action": "mask"}], "output": []});
        assert!(policy_needs_update(&desired, &remote_missing));
    }

    #[test]
    fn build_create_body_keeps_present_drops_absent() {
        let resource = json!({
            "name": "content-safety", "policy_status": "draft",
            "input": [{"detector": "pii", "action": "mask"}], "output": [],
            "tags": ["g"], "ref_name": "ignored", "mask_character": null
        });
        let body = build_create_body(&resource);
        assert_eq!(body.get("name").and_then(|v| v.as_str()), Some("content-safety"));
        assert!(body.get("input").and_then(|v| v.as_array()).is_some());
        assert!(body.get("tags").is_some());
        assert!(body.get("mask_character").is_none(), "null is dropped");
        assert!(body.get("ref_name").is_none(), "undeclared key not forwarded");
    }
}
