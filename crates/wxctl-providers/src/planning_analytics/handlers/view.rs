//! `pa_view` handler — a TM1 cube view is an abstract OData type with two concrete subtypes
//! (`NativeView`, `MDXView`); the create body MUST carry an `@odata.type` discriminator to pick
//! one, and a native view's axes carry dotted `Subset@odata.bind` keys — neither is expressible
//! as a declared `api_field`, and the default materializer would drop them
//! (docs/troubleshoot/pre-create-body-reshape-dropped-fix.md). So this handler OWNS the create
//! POST: it injects `@odata.type` from the declared `view_type` and passes the axes through
//! verbatim. The parent cube is a Path segment (`/Cubes('{cube}')/Views`), so no `Cube@odata.bind`
//! is needed. Axis subset binds are config-supplied; their exact form is live-verified in Phase 4.
//! This handler-owned POST bypasses the default materializer's Path-field interpolation, so it
//! sets the `cube` `path_var` itself before calling `client.execute` (see
//! docs/troubleshoot/nested-api-field-not-materialized-fix.md for the sibling materializer gap).

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct ViewHandler;

impl ResourceHandler for ViewHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = build_view_create_body(resource)?;
            let cube = resource.get("cube").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_view requires a 'cube' field"))?;
            let spec = RequestSpec::new(Method::POST, endpoint).path_var("cube", cube).body(BodyKind::Json(body));
            let response: Value = client.execute(operation_id, spec).await?;
            Ok(HookOutcome::Handled(response))
        })
    }
}

/// Build the `POST /Cubes('{cube}')/Views` body. `view_type` selects the concrete OData type:
/// `native` -> `#ibm.tm1.api.v1.NativeView` with `Columns`/`Rows`/`Titles` axes passed through
/// verbatim; `mdx` -> `#ibm.tm1.api.v1.MDXView` with the `MDX` string. The parent cube rides the
/// path, not the body.
fn build_view_create_body(resource: &Value) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_view requires a 'name' field"))?;
    let view_type = resource.get("view_type").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_view requires a 'view_type' of 'native' or 'mdx'"))?;
    let mut body = Map::new();
    body.insert("Name".to_string(), json!(name));
    match view_type {
        "mdx" => {
            body.insert("@odata.type".to_string(), json!("#ibm.tm1.api.v1.MDXView"));
            if let Some(mdx) = resource.get("mdx").and_then(|v| v.as_str()) {
                body.insert("MDX".to_string(), json!(mdx));
            }
        }
        "native" => {
            body.insert("@odata.type".to_string(), json!("#ibm.tm1.api.v1.NativeView"));
            for (field, key) in [("columns", "Columns"), ("rows", "Rows"), ("titles", "Titles")] {
                if let Some(axis) = resource.get(field).filter(|v| v.is_array()) {
                    body.insert(key.to_string(), axis.clone());
                }
            }
        }
        other => return Err(anyhow!("pa_view 'view_type' must be 'native' or 'mdx', got '{other}'")),
    }
    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure-function unit tests of the body-builder (no I/O).
    #[test]
    fn build_mdx_view_sets_discriminator_and_mdx() {
        let resource = json!({"name": "HighMargin", "view_type": "mdx", "mdx": "SELECT {} ON 0 FROM [Sales]"});
        let body = build_view_create_body(&resource).expect("body");
        assert_eq!(body.get("@odata.type").and_then(|v| v.as_str()), Some("#ibm.tm1.api.v1.MDXView"));
        assert_eq!(body.get("MDX").and_then(|v| v.as_str()), Some("SELECT {} ON 0 FROM [Sales]"));
        assert!(!body.as_object().unwrap().contains_key("Columns"));
    }

    #[test]
    fn build_native_view_sets_discriminator_and_passes_axes() {
        let resource = json!({"name": "ByRegion", "view_type": "native", "columns": [{"Subset@odata.bind": "Dimensions('Region')/Hierarchies('Region')/Subsets('All')"}]});
        let body = build_view_create_body(&resource).expect("body");
        assert_eq!(body.get("@odata.type").and_then(|v| v.as_str()), Some("#ibm.tm1.api.v1.NativeView"));
        assert!(body.get("Columns").and_then(|v| v.as_array()).is_some());
        assert!(!body.as_object().unwrap().contains_key("MDX"));
    }

    #[test]
    fn build_view_rejects_unknown_type() {
        assert!(build_view_create_body(&json!({"name": "X", "view_type": "pivot"})).is_err());
    }
}
