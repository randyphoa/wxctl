//! `instana_alert` handler — Instana's Alerting Configuration API
//! (`/api/events/settings/alerts`) has NO POST. A configuration is created AND
//! updated by the same idempotent upsert `PUT /api/events/settings/alerts/{id}`
//! with a CLIENT-SUPPLIED id (a declared schema field, also the `get_by_id`
//! discovery id_source).
//!
//! Both create AND update are handler-owned: `pre_create` and `pre_update` share
//! one `upsert` fn returning `HookOutcome::Handled`. Owning `pre_update` is the
//! difference from `MaintenanceWindowHandler`: the default update path prunes the
//! PUT body to `state_fields` (execution/operations/update.rs), which would drop
//! the API-required `id`/`customPayloadFields`. The PUT answers 200 (the config)
//! OR 202 (no body → `Value::Null`), so the handler returns the declared resource
//! (carrying the client id) with any object response overlaid — returning the raw
//! response would let a Null wipe the id in `merge_request_response`
//! (resolution.rs). Delete is schema-driven; discovery is `get_by_id`.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const ALERTS_PATH: &str = "/api/events/settings/alerts";

/// Fields copied from the declared resource into the AlertingConfiguration PUT
/// body — the full declared writable set incl. the client-supplied `id` (rides
/// the body per the API) and `customPayloadFields` (both would be dropped by the
/// default update-path prune). No server-derived read-model fields to exclude.
const UPSERT_FIELDS: &[&str] = &["id", "alertName", "integrationIds", "eventFilteringConfiguration", "customPayloadFields", "muteUntil", "includeEntityNameInLegacyAlerts"];

pub struct AlertHandler;

impl ResourceHandler for AlertHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { upsert(resource, client, operation_id).await })
    }

    fn pre_update<'a>(&'a self, _current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move { upsert(desired, client, operation_id).await })
    }
}

/// Shared PUT-upsert serving both `pre_create` and `pre_update`: build the body
/// from the declared writable fields and `PUT /api/events/settings/alerts/{id}`.
/// The alerts API has no POST and the default update path would prune the
/// API-required `id`/`customPayloadFields`, so a single handler owns both verbs.
async fn upsert(resource: &Value, client: &HttpClient, operation_id: &str) -> Result<HookOutcome> {
    let id = resource.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("instana_alert requires a client-supplied 'id' field"))?.to_string();
    let body = build_upsert_body(resource);
    let path = format!("{ALERTS_PATH}/{id}");
    let spec = RequestSpec::new(Method::PUT, &path).body(BodyKind::Json(Value::Object(body)));
    let response: Value = client.execute(operation_id, spec).await?;
    Ok(HookOutcome::Handled(merge_upsert_response(resource, &response)))
}

/// Build the AlertingConfiguration PUT body from the declared writable fields,
/// dropping any field not in `UPSERT_FIELDS`.
fn build_upsert_body(resource: &Value) -> Map<String, Value> {
    let mut body = Map::new();
    for &field in UPSERT_FIELDS {
        if let Some(v) = resource.get(field) {
            body.insert(field.to_string(), v.clone());
        }
    }
    body
}

/// Return the declared resource (which carries the client-supplied `id` + its
/// fields) with any object the upsert PUT returned overlaid. The PUT answers 200
/// with the config OR 202 with no body (→ `Value::Null`); using the resource as
/// the base guarantees the id survives even when the response is Null (which
/// `merge_request_response` would otherwise clone over the resource).
fn merge_upsert_response(resource: &Value, response: &Value) -> Value {
    let mut out = resource.clone();
    if let (Some(obj), Some(resp)) = (out.as_object_mut(), response.as_object()) {
        for (k, v) in resp {
            obj.insert(k.clone(), v.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_upsert_body_keeps_id_and_custom_payload_and_drops_unknown() {
        let resource = json!({"id": "a-1", "alertName": "route", "integrationIds": ["ch-1"], "eventFilteringConfiguration": {"ruleIds": ["r-1"]}, "customPayloadFields": [], "muteUntil": 0, "includeEntityNameInLegacyAlerts": true, "created": 123});
        let body = build_upsert_body(&resource);
        // The two fields the default update-path prune would drop MUST survive:
        assert_eq!(body.get("id").and_then(|v| v.as_str()), Some("a-1"));
        assert!(body.contains_key("customPayloadFields"), "customPayloadFields must ride the PUT body");
        assert_eq!(body.get("alertName").and_then(|v| v.as_str()), Some("route"));
        assert!(body.contains_key("integrationIds"));
        assert!(body.contains_key("eventFilteringConfiguration"));
        assert!(!body.contains_key("created"), "server field outside UPSERT_FIELDS must not ride the PUT body");
    }

    #[test]
    fn merge_upsert_response_null_preserves_client_id() {
        // A 202 no-content PUT deserializes to Value::Null; the client id must survive.
        let merged = merge_upsert_response(&json!({"id": "a-1", "alertName": "route"}), &Value::Null);
        assert_eq!(merged.get("id").and_then(|v| v.as_str()), Some("a-1"));
        assert_eq!(merged.get("alertName").and_then(|v| v.as_str()), Some("route"));
    }

    #[test]
    fn merge_upsert_response_object_overlays_server_fields() {
        let merged = merge_upsert_response(&json!({"id": "a-1", "alertName": "route"}), &json!({"id": "a-1", "created": 123}));
        assert_eq!(merged.get("id").and_then(|v| v.as_str()), Some("a-1"));
        assert_eq!(merged.get("created").and_then(|v| v.as_i64()), Some(123));
        assert_eq!(merged.get("alertName").and_then(|v| v.as_str()), Some("route"));
    }
}
