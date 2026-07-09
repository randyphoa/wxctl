use anyhow::{Context, Result, bail};
use serde_json::Value;

const MAX_REF_DEPTH: usize = 64;

/// Resolve all internal `$ref` pointers in an OpenAPI spec by inlining referenced definitions.
/// Only handles internal refs (`#/components/...`), not external file refs.
pub fn resolve_refs(spec: &Value) -> Result<Value> {
    let mut resolved = spec.clone();
    resolve_value(&mut resolved, spec, 0)?;
    Ok(resolved)
}

fn resolve_value(value: &mut Value, root: &Value, depth: usize) -> Result<()> {
    if depth > MAX_REF_DEPTH {
        bail!("$ref resolution exceeded maximum depth of {} (possible circular reference)", MAX_REF_DEPTH);
    }
    match value {
        Value::Object(map) => {
            if let Some(ref_val) = map.get("$ref").and_then(|v| v.as_str()).map(|s| s.to_string()) {
                let referenced = resolve_pointer(root, &ref_val).with_context(|| format!("Failed to resolve $ref: {}", ref_val))?;
                *value = referenced.clone();
                // Recursively resolve the inlined value (it may contain further $refs)
                resolve_value(value, root, depth + 1)?;
            } else {
                for (_key, val) in map.iter_mut() {
                    resolve_value(val, root, depth + 1)?;
                }
            }
        }
        Value::Array(arr) => {
            for item in arr.iter_mut() {
                resolve_value(item, root, depth + 1)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Resolve a JSON Pointer path like `#/components/schemas/Pet` against the root document.
fn resolve_pointer(root: &Value, ref_path: &str) -> Result<Value> {
    let path = ref_path.strip_prefix("#/").with_context(|| format!("Only internal $refs supported, got: {}", ref_path))?;

    let pointer = format!("/{}", path);
    root.pointer(&pointer).cloned().with_context(|| format!("$ref path not found: {}", ref_path))
}
