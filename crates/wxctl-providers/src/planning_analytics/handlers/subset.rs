//! `pa_subset` handler — a static TM1 subset binds its members via the OData
//! `Elements@odata.bind` key (dotted -> inexpressible as a declared `api_field`, dropped by the
//! default materializer: docs/troubleshoot/pre-create-body-reshape-dropped-fix.md), so for a
//! static subset (explicit `elements`) this handler OWNS the create POST and builds the bind
//! body. An MDX subset (an `expression`, no `elements`) has no dotted key -> `Continue`, and the
//! default materializer POSTs `Name` + `Expression`. The parent dimension+hierarchy ride the
//! path (`/Dimensions('{dimension}')/Hierarchies('{hierarchy}')/Subsets`), not the body. The
//! static (`Handled`) branch is a handler-owned POST that bypasses the default materializer's
//! Path-field interpolation, so it sets the `dimension`/`hierarchy` `path_var`s itself before
//! calling `client.execute` — the MDX (`Continue`) branch goes through the materializer and needs
//! nothing extra (see docs/troubleshoot/nested-api-field-not-materialized-fix.md for the sibling
//! materializer gap).

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct SubsetHandler;

impl ResourceHandler for SubsetHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // MDX subset (no static elements): the default materializer handles Name + Expression.
            if !has_static_elements(resource) {
                return Ok(HookOutcome::Continue);
            }
            let body = build_static_subset_body(resource)?;
            let dimension = resource.get("dimension").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_subset requires a 'dimension' field"))?;
            let hierarchy = resource.get("hierarchy").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_subset requires a 'hierarchy' field"))?;
            let spec = RequestSpec::new(Method::POST, endpoint).path_var("dimension", dimension).path_var("hierarchy", hierarchy).body(BodyKind::Json(body));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }
}

/// True when the subset declares a non-empty static `elements` list (vs a dynamic MDX expression).
fn has_static_elements(resource: &Value) -> bool {
    resource.get("elements").and_then(|v| v.as_array()).is_some_and(|a| !a.is_empty())
}

/// Build the static-subset `POST` body: `Name` + `Elements@odata.bind`, each element bound as
/// `Dimensions('{dimension}')/Hierarchies('{hierarchy}')/Elements('{element}')`. Names exclude `'`
/// at schema validation, so no OData escaping is needed here.
fn build_static_subset_body(resource: &Value) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_subset requires a 'name' field"))?;
    let dimension = resource.get("dimension").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_subset requires a 'dimension' field"))?;
    let hierarchy = resource.get("hierarchy").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_subset requires a 'hierarchy' field"))?;
    let elements = resource.get("elements").and_then(|v| v.as_array()).ok_or_else(|| anyhow!("pa_subset static build requires an 'elements' array"))?;
    let binds: Vec<Value> = elements.iter().filter_map(|e| e.as_str()).map(|e| Value::String(format!("Dimensions('{dimension}')/Hierarchies('{hierarchy}')/Elements('{e}')"))).collect();
    if binds.len() != elements.len() {
        return Err(anyhow!("pa_subset 'elements' must be an array of element-name strings"));
    }
    let mut body = Map::new();
    body.insert("Name".to_string(), json!(name));
    body.insert("Elements@odata.bind".to_string(), Value::Array(binds));
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure-function unit tests of the body-builder (no I/O) — matches the co-located test
    // convention of every existing handler (e.g. planning_analytics/handlers/cube.rs).
    #[test]
    fn static_subset_binds_elements() {
        let resource = json!({"name": "Top", "dimension": "Region", "hierarchy": "Region", "elements": ["North", "South"]});
        assert!(has_static_elements(&resource));
        let body = build_static_subset_body(&resource).expect("body");
        assert_eq!(body.get("Name").and_then(|v| v.as_str()), Some("Top"));
        assert_eq!(body.get("Elements@odata.bind").unwrap(), &json!(["Dimensions('Region')/Hierarchies('Region')/Elements('North')", "Dimensions('Region')/Hierarchies('Region')/Elements('South')"]));
    }

    #[test]
    fn mdx_or_empty_subset_is_not_static() {
        assert!(!has_static_elements(&json!({"name": "Dyn", "dimension": "Region", "hierarchy": "Region", "expression": "{TM1SUBSETALL([Region])}"})));
        assert!(!has_static_elements(&json!({"name": "Empty", "elements": []})));
    }
}
