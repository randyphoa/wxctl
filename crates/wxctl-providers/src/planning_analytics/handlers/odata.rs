//! Shared TM1 OData body-mapping helpers for handlers that own their create POSTs.
//! The default materializer applies `api_field` mappings for TOP-LEVEL declared
//! fields only — it does not recurse into object-array items — so `elements[]` /
//! `edges[]` must be mapped to their PascalCase wire keys by hand
//! (docs/troubleshoot/nested-api-field-not-materialized-fix.md). Used by both
//! `dimension` (inline `hierarchies[].elements/edges`) and `hierarchy` (standalone
//! `elements`/`edges`).

use anyhow::{Result, anyhow};
use serde_json::{Map, Value, json};

/// Map an `elements[]` array to nested PascalCase: `name` -> `Name` (required),
/// `element_type` -> `Type` (optional). `kind` names the resource in error messages.
pub(crate) fn build_elements_array(elements: &[Value], kind: &str) -> Result<Value> {
    let mut out = Vec::with_capacity(elements.len());
    for e in elements {
        let ename = e.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("{kind} 'elements' items require a 'name' field"))?;
        let mut eobj = Map::new();
        eobj.insert("Name".to_string(), json!(ename));
        if let Some(t) = e.get("element_type").and_then(|v| v.as_str()) {
            eobj.insert("Type".to_string(), json!(t));
        }
        out.push(Value::Object(eobj));
    }
    Ok(Value::Array(out))
}

/// Map an `edges[]` array to nested PascalCase: `parent_name` -> `ParentName` (required),
/// `component_name` -> `ComponentName` (required), `weight` -> `Weight` (optional).
pub(crate) fn build_edges_array(edges: &[Value], kind: &str) -> Result<Value> {
    let mut out = Vec::with_capacity(edges.len());
    for e in edges {
        let parent = e.get("parent_name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("{kind} 'edges' items require a 'parent_name' field"))?;
        let component = e.get("component_name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("{kind} 'edges' items require a 'component_name' field"))?;
        let mut eobj = Map::new();
        eobj.insert("ParentName".to_string(), json!(parent));
        eobj.insert("ComponentName".to_string(), json!(component));
        if let Some(w) = e.get("weight")
            && !w.is_null()
        {
            eobj.insert("Weight".to_string(), w.clone());
        }
        out.push(Value::Object(eobj));
    }
    Ok(Value::Array(out))
}
