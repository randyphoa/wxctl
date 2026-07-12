//! `factsheets/inventory` handler — creates an AI Factsheets model inventory
//! (governance catalog) on BOTH deployments.
//!
//! - **Software**: the inventory is backed by the platform-managed `assetfiles`
//!   store. No custom logic is needed — `pre_create` / `pre_delete` return
//!   `Continue` and the schema-driven default POST / DELETE
//!   (`?delete_bucket=true`) reconcile it.
//! - **SaaS**: `assetfiles` is CPD/Software-only and is rejected
//!   (`BUCSV3017E`). The inventory must instead be backed by a real IBM Cloud
//!   Object Storage bucket (`bmcos_object_storage` + `credentials_rw`). This
//!   handler reuses the live-verified COS-backed path shared with the
//!   `openscale/guardrails_policy` handler
//!   (`crate::cloud_object_storage::aigov_bucket`): ensure a Cross Region US COS
//!   bucket, then `POST {COMMON_CORE_URL}/v1/aigov/inventories` with the bucket
//!   + HMAC creds. See `docs/troubleshoot/guardrails-policy-saas-inventory-fix.md`.
//!
//! SaaS delete tolerance: the aigov inventory DELETE 401/403s with the profile
//! token (it needs an internal `/v3/search` auth scope), so on SaaS the inventory
//! is left in place as shared infrastructure on `destroy` (mirrors the guardrails
//! handler — the inventory is shared governance infra, not torn down with the
//! resource). There is no `recover_from_delete_error` hook, so the tolerance is
//! expressed by owning the delete in `pre_delete` (returns `Handled`).

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};
use wxctl_core::types::Flavor;

use crate::cloud_object_storage::aigov_bucket::{aigov_host, cos_config_from_env, ensure_cos_bucket};

pub struct InventoryHandler;

impl ResourceHandler for InventoryHandler {
    /// On Software, fall through to the schema-driven `assetfiles` create
    /// (`Continue`). On SaaS, create the inventory directly against a Cross
    /// Region US COS bucket and return `Handled` (the engine extracts
    /// `id_field: guid` from `metadata.guid`), skipping the default POST.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            if client.deployment().flavor() != Flavor::Saas {
                return Ok(HookOutcome::Continue);
            }

            let name = require_str(resource, "name")?.to_string();
            let description = resource.get("description").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string);
            let is_governed = resource.get("is_governed").and_then(|v| v.as_bool()).unwrap_or(true);
            let bucket_name =
                resource.get("bucket").and_then(|b| b.get("bucket_name")).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{}] inventory on SaaS requires bucket.bucket_name (the globally-unique IBM COS bucket to back the inventory)", error_codes::H901))?.to_string();

            let host = aigov_host()?;
            let token = client.get_token().await.context("inventory: failed to get token for inventory create")?;

            // Idempotent create: adopt an existing same-named inventory rather than
            // minting a duplicate (mirrors guardrails' ensure_inventory). Normal
            // re-applies discover the inventory by guid and never reach pre_create;
            // this guards the state-loss edge.
            if let Some(existing) = find_inventory_by_name(client, &host, &token, &name).await? {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, "inventory already exists — adopting (idempotent create)");
                return Ok(HookOutcome::Handled(existing));
            }

            let cos = cos_config_from_env()?;
            ensure_cos_bucket(client, &cos, &bucket_name, operation_id).await?;

            let mut body = json!({
                "name": name,
                "generator": "wxctl",
                "is_governed": is_governed,
                "bucket": {
                    "bucket_name": bucket_name,
                    "bucket_type": "bmcos_object_storage",
                    "resource_instance_id": cos.crn,
                    "endpoint_url": cos.endpoint,
                    "credentials_rw": { "access_key_id": cos.access_key, "secret_access_key": cos.secret_key }
                }
            });
            if let Some(desc) = description {
                body["description"] = Value::String(desc);
            }

            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, bucket = %bucket_name, "creating COS-backed aigov inventory (SaaS)");
            let url = format!("{host}/v1/aigov/inventories");
            // Hardcoded Bearer is correct: aigov_host() is the IBM-Cloud-public platform
            // host and this branch is SaaS-only (Software returns Continue above).
            let resp = client.raw_client().post(&url).bearer_auth(&token).json(&body).send().await.context("inventory: inventory create request failed")?;
            let created = read_json(resp, "inventory create").await?;
            // metadata.guid must be present — the engine extracts id_field: guid from it.
            if inventory_guid(&created).is_none() {
                bail!("[{}] inventory: create response has no metadata.guid", error_codes::H901);
            }
            Ok(HookOutcome::Handled(created))
        })
    }

    /// On Software, fall through to the schema-driven DELETE
    /// (`?delete_bucket=true` also tears down the assetfiles bucket). On SaaS,
    /// resolve the inventory by name and attempt the DELETE, tolerating 401/403
    /// (CAMS governance infra rejects the profile token) by leaving the inventory
    /// in place as shared infrastructure. Returns `Handled` so the default DELETE
    /// is skipped on SaaS.
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            if client.deployment().flavor() != Flavor::Saas {
                return Ok(HookOutcome::Continue);
            }

            let name = require_str(resource, "name")?.to_string();
            let host = aigov_host()?;
            let token = client.get_token().await.context("inventory: failed to get token for inventory delete")?;

            let Some(existing) = find_inventory_by_name(client, &host, &token, &name).await? else {
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, "inventory already absent — idempotent delete");
                return Ok(HookOutcome::Handled(json!({ "name": name, "deleted": false })));
            };
            let guid = inventory_guid(&existing).ok_or_else(|| anyhow!("[{}] inventory: matched inventory '{name}' has no metadata.guid", error_codes::H901))?.to_string();

            // `delete_bucket` is a REQUIRED query param on the aigov inventory DELETE
            // — omitting it 400s ("One of the required value is missing"). Use
            // `=false` to keep the caller's external COS bucket in place (it is the
            // caller's store, not a platform-managed assetfiles volume). On SaaS the
            // DELETE then 401s (governance scope: internal
            // `/v3/search?auth_scope=catalog,ibm_watsonx_governance_catalog`),
            // tolerated below by leaving the inventory in place as shared infra
            // (live-verified 2026-07-01, SaaS tenant).
            let url = format!("{host}/v1/aigov/inventories/{guid}?delete_bucket=false");
            // Hardcoded Bearer is correct: IBM-Cloud-public platform host, SaaS-only path.
            let resp = client.raw_client().delete(&url).bearer_auth(&token).send().await.context("inventory: inventory delete request failed")?;
            let status = resp.status();
            match status.as_u16() {
                s if (200..300).contains(&s) || s == 404 => Ok(HookOutcome::Handled(json!({ "name": name, "id": guid, "deleted": true }))),
                401 | 403 => {
                    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, name = %name, id = %guid, %status, "SaaS aigov inventory DELETE rejected (auth scope) — leaving inventory in place as shared infrastructure");
                    Ok(HookOutcome::Handled(json!({ "name": name, "id": guid, "deleted": false })))
                }
                _ => {
                    let body = resp.text().await.unwrap_or_default();
                    bail!("[{}] inventory: delete returned {status}: {body}", error_codes::H901);
                }
            }
        })
    }
}

/// GET `{host}/v1/aigov/inventories` and return the matching catalog object
/// (`entity.name == name`), if present. The created inventory IS a /v2/catalogs
/// catalog, so the list returns them under `catalogs[]` with the guid at
/// `metadata.guid`.
async fn find_inventory_by_name(client: &HttpClient, host: &str, token: &str, name: &str) -> Result<Option<Value>> {
    let url = format!("{host}/v1/aigov/inventories");
    // Hardcoded Bearer is correct: IBM-Cloud-public platform host, SaaS-only callers.
    let resp = client.raw_client().get(&url).bearer_auth(token).send().await.context("inventory: inventory list request failed")?;
    let body = read_json(resp, "inventory list").await?;
    let found = body.get("catalogs").and_then(|v| v.as_array()).into_iter().flatten().find(|c| c.get("entity").and_then(|e| e.get("name")).and_then(|v| v.as_str()) == Some(name)).cloned();
    Ok(found)
}

/// The inventory (catalog) guid, read from `metadata.guid`.
fn inventory_guid(catalog: &Value) -> Option<&str> {
    catalog.get("metadata").and_then(|m| m.get("guid")).and_then(|v| v.as_str())
}

/// Parse a successful JSON response; turn any non-2xx into an error carrying the
/// status + body (H901).
async fn read_json(resp: reqwest::Response, what: &str) -> Result<Value> {
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        bail!("[{}] inventory: {what} returned {status}: {body}", error_codes::H901);
    }
    resp.json::<Value>().await.with_context(|| format!("inventory: failed to parse {what} response"))
}

fn require_str<'a>(resource: &'a Value, field: &str) -> Result<&'a str> {
    resource.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{}] inventory missing required field '{field}'", error_codes::H901))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inventory_guid_reads_metadata() {
        let c = json!({ "metadata": { "guid": "cat-123" }, "entity": { "name": "inv" } });
        assert_eq!(inventory_guid(&c), Some("cat-123"));
        assert_eq!(inventory_guid(&json!({ "entity": { "name": "inv" } })), None);
    }

    #[test]
    fn require_str_rejects_empty_and_missing() {
        let r = json!({ "name": "e2e", "blank": "" });
        assert_eq!(require_str(&r, "name").unwrap(), "e2e");
        assert!(require_str(&r, "blank").is_err());
        assert!(require_str(&r, "absent").is_err());
    }
}
