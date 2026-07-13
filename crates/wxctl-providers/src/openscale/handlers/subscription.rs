//! `openscale/subscription` handler — after the generic create POST lands the
//! subscription, create its default `payload_logging` + `feedback` data sets
//! (`POST /v2/subscriptions/{id}/tables/{dataset_type}`) and poll the subscription
//! to `active`. Without the default data sets the subscription stalls in
//! `preparing` and OpenScale's own default data-set step throws
//! AIQCS0002E/AIQPO0003E (ClassCastException); monitors created after this need an
//! active subscription with its tables. The full `asset_properties`
//! (input/output/training_data_schema, prediction_probability_field) is authored as
//! a free-form object in config and passed through verbatim by the generic
//! reconciler — no create-body reshape here, so this handler runs only post_create.

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;
use wxctl_core::traits::ResourceHandler;

/// Data sets OpenScale needs before a subscription activates and its monitors can
/// compute metrics. `payload_logging` drives the `preparing -> active` transition;
/// `feedback` backs the quality monitor.
const DEFAULT_DATASET_TYPES: [&str; 2] = ["payload_logging", "feedback"];

pub struct SubscriptionHandler;

impl ResourceHandler for SubscriptionHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let Some(sub_id) = crate::util::resource_id(response).map(str::to_string) else {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "subscription", reason = "missing_id_in_create_response", "skipping default data-set creation");
                return Ok(());
            };
            for dataset_type in DEFAULT_DATASET_TYPES {
                create_default_dataset(client, &sub_id, dataset_type, operation_id).await?;
            }
            wait_for_subscription_active(client, &sub_id, operation_id).await?;
            if let Some(path) = resource.get("payload_records").and_then(|v| v.as_str()) {
                seed_records(client, &sub_id, "payload_logging", path, operation_id).await?;
            }
            if let Some(path) = resource.get("feedback_records").and_then(|v| v.as_str()) {
                seed_records(client, &sub_id, "feedback", path, operation_id).await?;
            }
            Ok(())
        })
    }
}

/// POST the default table for one data-set type against the OpenScale service base
/// (`{base}/openscale/{guid}` + `/v2/...`). 200/201/202 = accepted (202 = async
/// table build). 400/409 = the table already exists (idempotent re-run after a
/// partial create) — tolerated. Any other status is fatal.
async fn create_default_dataset(client: &HttpClient, sub_id: &str, dataset_type: &str, operation_id: &str) -> Result<()> {
    let token = client.get_token().await.context("subscription: failed to get token for default data-set create")?;
    let url = format!("{}/v2/subscriptions/{sub_id}/tables/{dataset_type}", client.base_url().trim_end_matches('/'));
    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, subscription_id = %sub_id, dataset_type = %dataset_type, "creating default OpenScale data set");
    // Raw request (not execute()) because the already-exists tolerance below needs the
    // raw status + body; apply_auth_scheme keeps zenapikey/CP4D auth working.
    let req = client.raw_client().post(&url).json(&json!({}));
    let resp = client.apply_auth_scheme(req, &token)?.send().await.context("subscription: default data-set request failed")?;
    let status = resp.status().as_u16();
    if is_accepted(status) {
        return Ok(());
    }
    if is_already_exists(status) {
        // 409 = table already exists (idempotent re-run after a partial create). 400 is
        // ALSO tolerated for the same case, but the precise already-exists error code has
        // not been live-captured — a genuine bad request would otherwise only surface
        // ~150s later as an opaque activation timeout, so log the (redacted) body to make
        // the root cause diagnosable. Capture attempted live 2026-07-04 (SaaS tenant):
        // unreachable via config re-apply — discovery finds the subscription, decides NoOp,
        // and post_create (the only caller) never re-runs, so no table POST is ever
        // re-issued. Tightening needs a forced create against an existing subscription
        // (handler-level replay). For calibration: a genuine failure on this endpoint
        // observed live was a 500 with an `AIQ*` code (AIQCS0004E, missing
        // asset_properties), consistent with the run_collision AIQMM0012E pattern below.
        if status == 400 {
            let body = resp.text().await.unwrap_or_default();
            let redacted = serde_json::from_str::<Value>(&body).map(|v| wxctl_core::logging::redact_sensitive(&v).to_string()).unwrap_or_else(|_| "<non-JSON body omitted>".to_string());
            tracing::warn!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                subscription_id = %sub_id,
                dataset_type = %dataset_type,
                http_status = status,
                body = %redacted,
                "default data-set POST returned 400 — treated as already-exists; if the subscription later stalls in preparing, this body is the root cause"
            );
        }
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    bail!("[{}] subscription: creating default {dataset_type} data set returned {status}: {body}", error_codes::H901);
}

/// Poll the subscription until `entity.status.state` is `active` (or `error`).
/// The default data sets trigger the `preparing -> active` transition
/// asynchronously; monitors created after this must see an active subscription.
async fn wait_for_subscription_active(client: &HttpClient, sub_id: &str, operation_id: &str) -> Result<Value> {
    let max_attempts = 30;
    crate::util::poll_until(max_attempts, Duration::from_secs(5), crate::util::PollTimeout::Bail(format!("[{}] subscription {sub_id} did not reach active state", error_codes::H901)), None::<String>, |attempt, mut prev| async move {
        let path = format!("/v2/subscriptions/{sub_id}");
        let spec = wxctl_core::client::RequestSpec::new(wxctl_core::client::Method::GET, &path).body(wxctl_core::client::BodyKind::None);
        let response: Value = client.execute(operation_id, spec).await?;
        let state = subscription_state(&response).unwrap_or("unknown").to_string();
        if prev.as_deref() != Some(state.as_str()) {
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, subscription_id = %sub_id, status = %state, attempt = attempt, max_attempts = max_attempts, "subscription status observed");
            prev = Some(state.clone());
        }
        let outcome = match state.as_str() {
            "active" => crate::util::PollOutcome::Done(response.clone()),
            "error" => crate::util::PollOutcome::Failed(format!("[{}] subscription {sub_id} entered error state", error_codes::H901)),
            _ => crate::util::PollOutcome::Pending,
        };
        Ok((outcome, prev))
    })
    .await
}

/// Read the JSON records file at `path`, resolve the subscription's data set of
/// `dataset_type`, store the records, and wait for the async store to complete.
/// Missing/malformed file, missing data set, or non-2xx store is fatal (H901).
async fn seed_records(client: &HttpClient, sub_id: &str, dataset_type: &str, path: &str, operation_id: &str) -> Result<()> {
    let bytes = std::fs::read(path).map_err(|e| anyhow!("[{}] subscription: failed to read {dataset_type} records file '{path}': {e}", error_codes::H901))?;
    let records = parse_records_array(&bytes).with_context(|| format!("[{}] subscription: invalid {dataset_type} records file '{path}'", error_codes::H901))?;
    let dataset_id = resolve_dataset_id(client, sub_id, dataset_type, operation_id).await?;
    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, subscription_id = %sub_id, dataset_type = %dataset_type, dataset_id = %dataset_id, records = records.len(), "storing OpenScale records");
    if let Some(request_id) = store_records(client, &dataset_id, &records).await? {
        wait_for_store_complete(client, &dataset_id, &request_id, operation_id).await?;
    }
    Ok(())
}

/// Parse a records file's bytes into the JSON array OpenScale's store endpoint
/// expects (`application/json` body type is `array`). A top-level array is used
/// verbatim (and must be non-empty); a single object is wrapped into a one-element
/// array; anything else is an error.
fn parse_records_array(bytes: &[u8]) -> Result<Vec<Value>> {
    let value: Value = serde_json::from_slice(bytes).context("records file is not valid JSON")?;
    match value {
        Value::Array(items) if !items.is_empty() => Ok(items),
        Value::Array(_) => bail!("records file is an empty JSON array"),
        Value::Object(_) => Ok(vec![value]),
        _ => bail!("records file must be a JSON array or object"),
    }
}

/// GET the subscription's data set of the given type
/// (`/v2/data_sets?target.target_id=<sub>&target.target_type=subscription&type=<t>`)
/// and return the first match's id. Errors if none exists (H901).
async fn resolve_dataset_id(client: &HttpClient, sub_id: &str, dataset_type: &str, operation_id: &str) -> Result<String> {
    let spec = wxctl_core::client::RequestSpec::new(wxctl_core::client::Method::GET, "/v2/data_sets").query_param("target.target_id", sub_id).query_param("target.target_type", "subscription").query_param("type", dataset_type).body(wxctl_core::client::BodyKind::None);
    let parsed: Value = client.execute(operation_id, spec).await.with_context(|| format!("[{}] subscription: data-set lookup for {dataset_type} failed", error_codes::H901))?;
    dataset_id_from_list(&parsed).map(str::to_string).ok_or_else(|| anyhow!("[{}] subscription: no {dataset_type} data set found for subscription {sub_id}", error_codes::H901))
}

/// Read the first data set's id from a `/v2/data_sets` list response
/// (`{data_sets: [{metadata: {id}}]}`).
fn dataset_id_from_list(response: &Value) -> Option<&str> {
    response.get("data_sets").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|d| d.pointer("/metadata/id")).and_then(|v| v.as_str())
}

/// POST the records array to `/v2/data_sets/{id}/records`. Returns the async store
/// request id parsed from the 202 `Location` header (None if absent). Non-2xx is fatal.
async fn store_records(client: &HttpClient, dataset_id: &str, records: &[Value]) -> Result<Option<String>> {
    let token = client.get_token().await.context("subscription: failed to get token for records store")?;
    let url = format!("{}/v2/data_sets/{dataset_id}/records", client.base_url().trim_end_matches('/'));
    // Raw request (not execute()) because the async store id comes from the 202
    // Location header, which execute() discards; apply_auth_scheme keeps
    // zenapikey/CP4D auth working.
    let req = client.raw_client().post(&url).json(&Value::Array(records.to_vec()));
    let resp = client.apply_auth_scheme(req, &token)?.send().await.context("subscription: records store request failed")?;
    let status = resp.status().as_u16();
    let location = resp.headers().get(reqwest::header::LOCATION).and_then(|v| v.to_str().ok()).map(str::to_string);
    if !is_accepted(status) {
        let body = resp.text().await.unwrap_or_default();
        bail!("[{}] subscription: storing records in data set {dataset_id} returned {status}: {body}", error_codes::H901);
    }
    Ok(location.as_deref().and_then(request_id_from_location).map(str::to_string))
}

/// The async store request id is the last non-empty path segment of the 202
/// `Location` header (`.../v2/data_sets/{id}/requests/{request_id}`).
fn request_id_from_location(location: &str) -> Option<&str> {
    location.trim_end_matches('/').rsplit('/').next().filter(|s| !s.is_empty())
}

/// Poll `/v2/data_sets/{id}/requests/{request_id}` until the store request reports
/// `active` (done) or `error` (fail); bounded at ~60s (12 x 5s).
async fn wait_for_store_complete(client: &HttpClient, dataset_id: &str, request_id: &str, operation_id: &str) -> Result<Value> {
    let max_attempts = 12;
    crate::util::poll_until(max_attempts, Duration::from_secs(5), crate::util::PollTimeout::Bail(format!("[{}] subscription: records store request {request_id} did not complete", error_codes::H901)), None::<String>, |attempt, mut prev| async move {
        let path = format!("/v2/data_sets/{dataset_id}/requests/{request_id}");
        let spec = wxctl_core::client::RequestSpec::new(wxctl_core::client::Method::GET, &path).body(wxctl_core::client::BodyKind::None);
        let response: Value = client.execute(operation_id, spec).await?;
        let state = store_state(&response).unwrap_or("unknown").to_string();
        if prev.as_deref() != Some(state.as_str()) {
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, dataset_id = %dataset_id, request_id = %request_id, status = %state, attempt = attempt, max_attempts = max_attempts, "records store status observed");
            prev = Some(state.clone());
        }
        let outcome = match store_state_outcome(&state) {
            StoreOutcome::Done => crate::util::PollOutcome::Done(response.clone()),
            StoreOutcome::Failed => crate::util::PollOutcome::Failed(format!("[{}] subscription: records store request {request_id} entered error state", error_codes::H901)),
            StoreOutcome::Pending => crate::util::PollOutcome::Pending,
        };
        Ok((outcome, prev))
    })
    .await
}

/// Read the store request lifecycle state from a requests-status GET (top-level
/// `state`, or `entity.status.state` when enveloped).
fn store_state(response: &Value) -> Option<&str> {
    response.get("state").and_then(|v| v.as_str()).or_else(|| response.pointer("/entity/status/state").and_then(|v| v.as_str()))
}

/// Classify a store-request `state` into a poll step. `active` = stored;
/// `error` = failed; everything else (`preparing`, `pending`, ...) keeps polling.
fn store_state_outcome(state: &str) -> StoreOutcome {
    match state {
        "active" => StoreOutcome::Done,
        "error" => StoreOutcome::Failed,
        _ => StoreOutcome::Pending,
    }
}

/// Terminal classification of one store-request poll attempt.
enum StoreOutcome {
    Done,
    Failed,
    Pending,
}

/// Read the subscription lifecycle state from a GET response (`entity.status.state`).
fn subscription_state(response: &Value) -> Option<&str> {
    response.pointer("/entity/status/state").and_then(|v| v.as_str())
}

/// 2xx success codes returned by the tables endpoint (202 = async accepted).
fn is_accepted(status: u16) -> bool {
    matches!(status, 200..=202)
}

/// OpenScale returns 400 or 409 when a data-set table already exists for the type.
fn is_already_exists(status: u16) -> bool {
    matches!(status, 400 | 409)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn subscription_state_reads_nested_status() {
        let active = json!({"entity": {"status": {"state": "active"}}});
        assert_eq!(subscription_state(&active), Some("active"));
        let preparing = json!({"entity": {"status": {"state": "preparing"}}});
        assert_eq!(subscription_state(&preparing), Some("preparing"));
        let absent = json!({"entity": {"status": {}}});
        assert_eq!(subscription_state(&absent), None);
    }

    #[test]
    fn status_classifiers() {
        assert!(is_accepted(202));
        assert!(is_accepted(201));
        assert!(!is_accepted(500));
        assert!(is_already_exists(409));
        assert!(is_already_exists(400));
        assert!(!is_already_exists(202));
    }

    #[test]
    fn parse_records_array_shapes() {
        let arr = parse_records_array(br#"[{"request":{},"response":{}}]"#).unwrap();
        assert_eq!(arr.len(), 1);
        let obj = parse_records_array(br#"{"fields":["a"],"values":[[1]]}"#).unwrap();
        assert_eq!(obj.len(), 1, "a single object is wrapped");
        assert!(parse_records_array(b"[]").is_err(), "empty array rejected");
        assert!(parse_records_array(b"42").is_err(), "scalar rejected");
        assert!(parse_records_array(b"not json").is_err());
    }

    #[test]
    fn request_id_parsed_from_location() {
        assert_eq!(request_id_from_location("https://h/openscale/g/v2/data_sets/ds-1/requests/req-9"), Some("req-9"));
        assert_eq!(request_id_from_location("/v2/data_sets/ds-1/requests/req-9/"), Some("req-9"));
        assert_eq!(request_id_from_location(""), None);
    }

    #[test]
    fn store_state_classified() {
        assert!(matches!(store_state_outcome("active"), StoreOutcome::Done));
        assert!(matches!(store_state_outcome("error"), StoreOutcome::Failed));
        assert!(matches!(store_state_outcome("preparing"), StoreOutcome::Pending));
    }

    #[test]
    fn dataset_id_read_from_list() {
        let list = json!({"data_sets": [{"metadata": {"id": "ds-1"}}]});
        assert_eq!(dataset_id_from_list(&list), Some("ds-1"));
        assert_eq!(dataset_id_from_list(&json!({"data_sets": []})), None);
    }
}
