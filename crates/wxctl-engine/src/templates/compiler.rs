use super::parser::{extract_references, parse_reference_with_path};
use anyhow::{Result, anyhow};
use serde_json::Value;

/// Compiled template, validated at configuration load time to catch syntax errors early.
#[derive(Debug, Clone)]
pub struct CompiledTemplate {
    /// Original JSON value with template strings
    pub original: Value,
}

impl CompiledTemplate {
    /// Compile a JSON value, validating that every template string has valid syntax.
    pub fn compile(value: Value) -> Result<Self> {
        validate_templates(&value)?;
        Ok(CompiledTemplate { original: value })
    }

    /// Get all unique resource keys referenced by this template
    pub fn dependencies(&self) -> Vec<wxctl_core::ResourceKey> {
        let mut keys: Vec<_> = extract_references(&self.original).into_iter().map(|r| r.key).collect();
        keys.sort_by(|a, b| (&a.kind, &a.name).cmp(&(&b.kind, &b.name)));
        keys.dedup();
        keys
    }
}

/// Validate that all template strings in a value have valid syntax
fn validate_templates(value: &Value) -> Result<()> {
    validate_templates_recursive(value, &mut Vec::new())
}

fn validate_templates_recursive(value: &Value, path: &mut Vec<String>) -> Result<()> {
    match value {
        Value::String(s) if super::parser::is_template(s) => {
            if parse_reference_with_path(s).is_none() {
                let path_str = if path.is_empty() { "root".to_string() } else { path.join(".") };
                return Err(anyhow!("Invalid template syntax at {}: '{}'\nExpected format: ${{kind.name}} or ${{kind.name.field.subfield}}", path_str, s));
            }
        }
        Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                path.push(format!("[{}]", i));
                validate_templates_recursive(item, path)?;
                path.pop();
            }
        }
        Value::Object(obj) => {
            for (key, val) in obj {
                path.push(key.clone());
                validate_templates_recursive(val, path)?;
                path.pop();
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::ResourceKey;

    // ── CompiledTemplate::compile + dependencies (success paths) ──

    #[test]
    fn compile_and_dependencies_success_branches() {
        // Valid template → compiles, one dependency.
        let compiled = CompiledTemplate::compile(json!({"conn": "${connection.my-conn}"})).unwrap();
        let deps = compiled.dependencies();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], ResourceKey::new("connection", "my-conn"));

        // No templates → no dependencies.
        let compiled = CompiledTemplate::compile(json!({"field": "plain", "num": 42})).unwrap();
        assert!(compiled.dependencies().is_empty());

        // Repeated references to the same resource key deduplicate.
        let compiled = CompiledTemplate::compile(json!({"a": "${catalog.x}", "b": "${catalog.x}", "c": "${connection.y}"})).unwrap();
        assert_eq!(compiled.dependencies().len(), 2);

        // References with field paths like ${catalog.x.metadata.id} still
        // deduplicate by resource key (catalog.x).
        let compiled = CompiledTemplate::compile(json!({"a": "${catalog.x.metadata.id}", "b": "${catalog.x.entity.name}"})).unwrap();
        let deps = compiled.dependencies();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], ResourceKey::new("catalog", "x"));
    }

    // ── CompiledTemplate::compile (invalid-syntax error + path reporting) ──

    #[test]
    fn compile_invalid_template_error_branches() {
        // Invalid syntax → error mentioning "Invalid template syntax".
        let err = CompiledTemplate::compile(json!({"field": "${invalid}"})).unwrap_err().to_string();
        assert!(err.contains("Invalid template syntax"), "got: {err}");

        // Error path includes the array index for an offending element.
        let err = CompiledTemplate::compile(json!([{"field": "${invalid}"}])).unwrap_err().to_string();
        assert!(err.contains("[0]"), "Error should include array index, got: {err}");

        // Error path includes the nested object field name.
        let err = CompiledTemplate::compile(json!({"outer": {"inner": "${invalid}"}})).unwrap_err().to_string();
        assert!(err.contains("outer"), "Error should include field path, got: {err}");
    }
}
