use super::FieldDescriptor;
use anyhow::Result;
use serde_json::Value;
use wxctl_schema::ir::FieldLocationIr;

/// Filter out Computed, LocalOnly, and Query fields from API request bodies
///
/// Used by handlers (hook path) to ensure consistent field filtering across
/// API requests; query-param handling lives in the engine's scoping pass and
/// the request materializer.
pub fn filter_request_fields(data: &Value, fields: &[FieldDescriptor]) -> Result<Value> {
    let mut filtered = data.clone();

    if let Some(obj) = filtered.as_object_mut() {
        // Always remove internal-only fields
        obj.remove("ref_name"); // Internal reference name, never sent to API

        for field in fields {
            // Remove fields that should not be sent in request body
            match field.location {
                FieldLocationIr::Computed | FieldLocationIr::LocalOnly | FieldLocationIr::Query => {
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

    fn make_field_desc(name: &str, location: FieldLocationIr) -> FieldDescriptor {
        FieldDescriptor { name: name.to_string(), required: false, immutable: false, location }
    }

    #[test]
    fn test_filter_request_fields_drops_non_body_and_internal_keeps_rest() {
        // Computed/LocalOnly/Query and the internal ref_name are stripped from the
        // request body; Body/Path/Header fields (and unlisted keys) are retained.
        let fields = vec![
            make_field_desc("status", FieldLocationIr::Computed),
            make_field_desc("source_path", FieldLocationIr::LocalOnly),
            make_field_desc("version", FieldLocationIr::Query),
            make_field_desc("name", FieldLocationIr::Body),
            make_field_desc("id", FieldLocationIr::Path),
            make_field_desc("x-custom", FieldLocationIr::Header),
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
