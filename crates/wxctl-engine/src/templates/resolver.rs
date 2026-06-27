use super::compiler::CompiledTemplate;
use super::parser::{is_template, parse_reference_with_path};
use crate::context::RuntimeIdStore;
use anyhow::{Result, anyhow};
use serde_json::Value;

/// Resolves template references using runtime ID store
pub struct TemplateResolver<'a> {
    store: &'a RuntimeIdStore,
    recursion_depth: usize,
    max_depth: usize,
}

impl<'a> TemplateResolver<'a> {
    /// Create new resolver with runtime ID store
    pub fn new(store: &'a RuntimeIdStore) -> Self {
        Self { store, recursion_depth: 0, max_depth: 100 }
    }

    /// Resolve all template references in compiled template
    /// Returns error if any reference cannot be resolved
    pub fn resolve(&mut self, template: &CompiledTemplate) -> Result<Value> {
        self.resolve_value(&template.original)
    }

    /// Resolve template references in a JSON value
    pub fn resolve_value(&mut self, value: &Value) -> Result<Value> {
        // Prevent infinite recursion
        if self.recursion_depth >= self.max_depth {
            return Err(anyhow!("Maximum recursion depth exceeded while resolving templates"));
        }

        self.recursion_depth += 1;
        let result = self.resolve_value_impl(value);
        self.recursion_depth -= 1;

        result
    }

    fn resolve_value_impl(&mut self, value: &Value) -> Result<Value> {
        match value {
            Value::String(s) if is_template(s) => self.resolve_reference(s),
            Value::Array(arr) => {
                let resolved: Result<Vec<_>> = arr.iter().map(|v| self.resolve_value(v)).collect();
                Ok(Value::Array(resolved?))
            }
            Value::Object(obj) => {
                let mut resolved = serde_json::Map::new();
                for (key, val) in obj {
                    resolved.insert(key.clone(), self.resolve_value(val)?);
                }
                Ok(Value::Object(resolved))
            }
            _ => Ok(value.clone()),
        }
    }

    fn resolve_reference(&self, template: &str) -> Result<Value> {
        let reference = parse_reference_with_path(template).ok_or_else(|| anyhow!("Invalid template syntax: {}", template))?;

        // Get resource data from store
        let resource_data = self.store.get(&reference.key).ok_or_else(|| anyhow!("Template reference not found: {}\nResource '{}' of kind '{}' has not been created yet", template, reference.key.name, reference.key.kind))?;

        // Navigate nested field path if specified
        let mut current = &resource_data;
        for field in &reference.field_path {
            // Convert IStr to &str for serde_json::Index
            current = current.get(field.as_ref()).ok_or_else(|| anyhow!("Field '{}' not found in template reference: {}", field, template))?;
        }

        Ok(current.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::ResourceKey;

    fn make_store_with(entries: Vec<(&str, &str, Value)>) -> RuntimeIdStore {
        let store = RuntimeIdStore::new();
        for (kind, name, data) in entries {
            store.insert(ResourceKey::new(kind, name), data);
        }
        store
    }

    fn compile(value: Value) -> CompiledTemplate {
        CompiledTemplate::compile(value).unwrap()
    }

    fn resolve(store: &RuntimeIdStore, value: Value) -> Result<Value> {
        let template = compile(value);
        TemplateResolver::new(store).resolve(&template)
    }

    #[test]
    fn resolve_reference_success_branches() {
        // Whole-resource reference returns the full cached object.
        let store = make_store_with(vec![("catalog", "x", json!({"id": "cat-123", "name": "my-catalog"}))]);
        assert_eq!(resolve(&store, json!("${catalog.x}")).unwrap(), json!({"id": "cat-123", "name": "my-catalog"}));

        // Nested field path navigates into the cached object.
        let store = make_store_with(vec![("catalog", "x", json!({"metadata": {"id": "meta-id-123"}}))]);
        assert_eq!(resolve(&store, json!("${catalog.x.metadata.id}")).unwrap(), json!("meta-id-123"));

        // Non-template values pass through unchanged (no resource lookup).
        let store = RuntimeIdStore::new();
        assert_eq!(resolve(&store, json!({"num": 42, "flag": true, "nested": {"key": "plain"}})).unwrap(), json!({"num": 42, "flag": true, "nested": {"key": "plain"}}));

        // Templates inside arrays and nested objects are resolved recursively.
        let store = make_store_with(vec![("catalog", "a", json!("resolved-a")), ("connection", "b", json!("resolved-b"))]);
        let result = resolve(&store, json!({"items": ["${catalog.a}", "${connection.b}"], "nested": {"ref": "${catalog.a}"}})).unwrap();
        assert_eq!(result["items"][0], json!("resolved-a"));
        assert_eq!(result["items"][1], json!("resolved-b"));
        assert_eq!(result["nested"]["ref"], json!("resolved-a"));

        // Toolkit tool resolved by name (map lookup under `tools`).
        let store = make_store_with(vec![("toolkit", "hello_toolkit", json!({"id": "tk-123", "name": "hello_toolkit", "tools": {"hello": "tool-uuid-1", "goodbye": "tool-uuid-2"}}))]);
        assert_eq!(resolve(&store, json!("${toolkit.hello_toolkit.tools.hello}")).unwrap(), json!("tool-uuid-1"));

        // Agent `tools` array referencing toolkit tool uuids.
        let store = make_store_with(vec![("toolkit", "my_tk", json!({"id": "tk-1", "tools": {"search": "tool-aaa", "chat": "tool-bbb"}}))]);
        let result = resolve(&store, json!({"tools": ["${toolkit.my_tk.tools.search}", "${toolkit.my_tk.tools.chat}"]})).unwrap();
        assert_eq!(result["tools"][0], json!("tool-aaa"));
        assert_eq!(result["tools"][1], json!("tool-bbb"));
    }

    #[test]
    fn resolve_reference_error_branches() {
        // Missing resource → error.
        let store = RuntimeIdStore::new();
        assert!(resolve(&store, json!("${catalog.missing}")).unwrap_err().to_string().contains("not found"));

        // Resource present but the nested field path is missing → error.
        let store = make_store_with(vec![("catalog", "x", json!({"id": "cat-123"}))]);
        assert!(resolve(&store, json!("${catalog.x.metadata.id}")).unwrap_err().to_string().contains("not found"));

        // Toolkit present but the named tool is absent → error.
        let store = make_store_with(vec![("toolkit", "hello_toolkit", json!({"id": "tk-123", "name": "hello_toolkit", "tools": {"hello": "tool-uuid-1"}}))]);
        assert!(resolve(&store, json!("${toolkit.hello_toolkit.tools.nonexistent}")).unwrap_err().to_string().contains("not found"));
    }
}
