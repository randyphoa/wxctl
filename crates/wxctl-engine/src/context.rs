use dashmap::DashMap;
use serde_json::Value;
use wxctl_core::ResourceKey;

/// Resolve a field value from a JSON object, checking top-level,
/// entity-nested (CP4D), and metadata-nested locations.
fn resolve_field(data: &Value, field: &str) -> Option<String> {
    // Try top-level field first
    if let Some(value) = data.get(field).and_then(|v| v.as_str()) {
        return Some(value.to_string());
    }

    // Try entity-nested field (CP4D/watsonx.data structure)
    if let Some(entity) = data.get("entity")
        && let Some(value) = entity.get(field).and_then(|v| v.as_str())
    {
        return Some(value.to_string());
    }

    // Also try metadata-nested field
    if let Some(metadata) = data.get("metadata")
        && let Some(value) = metadata.get(field).and_then(|v| v.as_str())
    {
        return Some(value.to_string());
    }

    None
}

/// Runtime resource store: full resource data collected during reconciliation and
/// reused at execution for field-specific (`${kind.ref.field}`) and `__ref__*`
/// resolution. One `DashMap` shared by the reconcile pass and the executor — the
/// reconcile pass populates it via `insert`, destroy seeds the executor from it.
#[derive(Clone)]
pub struct RuntimeIdStore {
    resources: DashMap<ResourceKey, Value>,
}

impl Default for RuntimeIdStore {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeIdStore {
    pub fn new() -> Self {
        Self { resources: DashMap::new() }
    }

    /// Store full resource data
    pub fn insert(&self, key: ResourceKey, data: Value) {
        self.resources.insert(key, data);
    }

    /// Get full resource data
    pub fn get(&self, key: &ResourceKey) -> Option<Value> {
        self.resources.get(key).map(|data| data.clone())
    }

    /// Get a specific field value from the cached resource.
    /// Handles both top-level fields and CP4D/watsonx.data entity-nested fields.
    pub fn get_field(&self, key: &ResourceKey, field: &str) -> Option<String> {
        self.resources.get(key).and_then(|data| resolve_field(&data, field))
    }

    /// Check if a resource key exists in the store
    pub fn contains(&self, key: &ResourceKey) -> bool {
        self.resources.contains_key(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn store_insert_get_contains_and_field_tiers() {
        let store = RuntimeIdStore::new();

        // contains() flips from false to true once a key is inserted; get() returns
        // exactly what was stored.
        let conn = ResourceKey::new("connection", "my-conn");
        assert!(!store.contains(&conn));
        store.insert(conn.clone(), json!({"app_id": "app-123", "status": "active"}));
        assert!(store.contains(&conn));
        assert_eq!(store.get(&conn).unwrap(), json!({"app_id": "app-123", "status": "active"}));

        // get_field via resolve_field checks top-level, then entity-nested (CP4D),
        // then metadata-nested. Distinct field names force each fallback branch in turn.
        let key = ResourceKey::new("connection", "tiered");
        store.insert(
            key.clone(),
            json!({
                "top": "top-val",
                "entity": {"nested_field": "entity-val"},
                "metadata": {"meta_field": "meta-val"},
            }),
        );
        assert_eq!(store.get_field(&key, "top"), Some("top-val".into()));
        assert_eq!(store.get_field(&key, "nested_field"), Some("entity-val".into()));
        assert_eq!(store.get_field(&key, "meta_field"), Some("meta-val".into()));
    }
}
