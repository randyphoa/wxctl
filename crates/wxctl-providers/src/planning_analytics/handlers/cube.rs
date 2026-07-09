//! `pa_cube` handler — a TM1 cube's dimension list is created via the OData
//! `Dimensions@odata.bind` navigation-bind key, whose dot makes it inexpressible as a
//! declared schema field's `api_field`; the default materializer iterates declared fields
//! only and would drop it (docs/troubleshoot/pre-create-body-reshape-dropped-fix.md). So
//! this handler OWNS the create POST: it builds the bind body from the declared `dimensions`
//! name array and returns `HookOutcome::Handled`. Non-identity edits (rules,
//! drillthrough_rules) are declared fields and reconcile through the default PATCH;
//! `dimensions` is immutable (drift -> recreate).
//!
//! `dimensions` phantom recreate (live 2026-07-03; `docs/troubleshoot/pa-live-gateway-quirks.md`):
//! the default `GET /Cubes('{name}')` response carries no `Dimensions` key at all (only
//! Name/Rules/DrillthroughRules/LastSchemaUpdate/LastDataUpdate/Attributes), and the schema
//! field has no `api_field` (see below), so a discovered cube's `dimensions` was always
//! `Value::Null`. `dimensions` is in `immutable_fields` (not `state_fields`), and
//! `SchemaBasedReconciler::compare` (`wxctl-engine/src/reconciliation/schema_reconciler.rs`)
//! diffs the local declared array against that `Null` with no null-equivalence exemption for a
//! non-empty array — every apply with a literal `dimensions` list showed a phantom `+- recreate`.
//! `post_discover` fixes this by GETting `/Cubes('{name}')/Dimensions?$select=Name` and hoisting
//! the returned names into the discovered `dimensions` key, so a config whose declared list
//! matches the server's actual dimension set compares equal.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct CubeHandler;

impl ResourceHandler for CubeHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = build_cube_create_body(resource)?;
            let spec = RequestSpec::new(Method::POST, endpoint).body(BodyKind::Json(body));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let Some(name) = remote_data.get("name").and_then(|v| v.as_str()).map(str::to_string) else {
                return Ok(());
            };
            let endpoint = format!("/Cubes('{name}')/Dimensions?$select=Name");
            match client.get::<Value>(operation_id, &endpoint).await {
                Ok(response) => {
                    if let Some(names) = extract_dimension_names(&response)
                        && let Some(obj) = remote_data.as_object_mut()
                    {
                        obj.insert("dimensions".to_string(), Value::Array(names.into_iter().map(Value::String).collect()));
                    }
                }
                Err(e) => {
                    // Best-effort: leave `dimensions` unset on failure rather than fail
                    // discovery outright — the cube itself was already found, and a missed
                    // hoist just falls back to the pre-fix phantom-recreate behavior instead
                    // of blocking the whole reconciliation on a secondary GET.
                    tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "pa_cube", cube = %name, error = %e, "failed to fetch cube dimensions; dimensions drift comparison may show a phantom recreate");
                }
            }
            Ok(())
        })
    }
}

/// Extract dimension names from the `{"value":[{"Name":"X"}, ...]}` OData envelope returned by
/// `GET /Cubes('{name}')/Dimensions?$select=Name`. `None` when `value` isn't that shape.
fn extract_dimension_names(response: &Value) -> Option<Vec<String>> {
    let items = response.get("value")?.as_array()?;
    Some(items.iter().filter_map(|item| item.get("Name").and_then(|v| v.as_str()).map(str::to_string)).collect())
}

/// Build the `POST /Cubes` body from the declared cube resource. The `dimensions` name array
/// becomes `Dimensions@odata.bind: ["Dimensions('<name>')", ...]`; `rules`/`drillthrough_rules`
/// ride their PascalCase keys when present. Names are validated to exclude `'` at schema
/// validation, so no OData escaping is needed here.
fn build_cube_create_body(resource: &Value) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_cube requires a 'name' field"))?;
    let dimensions = resource.get("dimensions").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("pa_cube requires a 'dimensions' array"))?;
    let binds: Vec<Value> = dimensions.iter().filter_map(|d| d.as_str()).map(|d| Value::String(format!("Dimensions('{d}')"))).collect();
    if binds.len() != dimensions.len() {
        return Err(anyhow!("pa_cube 'dimensions' must be an array of dimension-name strings"));
    }
    let mut body = Map::new();
    body.insert("Name".to_string(), json!(name));
    body.insert("Dimensions@odata.bind".to_string(), Value::Array(binds));
    if let Some(rules) = resource.get("rules").and_then(|v| v.as_str()) {
        body.insert("Rules".to_string(), json!(rules));
    }
    if let Some(dt) = resource.get("drillthrough_rules").and_then(|v| v.as_str()) {
        body.insert("DrillthroughRules".to_string(), json!(dt));
    }
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure-function unit test of the body-builder (no I/O, no runtime behavior) — matches the
    // co-located test convention of every existing handler (e.g. concert/handlers/source_repo.rs).
    #[test]
    fn build_cube_body_binds_dimensions_and_carries_rules() {
        let resource = json!({"name": "Sales", "dimensions": ["Region", "Product", "Month"], "rules": "SKIPCHECK;"});
        let body = build_cube_create_body(&resource).expect("body");
        assert_eq!(body.get("Name").and_then(|v| v.as_str()), Some("Sales"));
        assert_eq!(body.get("Dimensions@odata.bind").unwrap(), &json!(["Dimensions('Region')", "Dimensions('Product')", "Dimensions('Month')"]));
        assert_eq!(body.get("Rules").and_then(|v| v.as_str()), Some("SKIPCHECK;"));
        assert!(!body.as_object().unwrap().contains_key("DrillthroughRules"));
    }

    #[test]
    fn build_cube_body_requires_dimensions() {
        assert!(build_cube_create_body(&json!({"name": "X"})).is_err());
    }

    // Pure-function unit test of the dimensions-hoist envelope parser (no I/O) — matches the
    // GET /Cubes('{name}')/Dimensions?$select=Name response shape.
    #[test]
    fn extract_dimension_names_reads_value_envelope() {
        let response = json!({"value": [{"Name": "wxctlRegion"}, {"Name": "wxctlMeasures"}]});
        assert_eq!(extract_dimension_names(&response), Some(vec!["wxctlRegion".to_string(), "wxctlMeasures".to_string()]));
    }

    #[test]
    fn extract_dimension_names_none_when_not_envelope_shaped() {
        assert_eq!(extract_dimension_names(&json!({"Name": "not-a-list"})), None);
        assert_eq!(extract_dimension_names(&json!(null)), None);
    }
}
