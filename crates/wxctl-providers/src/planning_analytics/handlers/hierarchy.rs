//! `pa_hierarchy` handler — same gap as `pa_dimension` (`handlers::dimension`): the default
//! materializer applies `api_field` mappings for TOP-LEVEL declared fields only and does NOT
//! recurse into object-array items, so nested `api_field` mappings inside `elements[]`/`edges[]`
//! never reach the wire (docs/troubleshoot/nested-api-field-not-materialized-fix.md; live-proven
//! against TM1 error 278 "Missing Hierarchy name." — see `handlers::dimension`'s doc comment for
//! the exact evidence). So this handler OWNS the create POST and builds the nested PascalCase
//! body itself. `dimension` is a `location: Path` field — it rides the create endpoint's
//! `{dimension}` segment, never the body. Because this is a handler-owned (`Handled`) POST, it
//! bypasses the default materializer's Path-field interpolation entirely, so the handler sets the
//! `dimension` `path_var` itself before calling `client.execute` (see
//! docs/troubleshoot/nested-api-field-not-materialized-fix.md for the sibling materializer gap).

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::odata::{build_edges_array, build_elements_array};

pub struct HierarchyHandler;

impl ResourceHandler for HierarchyHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = build_hierarchy_create_body(resource)?;
            let dimension = resource.get("dimension").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_hierarchy requires a 'dimension' field"))?;
            let spec = RequestSpec::new(Method::POST, endpoint).path_var("dimension", dimension).body(BodyKind::Json(body));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }
}

/// Build the `POST /Dimensions('{dimension}')/Hierarchies` body: `name` -> `Name` (required),
/// optional `elements` -> `Elements[{Name,Type}]`, optional `edges` ->
/// `Edges[{ParentName,ComponentName,Weight}]`. `dimension` is excluded — it rides the endpoint
/// path, not the body.
fn build_hierarchy_create_body(resource: &Value) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_hierarchy requires a 'name' field"))?;
    let mut body = Map::new();
    body.insert("Name".to_string(), json!(name));
    if let Some(elements) = resource.get("elements").and_then(|v| v.as_array()) {
        body.insert("Elements".to_string(), build_elements_array(elements, "pa_hierarchy")?);
    }
    if let Some(edges) = resource.get("edges").and_then(|v| v.as_array()) {
        body.insert("Edges".to_string(), build_edges_array(edges, "pa_hierarchy")?);
    }
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure-function unit tests of the body-builder (no I/O) — matches the co-located test
    // convention of cube.rs/user.rs/dimension.rs.
    #[test]
    fn build_hierarchy_body_maps_nested_keys_and_omits_dimension() {
        let resource = json!({
            "name": "RegionAlt",
            "dimension": "Region",
            "elements": [{"name": "North", "element_type": "Numeric"}, {"name": "Total", "element_type": "Consolidated"}],
            "edges": [{"parent_name": "Total", "component_name": "North", "weight": 1}]
        });
        let body = build_hierarchy_create_body(&resource).expect("body");
        assert_eq!(body.get("Name").and_then(|v| v.as_str()), Some("RegionAlt"));

        let elements = body.get("Elements").and_then(|v| v.as_array()).expect("Elements array");
        assert_eq!(elements[0].get("Name").and_then(|v| v.as_str()), Some("North"));
        assert_eq!(elements[0].get("Type").and_then(|v| v.as_str()), Some("Numeric"));

        let edges = body.get("Edges").and_then(|v| v.as_array()).expect("Edges array");
        assert_eq!(edges[0].get("ParentName").and_then(|v| v.as_str()), Some("Total"));
        assert_eq!(edges[0].get("ComponentName").and_then(|v| v.as_str()), Some("North"));
        assert_eq!(edges[0].get("Weight"), Some(&json!(1)));

        // `dimension` rides the endpoint path (location: Path), never the body.
        assert!(!body.as_object().unwrap().contains_key("dimension"));
        assert!(!body.as_object().unwrap().contains_key("Dimension"));

        // No snake_case nested keys anywhere in the emitted body.
        let s = serde_json::to_string(&body).unwrap();
        assert!(!s.contains("\"element_type\""));
        assert!(!s.contains("\"parent_name\""));
        assert!(!s.contains("\"component_name\""));
        assert!(!s.contains("\"weight\""));
    }

    #[test]
    fn build_hierarchy_body_requires_name() {
        assert!(build_hierarchy_create_body(&json!({"dimension": "Region"})).is_err());
    }
}
