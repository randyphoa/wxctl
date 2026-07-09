//! `pa_dimension` handler — the default materializer applies `api_field` mappings for
//! TOP-LEVEL declared fields only; it does NOT recurse into object-array items, so nested
//! `api_field` mappings inside `hierarchies[].elements[]`/`hierarchies[].edges[]` never reach
//! the wire (docs/troubleshoot/nested-api-field-not-materialized-fix.md). Live-proven: a
//! default-materialized `POST /Dimensions` sent nested snake_case keys (`name`, `elements`,
//! `edges`) and TM1 rejected it with error 278 "Missing Hierarchy name."; the same body with
//! nested keys mapped to PascalCase (`Name`/`Elements[{Name,Type}]`/
//! `Edges[{ParentName,ComponentName,Weight}]`) returned 201. So this handler OWNS the create
//! POST and builds the fully nested PascalCase body itself.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::odata::{build_edges_array, build_elements_array};

pub struct DimensionHandler;

impl ResourceHandler for DimensionHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = build_dimension_create_body(resource)?;
            let spec = RequestSpec::new(Method::POST, endpoint).body(BodyKind::Json(body));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }
}

/// Build the `POST /Dimensions` body, mapping every nested key to its TM1 PascalCase property
/// name (the default materializer only maps top-level `api_field`s). `hierarchies[]` ->
/// `Hierarchies[{Name, Elements[{Name,Type}], Edges[{ParentName,ComponentName,Weight}]}]`.
fn build_dimension_create_body(resource: &Value) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_dimension requires a 'name' field"))?;
    let mut body = Map::new();
    body.insert("Name".to_string(), json!(name));
    if let Some(hierarchies) = resource.get("hierarchies").and_then(|v| v.as_array()) {
        let mut out = Vec::with_capacity(hierarchies.len());
        for h in hierarchies {
            out.push(build_hierarchy_object(h, "pa_dimension")?);
        }
        body.insert("Hierarchies".to_string(), Value::Array(out));
    }
    Ok(Value::Object(body))
}

/// Build one nested `Hierarchies[]` item (dimension-inline shape): requires `name` -> `Name`,
/// optional `elements`/`edges` mapped via the shared `super::odata` helpers (also used by
/// `pa_hierarchy`'s standalone body builder).
fn build_hierarchy_object(item: &Value, kind: &str) -> Result<Value> {
    let name = item.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("{kind} 'hierarchies' items require a 'name' field"))?;
    let mut obj = Map::new();
    obj.insert("Name".to_string(), json!(name));
    if let Some(elements) = item.get("elements").and_then(|v| v.as_array()) {
        obj.insert("Elements".to_string(), build_elements_array(elements, kind)?);
    }
    if let Some(edges) = item.get("edges").and_then(|v| v.as_array()) {
        obj.insert("Edges".to_string(), build_edges_array(edges, kind)?);
    }
    Ok(Value::Object(obj))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure-function unit tests of the body-builder (no I/O) — matches the co-located test
    // convention of cube.rs/user.rs.
    #[test]
    fn build_dimension_body_maps_nested_keys_to_pascal_case() {
        let resource = json!({
            "name": "Region",
            "hierarchies": [{
                "name": "Region",
                "elements": [{"name": "North", "element_type": "Numeric"}, {"name": "Total", "element_type": "Consolidated"}],
                "edges": [{"parent_name": "Total", "component_name": "North", "weight": 1}]
            }]
        });
        let body = build_dimension_create_body(&resource).expect("body");
        assert_eq!(body.get("Name").and_then(|v| v.as_str()), Some("Region"));

        let hierarchies = body.get("Hierarchies").and_then(|v| v.as_array()).expect("Hierarchies array");
        assert_eq!(hierarchies.len(), 1);
        let h0 = &hierarchies[0];
        assert_eq!(h0.get("Name").and_then(|v| v.as_str()), Some("Region"));

        let elements = h0.get("Elements").and_then(|v| v.as_array()).expect("Elements array");
        assert_eq!(elements[0].get("Name").and_then(|v| v.as_str()), Some("North"));
        assert_eq!(elements[0].get("Type").and_then(|v| v.as_str()), Some("Numeric"));

        let edges = h0.get("Edges").and_then(|v| v.as_array()).expect("Edges array");
        assert_eq!(edges[0].get("ParentName").and_then(|v| v.as_str()), Some("Total"));
        assert_eq!(edges[0].get("ComponentName").and_then(|v| v.as_str()), Some("North"));
        assert_eq!(edges[0].get("Weight"), Some(&json!(1)));

        // No snake_case nested keys anywhere in the emitted body — the exact bug that produced
        // TM1 error 278 "Missing Hierarchy name."
        let s = serde_json::to_string(&body).unwrap();
        assert!(!s.contains("\"name\""));
        assert!(!s.contains("\"elements\""));
        assert!(!s.contains("\"edges\""));
        assert!(!s.contains("\"element_type\""));
        assert!(!s.contains("\"parent_name\""));
        assert!(!s.contains("\"component_name\""));
        assert!(!s.contains("\"weight\""));
    }

    #[test]
    fn build_dimension_body_requires_name() {
        assert!(build_dimension_create_body(&json!({"hierarchies": []})).is_err());
    }

    #[test]
    fn build_dimension_body_without_hierarchies_is_name_only() {
        let body = build_dimension_create_body(&json!({"name": "Product"})).expect("body");
        assert_eq!(body, json!({"Name": "Product"}));
    }
}
