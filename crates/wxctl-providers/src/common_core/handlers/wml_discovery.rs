//! Auto-discover Watson Machine Learning instances from the IBM Cloud account.
//!
//! Used by the space pre_create hook to inject `compute` entries when the user
//! doesn't provide them. The WML service resource ID is resolved from the
//! Global Catalog at runtime via `catalog_discovery`. The instance list is
//! cached for the lifetime of the process.

use anyhow::{Context, Result};
use serde_json::Value;
use std::sync::OnceLock;
use tokio::sync::OnceCell;
use wxctl_core::client::HttpClient;

use super::catalog_discovery::{self, AUTH_TYPE_APIKEY, RESOURCE_CONTROLLER_URL, ResourceInstance, ResourceListResponse};

const WML_SERVICE_NAME: &str = "pm-20";

/// Env vars an operator can set to pin which Watson ML instance a space
/// associates when the account holds more than one. A space accepts only one
/// `machine_learning` service, so auto-associating every instance 400s on
/// multi-instance accounts.
const WML_CRN_ENV_VARS: [&str; 2] = ["WXCTL_WML_CRN", "WML_CRN"];

/// Cached WML instance list. Populated on first use.
static WML_CACHE: OnceLock<OnceCell<Vec<ResourceInstance>>> = OnceLock::new();

/// Explicit WML instance CRN from the environment, if set and non-empty.
fn configured_wml_crn() -> Option<String> {
    WML_CRN_ENV_VARS.iter().find_map(|k| std::env::var(k).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty()))
}

fn fmt_instances(resources: &[ResourceInstance]) -> String {
    resources.iter().map(|r| format!("  - {} ({})", r.name, r.crn)).collect::<Vec<_>>().join("\n")
}

/// Pick the single WML instance to associate with a space.
///
/// One instance → use it. More than one → require an explicit CRN
/// (`configured`, from `WXCTL_WML_CRN`/`WML_CRN`) since a space accepts only one
/// `machine_learning` service; bail otherwise. The single-instance path matches
/// the original behaviour (which associated the lone instance).
fn select_wml_instance<'a>(resources: &'a [ResourceInstance], configured: Option<&str>) -> Result<&'a ResourceInstance> {
    match resources.len() {
        0 => anyhow::bail!(
            "No Watson Machine Learning instances found in this account. \
             Create one at https://cloud.ibm.com/catalog/services/watson-machine-learning \
             or provide the compute array explicitly."
        ),
        1 => Ok(&resources[0]),
        n => {
            let Some(want) = configured.map(str::trim).filter(|s| !s.is_empty()) else {
                anyhow::bail!("Found {} Watson Machine Learning instances — a space accepts only one; set WXCTL_WML_CRN (or provide the compute array) to choose one:\n{}", n, fmt_instances(resources));
            };
            resources.iter().find(|r| r.crn == want || want.contains(r.guid.as_str())).ok_or_else(|| anyhow::anyhow!("Configured WML CRN ({}) matched none of the {} Watson Machine Learning instances in this account:\n{}", want, n, fmt_instances(resources)))
        }
    }
}

/// Returns true if the compute array is absent/empty AND the client uses API key auth.
fn needs_wml_injection(resource: &Value, client: &HttpClient) -> bool {
    let has_compute = resource.get("compute").and_then(|c| c.as_array()).is_some_and(|a| !a.is_empty());
    !has_compute && client.auth_type() == AUTH_TYPE_APIKEY
}

/// List WML service instances in the account.
/// Result is cached — subsequent calls return immediately.
async fn discover_wml_instances(client: &HttpClient, operation_id: &str) -> Result<Vec<ResourceInstance>> {
    let cell = WML_CACHE.get_or_init(OnceCell::new);

    cell.get_or_try_init(|| async {
        let resource_id = catalog_discovery::resolve_resource_id(client, WML_SERVICE_NAME, operation_id).await?;

        let token = client.get_token().await.context("Failed to get IAM token for WML discovery")?;

        let response = client.raw_client().get(RESOURCE_CONTROLLER_URL).query(&[("resource_id", resource_id.as_str()), ("type", "service_instance")]).bearer_auth(&token).send().await.context("Failed to call Resource Controller API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("Resource Controller API returned {}: {}", status, body);
        }

        let list: ResourceListResponse = response.json().await.context("Failed to parse Resource Controller response")?;

        if list.resources.is_empty() {
            anyhow::bail!(
                "No Watson Machine Learning instances found in this account. \
                 Create one at https://cloud.ibm.com/catalog/services/watson-machine-learning \
                 or provide the compute array explicitly."
            );
        }

        for r in &list.resources {
            tracing::info!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                wml_name = %r.name,
                wml_crn = %r.crn,
                "Auto-discovered WML instance"
            );
        }

        Ok(list.resources)
    })
    .await
    .cloned()
}

/// Inject WML compute entries into a space resource if not already provided.
/// Discovers all WML instances in the account and adds them to the `compute` array.
pub async fn ensure_space_compute(resource: &mut Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    if super::skip_on_non_saas(client, operation_id, "wml_discovery") {
        return Ok(());
    }

    if !needs_wml_injection(resource, client) {
        return Ok(());
    }

    let instances = discover_wml_instances(client, operation_id).await?;
    let inst = select_wml_instance(&instances, configured_wml_crn().as_deref())?;
    tracing::info!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        wml_name = %inst.name,
        wml_crn = %inst.crn,
        "Selected WML instance for space compute"
    );

    let entry = serde_json::json!({ "name": inst.name, "crn": inst.crn, "guid": inst.guid, "type": "machine_learning" });

    let obj = resource.as_object_mut().ok_or_else(|| anyhow::anyhow!("resource is not a JSON object"))?;
    let compute = obj.entry("compute").or_insert_with(|| serde_json::json!([])).as_array_mut().ok_or_else(|| anyhow::anyhow!("compute field is not an array"))?;
    compute.push(entry);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inst(name: &str, crn: &str, guid: &str) -> ResourceInstance {
        ResourceInstance { name: name.to_string(), crn: crn.to_string(), guid: guid.to_string() }
    }

    const CRN_EU: &str = "crn:v1:bluemix:public:pm-20:eu-gb:a/acct:eeee1111-2222-3333-4444-555566667777::";
    const CRN_TOR: &str = "crn:v1:bluemix:public:pm-20:ca-tor:a/acct:cccc2222-3333-4444-5555-666677778888::";

    const GUID_EU: &str = "eeee1111-2222-3333-4444-555566667777";
    const GUID_TOR: &str = "cccc2222-3333-4444-5555-666677778888";

    fn two() -> Vec<ResourceInstance> {
        vec![inst("tor", CRN_TOR, GUID_TOR), inst("eu", CRN_EU, GUID_EU)]
    }

    // OK cases: single-instance fast path ignores `configured`; multi-instance requires
    // it and matches by exact CRN or by embedding the instance guid.
    #[test]
    fn select_wml_instance_ok_cases() {
        let one = vec![inst("only", CRN_EU, GUID_EU)];
        assert_eq!(select_wml_instance(&one, Some("crn:does:not:match")).unwrap().crn, CRN_EU, "single + nonmatching configured");
        assert_eq!(select_wml_instance(&one, None).unwrap().crn, CRN_EU, "single + no configured");
        assert_eq!(select_wml_instance(&two(), Some(CRN_EU)).unwrap().name, "eu", "multi by exact crn");
        assert_eq!(select_wml_instance(&two(), Some("crn:v1:bluemix:public:pm-20:eu-gb:a/acct:eeee1111-2222-3333-4444-555566667777:workspace:x")).unwrap().name, "eu", "multi by guid substring");
    }

    // Error cases: 0 instances; multi with no/non-matching configured. The listing-error
    // wording (count, "only one", env-var hint, "matched none") is part of the contract.
    #[test]
    fn select_wml_instance_error_cases() {
        let err = select_wml_instance(&[], None).unwrap_err().to_string();
        assert!(err.contains("No Watson Machine Learning instances"), "{err}");

        // Multi, no configured → ambiguity error names the count, env var, and "only one".
        let err = select_wml_instance(&two(), None).unwrap_err().to_string();
        assert!(err.contains("Found 2 Watson Machine Learning instances") && err.contains("WXCTL_WML_CRN") && err.contains("only one"), "{err}");

        // Multi, non-matching configured → "matched none".
        let err = select_wml_instance(&two(), Some("crn:v1:bluemix:public:pm-20:us-south:a/acct:dddd::")).unwrap_err().to_string();
        assert!(err.contains("matched none"), "{err}");
    }
}
