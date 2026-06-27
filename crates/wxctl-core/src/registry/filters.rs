use super::FieldDescriptor;
use crate::schema::FieldLocation;
use anyhow::Result;
use serde_json::Value;
use std::collections::HashMap;

/// Extract query parameter fields from data
///
/// Returns a HashMap of field names and their string values for fields with Query location
pub fn extract_query_params(data: &Value, fields: &[FieldDescriptor]) -> HashMap<String, String> {
    let mut params = HashMap::new();

    if let Some(obj) = data.as_object() {
        for field in fields {
            if field.location == FieldLocation::Query
                && let Some(value) = obj.get(&field.name).and_then(|v| v.as_str())
            {
                params.insert(field.name.clone(), value.to_string());
            }
        }
    }

    params
}

/// Filter out Computed, LocalOnly, and Query fields from API request bodies
///
/// This utility is used by both the executor (default path) and handlers (hook path)
/// to ensure consistent field filtering across all API requests.
pub fn filter_request_fields(data: &Value, fields: &[FieldDescriptor]) -> Result<Value> {
    let mut filtered = data.clone();

    if let Some(obj) = filtered.as_object_mut() {
        // Always remove internal-only fields
        obj.remove("ref_name"); // Internal reference name, never sent to API

        for field in fields {
            // Remove fields that should not be sent in request body
            match field.location {
                FieldLocation::Computed | FieldLocation::LocalOnly | FieldLocation::Query => {
                    obj.remove(&field.name);
                }
                _ => {
                    // Body, Header, Path fields are kept (will be placed appropriately)
                }
            }
        }
    }

    Ok(filtered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_field_desc(name: &str, location: FieldLocation) -> FieldDescriptor {
        FieldDescriptor { name: name.to_string(), required: false, immutable: false, location }
    }

    #[test]
    fn test_extract_query_params_takes_string_query_fields_only() {
        // Only Query-location fields are extracted (Body ignored); non-string
        // query values are skipped (only string params survive coercion-free).
        let fields = vec![make_field_desc("version", FieldLocation::Query), make_field_desc("name", FieldLocation::Body), make_field_desc("count", FieldLocation::Query)];
        let data = json!({"version": "2024-01-01", "name": "my-agent", "count": 42});

        let params = extract_query_params(&data, &fields);

        assert_eq!(params.len(), 1);
        assert_eq!(params.get("version").unwrap(), "2024-01-01");
        assert!(!params.contains_key("name"), "non-query field excluded");
        assert!(!params.contains_key("count"), "non-string query value skipped");
    }

    #[test]
    fn test_filter_request_fields_drops_non_body_and_internal_keeps_rest() {
        // Computed/LocalOnly/Query and the internal ref_name are stripped from the
        // request body; Body/Path/Header fields (and unlisted keys) are retained.
        let fields = vec![
            make_field_desc("status", FieldLocation::Computed),
            make_field_desc("source_path", FieldLocation::LocalOnly),
            make_field_desc("version", FieldLocation::Query),
            make_field_desc("name", FieldLocation::Body),
            make_field_desc("id", FieldLocation::Path),
            make_field_desc("x-custom", FieldLocation::Header),
        ];
        let data = json!({"status": "active", "source_path": "/tmp", "version": "v1", "ref_name": "my-ref", "name": "agent", "id": "123", "x-custom": "val"});

        let filtered = filter_request_fields(&data, &fields).unwrap();
        let obj = filtered.as_object().unwrap();

        assert!(obj.get("status").is_none(), "computed dropped");
        assert!(obj.get("source_path").is_none(), "local-only dropped");
        assert!(obj.get("version").is_none(), "query dropped");
        assert!(obj.get("ref_name").is_none(), "internal ref_name dropped");
        assert_eq!(obj.get("name").unwrap(), "agent", "body kept");
        assert_eq!(obj.get("id").unwrap(), "123", "path kept");
        assert_eq!(obj.get("x-custom").unwrap(), "val", "header kept");
    }
}
