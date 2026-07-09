//! Auto-discover Cloud Object Storage instances from the IBM Cloud account.
//!
//! Used by space and project pre_create hooks to inject `storage.resource_crn`
//! (and `storage.guid` for projects) when the user doesn't provide them.
//! The result is cached for the lifetime of the process via `OnceLock`.

use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::OnceLock;
use tokio::sync::OnceCell;
use wxctl_core::client::HttpClient;

use super::catalog_discovery::{self, AUTH_TYPE_APIKEY, RESOURCE_CONTROLLER_URL, ResourceInstance, ResourceListResponse};

const COS_SERVICE_NAME: &str = "cloud-object-storage";
const STORAGE_TYPE_BMCOS: &str = "bmcos_object_storage";
const STORAGE_TYPE_ASSETFILES: &str = "assetfiles";

/// Env vars an operator can set to pin which COS instance backs project/space
/// storage when the account holds more than one. Needed because the project
/// schema doesn't expose `storage.resource_crn`, so auto-discovery is the only
/// path that sets it — and it can't otherwise disambiguate multiple instances.
const COS_CRN_ENV_VARS: [&str; 2] = ["WXCTL_COS_CRN", "COS_CRN"];

/// Explicit COS instance CRN from the environment, if set and non-empty.
fn configured_cos_crn() -> Option<String> {
    COS_CRN_ENV_VARS.iter().find_map(|k| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
}

fn fmt_instances(resources: &[ResourceInstance]) -> String {
    resources.iter().map(|r| format!("  - {} ({})", r.name, r.crn)).collect::<Vec<_>>().join("\n")
}

/// Pick the COS instance to back project/space storage.
///
/// One instance → use it. More than one → require an explicit CRN
/// (`configured`, from `WXCTL_COS_CRN`/`COS_CRN`) and select the match; bail
/// otherwise. The single-instance fast path is unchanged from the original
/// auto-discovery, so `configured` only matters when disambiguation is needed.
fn select_cos_instance<'a>(resources: &'a [ResourceInstance], configured: Option<&str>) -> Result<&'a ResourceInstance> {
    match resources.len() {
        0 => anyhow::bail!(
            "No Cloud Object Storage instances found in this account. \
             Create one at https://cloud.ibm.com/objectstorage or provide storage.resource_crn explicitly."
        ),
        1 => Ok(&resources[0]),
        n => {
            let Some(want) = configured.map(str::trim).filter(|s| !s.is_empty()) else {
                anyhow::bail!("Found {} COS instances — set WXCTL_COS_CRN (or provide storage.resource_crn) to choose one:\n{}", n, fmt_instances(resources));
            };
            resources.iter().find(|r| r.crn == want || want.contains(r.guid.as_str())).ok_or_else(|| anyhow::anyhow!("Configured COS CRN ({}) matched none of the {} COS instances in this account:\n{}", want, n, fmt_instances(resources)))
        }
    }
}

/// Cached COS discovery result: (crn, guid). Populated on first use.
static COS_CACHE: OnceLock<OnceCell<(String, String)>> = OnceLock::new();

fn needs_cos_injection(resource: &Value, client: &HttpClient) -> bool {
    let already_set = resource.get("storage").and_then(|s| s.get("resource_crn")).and_then(|v| v.as_str()).is_some();
    !already_set && client.auth_type() == AUTH_TYPE_APIKEY
}

/// Look up COS instances in the account and return (crn, guid).
/// Result is cached — subsequent calls return immediately.
pub(crate) async fn discover_cos_instance(client: &HttpClient, operation_id: &str) -> Result<(String, String)> {
    let cell = COS_CACHE.get_or_init(OnceCell::new);

    cell.get_or_try_init(|| async {
        let resource_id = catalog_discovery::resolve_resource_id(client, COS_SERVICE_NAME, operation_id).await?;

        let token = client.get_token().await.context("Failed to get IAM token for COS discovery")?;

        let response = client.raw_client().get(RESOURCE_CONTROLLER_URL).query(&[("resource_id", resource_id.as_str()), ("type", "service_instance")]).bearer_auth(&token).send().await.context("Failed to call Resource Controller API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Resource Controller API returned {}: {}", status, body);
        }

        let list: ResourceListResponse = response.json().await.context("Failed to parse Resource Controller response")?;

        let instance = select_cos_instance(&list.resources, configured_cos_crn().as_deref())?;
        tracing::info!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            cos_name = %instance.name,
            cos_crn = %instance.crn,
            "Selected COS instance"
        );
        Ok((instance.crn.clone(), instance.guid.clone()))
    })
    .await
    .cloned()
}

/// Ensure `storage.resource_crn` is set on a resource, returning the storage object.
fn ensure_storage_object(resource: &mut Value) -> Result<&mut serde_json::Map<String, Value>> {
    resource.as_object_mut().ok_or_else(|| anyhow::anyhow!("resource is not a JSON object"))?.entry("storage").or_insert_with(|| serde_json::json!({})).as_object_mut().ok_or_else(|| anyhow::anyhow!("storage field is not an object"))
}

/// Inject COS storage fields into a space resource if not already provided.
/// Spaces only need `storage.resource_crn`.
pub async fn ensure_space_storage(resource: &mut Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    if super::skip_on_non_saas(client, operation_id, "cos_discovery") {
        return Ok(());
    }

    if !needs_cos_injection(resource, client) {
        return Ok(());
    }

    let (crn, _guid) = discover_cos_instance(client, operation_id).await?;
    let storage = ensure_storage_object(resource)?;
    storage.insert("resource_crn".to_string(), Value::String(crn));
    Ok(())
}

/// Inject `storage` fields into a project resource if not already provided.
/// SaaS projects need `storage.type=bmcos_object_storage` plus `resource_crn` and `guid`
/// from an auto-discovered COS instance. Software projects only need
/// `storage.type=assetfiles` — the API auto-generates the guid for the
/// platform-managed assetfiles volume.
pub async fn ensure_project_storage(resource: &mut Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    let storage_already_set = resource.get("storage").and_then(|s| s.get("type")).and_then(|v| v.as_str()).is_some();
    if storage_already_set {
        return Ok(());
    }

    if client.deployment().flavor() != wxctl_core::types::Flavor::Saas {
        let storage = ensure_storage_object(resource)?;
        storage.insert("type".to_string(), Value::String(STORAGE_TYPE_ASSETFILES.to_string()));
        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            "injected storage.type=assetfiles for Software project",
        );
        return Ok(());
    }

    if !needs_cos_injection(resource, client) {
        return Ok(());
    }

    let (crn, guid) = discover_cos_instance(client, operation_id).await?;
    let storage = ensure_storage_object(resource)?;
    if !storage.contains_key("type") {
        storage.insert("type".to_string(), Value::String(STORAGE_TYPE_BMCOS.to_string()));
    }
    storage.insert("resource_crn".to_string(), Value::String(crn));
    storage.insert("guid".to_string(), Value::String(guid));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(name: &str, crn: &str, guid: &str) -> ResourceInstance {
        ResourceInstance { name: name.to_string(), crn: crn.to_string(), guid: guid.to_string() }
    }

    const CRN_A: &str = "crn:v1:bluemix:public:cloud-object-storage:global:a/acct:aaaa1111-2222-3333-4444-555566667777::";
    const CRN_B: &str = "crn:v1:bluemix:public:cloud-object-storage:global:a/acct:bbbb1111-2222-3333-4444-555566667777::";

    const GUID_A: &str = "aaaa1111-2222-3333-4444-555566667777";
    const GUID_B: &str = "bbbb1111-2222-3333-4444-555566667777";

    fn two() -> Vec<ResourceInstance> {
        vec![inst("a", CRN_A, GUID_A), inst("b", CRN_B, GUID_B)]
    }

    // OK cases: single-instance fast path ignores `configured` entirely; multi-instance
    // requires `configured` and matches by exact CRN or by embedding the instance guid.
    #[test]
    fn select_cos_instance_ok_cases() {
        let one = vec![inst("only", CRN_A, GUID_A)];
        // Single instance is returned even when `configured` doesn't match it (fast path).
        assert_eq!(select_cos_instance(&one, Some("crn:does:not:match")).unwrap().crn, CRN_A, "single + nonmatching configured");
        assert_eq!(select_cos_instance(&one, None).unwrap().crn, CRN_A, "single + no configured");
        // Multi: exact CRN match.
        assert_eq!(select_cos_instance(&two(), Some(CRN_B)).unwrap().name, "b", "multi by exact crn");
        // Multi: a configured CRN that embeds the instance guid (trailing colons differ) matches.
        assert_eq!(select_cos_instance(&two(), Some("crn:v1:bluemix:public:cloud-object-storage:global:a/acct:bbbb1111-2222-3333-4444-555566667777:bucket:thing")).unwrap().name, "b", "multi by guid substring");
    }

    // Error cases: 0 instances; multi with no/whitespace/non-matching configured. The
    // listing-error message wording (instance count, env-var hint, CRNs, "matched none")
    // is part of the contract.
    #[test]
    fn select_cos_instance_error_cases() {
        // Zero instances.
        let err = select_cos_instance(&[], Some(CRN_A)).unwrap_err().to_string();
        assert!(err.contains("No Cloud Object Storage instances"), "{err}");

        // Multi, no configured → ambiguity error lists both CRNs + the env-var hint.
        let err = select_cos_instance(&two(), None).unwrap_err().to_string();
        assert!(err.contains("Found 2 COS instances") && err.contains("WXCTL_COS_CRN") && err.contains(CRN_A) && err.contains(CRN_B), "{err}");

        // Whitespace-only configured is treated as unset → same ambiguity error.
        let err = select_cos_instance(&two(), Some("   ")).unwrap_err().to_string();
        assert!(err.contains("Found 2 COS instances"), "{err}");

        // Multi, non-matching configured → "matched none".
        let err = select_cos_instance(&two(), Some("crn:v1:bluemix:public:cloud-object-storage:global:a/acct:cccc::")).unwrap_err().to_string();
        assert!(err.contains("matched none"), "{err}");
    }
}
