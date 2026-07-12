//! Resolve IBM Cloud service resource IDs from the Global Catalog API.
//!
//! Given a service name (e.g. "cloud-object-storage", "pm-20"), queries the
//! Global Catalog to find its resource UUID. Results are cached per service
//! name for the lifetime of the process.
//!
//! Resolution strategy:
//! 1. Search `?q={name}` for a direct `kind=service` match
//! 2. Fallback: fetch the `oss.{name}` entry and extract
//!    `metadata.other.oss.reference_catalog_id`
//!
//! The fallback is needed because some services (e.g. cloud-object-storage)
//! are not returned by the catalog search but have an OSS reference entry.

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::OnceCell;
use wxctl_core::client::HttpClient;

pub(super) const RESOURCE_CONTROLLER_URL: &str = "https://resource-controller.cloud.ibm.com/v2/resource_instances";
pub(super) const AUTH_TYPE_APIKEY: &str = "apikey";

const GLOBAL_CATALOG_URL: &str = "https://globalcatalog.cloud.ibm.com/api/v1";

/// IBM Cloud Resource Controller instance — shared by COS and WML discovery.
#[derive(Debug, Clone, Deserialize)]
pub(super) struct ResourceInstance {
    pub name: String,
    pub crn: String,
    pub guid: String,
}

#[derive(Debug, Deserialize)]
pub(super) struct ResourceListResponse {
    pub resources: Vec<ResourceInstance>,
}

#[derive(Debug, Deserialize)]
struct CatalogResource {
    id: String,
    name: String,
    kind: String,
}

#[derive(Debug, Deserialize)]
struct CatalogResponse {
    resources: Vec<CatalogResource>,
}

/// Per-service-name cache of (OnceCell holding the resolved resource ID).
static CATALOG_CACHE: OnceLock<Mutex<HashMap<String, Arc<OnceCell<String>>>>> = OnceLock::new();

fn get_or_create_cell(service_name: &str) -> Arc<OnceCell<String>> {
    let map_mutex = CATALOG_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut map = map_mutex.lock().unwrap();
    map.entry(service_name.to_string()).or_insert_with(|| Arc::new(OnceCell::new())).clone()
}

/// Try the search API first: `?q={name}` and filter for `kind=service`.
async fn try_search(client: &HttpClient, token: &str, service_name: &str) -> Result<Option<String>> {
    let response = client.raw_client().get(GLOBAL_CATALOG_URL).query(&[("q", service_name)]).bearer_auth(token).send().await.context("Failed to call Global Catalog API")?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let catalog: CatalogResponse = response.json().await.context("Failed to parse Global Catalog response")?;

    Ok(catalog.resources.iter().find(|r| r.name == service_name && r.kind == "service").map(|r| r.id.clone()))
}

/// Fallback: fetch `oss.{name}` and extract `metadata.other.oss.reference_catalog_id`.
/// Some services (e.g. cloud-object-storage) are not returned by the catalog
/// search but have an OSS entry that references the actual service catalog ID.
async fn try_oss_reference(client: &HttpClient, token: &str, service_name: &str) -> Result<Option<String>> {
    let url = format!("{}/oss.{}", GLOBAL_CATALOG_URL, service_name);
    let response = client.raw_client().get(&url).bearer_auth(token).send().await.context("Failed to call Global Catalog OSS endpoint")?;

    if !response.status().is_success() {
        return Ok(None);
    }

    let body: Value = response.json().await.context("Failed to parse Global Catalog OSS response")?;

    Ok(body.pointer("/metadata/other/oss/reference_catalog_id").and_then(|v| v.as_str()).map(|s| s.to_string()))
}

/// Resolve a service name to its Global Catalog resource ID.
/// The result is cached — subsequent calls for the same service return immediately.
pub async fn resolve_resource_id(client: &HttpClient, service_name: &str, operation_id: &str) -> Result<String> {
    let cell = get_or_create_cell(service_name);

    cell.get_or_try_init(|| {
        let service_name = service_name.to_string();
        async move {
            let token = client.get_token().await.context("Failed to get IAM token for Global Catalog lookup")?;

            // Strategy 1: search for a direct kind=service match
            if let Some(id) = try_search(client, &token, &service_name).await? {
                tracing::info!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    service_name = %service_name,
                    resource_id = %id,
                    "Resolved service resource ID from Global Catalog search"
                );
                return Ok(id);
            }

            // Strategy 2: fetch the oss.{name} entry for reference_catalog_id
            if let Some(id) = try_oss_reference(client, &token, &service_name).await? {
                tracing::info!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    service_name = %service_name,
                    resource_id = %id,
                    "Resolved service resource ID from Global Catalog OSS reference"
                );
                return Ok(id);
            }

            anyhow::bail!("Service '{}' not found in Global Catalog", service_name)
        }
    })
    .await
    .cloned()
}
