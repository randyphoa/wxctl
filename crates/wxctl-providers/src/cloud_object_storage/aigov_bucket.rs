//! Shared COS-backed aigov inventory primitives.
//!
//! Creating an AI Factsheets / governance (aigov) inventory on **SaaS** requires
//! a real IBM Cloud Object Storage bucket (`bmcos_object_storage` +
//! `credentials_rw`) — the platform-managed `assetfiles` store is CPD/Software-only
//! and is rejected on SaaS (`BUCSV3017E`). The catalog verifies the bucket at the
//! COS instance's **Cross Region US** endpoint, so a regional bucket is reported
//! `BUCSV3006E: Bucket does not exist` even though it exists — the bucket MUST be
//! Cross Region US (region `us`, `LocationConstraint=us-standard`).
//!
//! These helpers are the live-verified path (proven 2026-06-28 by the
//! `openscale/guardrails_policy` handler; see
//! `docs/troubleshoot/guardrails-policy-saas-inventory-fix.md`) and are shared by
//! both the guardrails handler and the `factsheets/inventory` handler.

use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use serde_json::json;
use std::collections::BTreeMap;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;

use super::common::build_cos_client_from_connection;
use super::cos_client::{CosRequest, ServiceIdPolicy, parse_s3_error};

/// COS HMAC configuration for backing a SaaS aigov inventory, read from the
/// profile env. Fields are crate-visible so handlers can compose the inventory
/// `bucket` object directly.
pub(crate) struct CosCfg {
    pub(crate) crn: String,
    pub(crate) access_key: String,
    pub(crate) secret_key: String,
    pub(crate) endpoint: String,
    pub(crate) region: String,
}

/// The aigov inventory host (`COMMON_CORE_URL`), trailing slash trimmed.
pub(crate) fn aigov_host() -> Result<String> {
    std::env::var("COMMON_CORE_URL").ok().filter(|s| !s.is_empty()).map(|s| s.trim_end_matches('/').to_string()).ok_or_else(|| anyhow!("[{}] COMMON_CORE_URL not set — required to resolve the aigov inventory host", error_codes::H901))
}

fn env_nonempty(key: &str) -> Result<String> {
    std::env::var(key).ok().filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{}] {key} not set — COS config required to back a SaaS aigov inventory with a real IBM COS bucket. Add a cloud_object_storage block to the profile.", error_codes::H901))
}

/// Read the COS HMAC config from the profile env: `WXCTL_COS_CRN`,
/// `WXCTL_COS_ACCESS_KEY`, `WXCTL_COS_SECRET_KEY` (required), plus `COS_URL`
/// (default cross-region US endpoint) and `WXCTL_COS_REGION` (default `us`).
pub(crate) fn cos_config_from_env() -> Result<CosCfg> {
    Ok(CosCfg {
        crn: env_nonempty("WXCTL_COS_CRN")?,
        access_key: env_nonempty("WXCTL_COS_ACCESS_KEY")?,
        secret_key: env_nonempty("WXCTL_COS_SECRET_KEY")?,
        endpoint: std::env::var("COS_URL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "https://s3.us.cloud-object-storage.appdomain.cloud".to_string()),
        region: std::env::var("WXCTL_COS_REGION").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| "us".to_string()),
    })
}

/// The COS instance guid (segment 7 of the CRN), used to name a backing bucket
/// deterministically.
pub(crate) fn cos_instance_guid(crn: &str) -> &str {
    crn.split(':').nth(7).unwrap_or("inv")
}

/// HEAD-then-PUT a Cross Region US COS bucket (idempotent), reusing the shared
/// CosClient (SigV4/HMAC). The catalog verifies the bucket at the cross-region
/// endpoint, so a regional bucket is rejected (BUCSV3006E) — region must be `us`.
pub(crate) async fn ensure_cos_bucket(client: &HttpClient, cos: &CosCfg, bucket: &str, operation_id: &str) -> Result<()> {
    let connection = json!({ "type": "ibm_cos", "access_key": cos.access_key, "secret_key": cos.secret_key, "instance_crn": cos.crn, "endpoint": cos.endpoint });
    let cosc = build_cos_client_from_connection(client, &connection)?;
    let head = cosc.send(CosRequest { region: &cos.region, method: Method::HEAD, path: &format!("/{bucket}"), service_id_policy: ServiceIdPolicy::Include, ..Default::default() }, operation_id).await?;
    if head.status.as_u16() == 200 {
        return Ok(());
    }
    let mut headers = BTreeMap::new();
    headers.insert("content-type".to_string(), "text/xml".to_string());
    let cbody = format!("<CreateBucketConfiguration><LocationConstraint>{}-standard</LocationConstraint></CreateBucketConfiguration>", cos.region).into_bytes();
    let resp = cosc.send(CosRequest { region: &cos.region, method: Method::PUT, path: &format!("/{bucket}"), extra_headers: headers, body: cbody, service_id_policy: ServiceIdPolicy::Include, ..Default::default() }, operation_id).await?;
    match resp.status.as_u16() {
        200 | 201 => Ok(()),
        409 => {
            let err = parse_s3_error(&resp.body_str());
            if err.code == "BucketAlreadyOwnedByYou" { Ok(()) } else { bail!("[{}] COS bucket '{bucket}' create returned 409 {}: {}", error_codes::H901, err.code, err.message) }
        }
        status => {
            let err = parse_s3_error(&resp.body_str());
            bail!("[{}] COS bucket '{bucket}' create failed: HTTP {status} {} {}", error_codes::H901, err.code, err.message)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cos_instance_guid_extracts_segment() {
        assert_eq!(cos_instance_guid("crn:v1:bluemix:public:cloud-object-storage:global:a/acct:ed2e6421-2349-458c-a1c0-eb14c381a748::"), "ed2e6421-2349-458c-a1c0-eb14c381a748");
        assert_eq!(cos_instance_guid("bad"), "inv");
    }
}
