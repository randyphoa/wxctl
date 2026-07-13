//! `maintenance_window` handler — Instana's maintenance window API has NO POST.
//! A window is created AND updated by the same idempotent upsert
//! `PUT /api/settings/v2/maintenance/{id}` with a CLIENT-SUPPLIED id (a declared
//! schema field, also the `get_by_id` discovery id_source). The default create
//! path only issues POST, so `MaintenanceWindowHandler` OWNS the create via
//! `pre_create` returning `HookOutcome::Handled`: it builds the
//! `MaintenanceConfigV2` body from the declared fields and PUTs it to `/{id}`.
//! The PUT answers 200 (the config) OR 202 (no body → `Value::Null`), so the
//! handler returns the declared resource (carrying the client id) with any
//! object response overlaid — returning the raw response would let a Null wipe
//! the id in `merge_request_response` (resolution.rs:190). Delete is
//! schema-driven; discovery is `get_by_id`.
//!
//! NOTE: update is intentionally NOT hooked. The default update path prunes the
//! PUT body to `state_fields` (update.rs:97), which would drop the required
//! `id`/`query`/`scheduling` — so an edited-then-re-applied window is a Phase-5
//! concern (add a `pre_update` mirroring this PUT, or document
//! modify-via-recreate). The AC lifecycle (apply → re-plan-no-op → destroy)
//! never fires an update.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const MAINTENANCE_PATH: &str = "/api/settings/v2/maintenance";

/// Fields copied from the declared resource into the `MaintenanceConfigV2` PUT
/// body. Excludes server-derived read-model fields (state, occurrence,
/// applicationNames, invalid, lastUpdated) that never ride the write body.
const UPSERT_FIELDS: &[&str] = &["id", "name", "query", "scheduling", "paused", "retriggerOpenAlertsEnabled", "tagFilterExpression", "tagFilterExpressionEnabled"];

pub struct MaintenanceWindowHandler;

impl ResourceHandler for MaintenanceWindowHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let id = resource.get("id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("instana_maintenance_window requires a client-supplied 'id' field"))?.to_string();
            let body = build_upsert_body(resource);
            let path = format!("{MAINTENANCE_PATH}/{id}");
            let spec = RequestSpec::new(Method::PUT, &path).body(BodyKind::Json(Value::Object(body)));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(merge_upsert_response(resource, &response)))
        })
    }
}

/// Build the `MaintenanceConfigV2` PUT body from the declared writable fields,
/// dropping any server-derived read-model field that isn't in `UPSERT_FIELDS`.
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
    fn build_upsert_body_copies_declared_fields_and_drops_read_model() {
        let resource = json!({"id": "mw-1", "name": "release", "query": "entity.type:host", "scheduling": {"start": 1, "duration": 60}, "state": "SCHEDULED", "occurrence": {}, "invalid": false});
        let body = build_upsert_body(&resource);
        assert_eq!(body.get("id").and_then(|v| v.as_str()), Some("mw-1"));
        assert_eq!(body.get("name").and_then(|v| v.as_str()), Some("release"));
        assert_eq!(body.get("query").and_then(|v| v.as_str()), Some("entity.type:host"));
        assert!(body.contains_key("scheduling"));
        assert!(!body.contains_key("state"), "server read-model field must not ride the PUT body");
        assert!(!body.contains_key("occurrence"));
        assert!(!body.contains_key("invalid"));
    }

    #[test]
    fn merge_upsert_response_null_preserves_client_id() {
        // A 202 no-content PUT deserializes to Value::Null; the client id must survive.
        let merged = merge_upsert_response(&json!({"id": "mw-1", "name": "release"}), &Value::Null);
        assert_eq!(merged.get("id").and_then(|v| v.as_str()), Some("mw-1"));
        assert_eq!(merged.get("name").and_then(|v| v.as_str()), Some("release"));
    }

    #[test]
    fn merge_upsert_response_object_overlays_server_fields() {
        let merged = merge_upsert_response(&json!({"id": "mw-1", "name": "release"}), &json!({"id": "mw-1", "state": "SCHEDULED", "lastUpdated": 123}));
        assert_eq!(merged.get("id").and_then(|v| v.as_str()), Some("mw-1"));
        assert_eq!(merged.get("state").and_then(|v| v.as_str()), Some("SCHEDULED"));
        assert_eq!(merged.get("name").and_then(|v| v.as_str()), Some("release"));
    }
}
