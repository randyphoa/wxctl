//! JSON-schema-type → Python-type mapping for typed tool stubs.

use serde_json::Value;

/// Map a JSON-schema `type` string to its Python annotation.
/// string→str, integer→int, number→float, boolean→bool, array→list, object→dict.
/// Unknown / missing types fall back to a permissive annotation.
pub fn py_type(schema_type: Option<&str>) -> &'static str {
    match schema_type {
        Some("string") => "str",
        Some("integer") => "int",
        Some("number") => "float",
        Some("boolean") => "bool",
        Some("array") => "list",
        Some("object") => "dict",
        _ => "object",
    }
}

/// Ordered (name, python_type) parameters derived from an `input_schema`.
/// Reads `properties` in insertion order; every property is required by
/// pipeline convention, so no defaults are emitted. Returns empty when the
/// schema has no `properties` object.
pub fn params_from_input_schema(input_schema: &Value) -> Vec<(String, &'static str)> {
    let Some(props) = input_schema.get("properties").and_then(|v| v.as_object()) else {
        return Vec::new();
    };
    props
        .iter()
        .map(|(name, prop)| {
            let t = prop.get("type").and_then(|v| v.as_str());
            (name.clone(), py_type(t))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn maps_each_scalar_type() {
        assert_eq!(py_type(Some("string")), "str");
        assert_eq!(py_type(Some("integer")), "int");
        assert_eq!(py_type(Some("number")), "float");
        assert_eq!(py_type(Some("boolean")), "bool");
        assert_eq!(py_type(Some("array")), "list");
        assert_eq!(py_type(Some("object")), "dict");
        assert_eq!(py_type(Some("weird")), "object");
        assert_eq!(py_type(None), "object");
    }

    #[test]
    fn params_from_input_schema_orders_and_handles_empty() {
        // Properties are extracted in order with their mapped Python types.
        let schema = json!({
            "type": "object",
            "properties": {
                "city": { "type": "string" },
                "days": { "type": "integer" }
            },
            "required": ["city", "days"]
        });
        assert_eq!(params_from_input_schema(&schema), vec![("city".to_string(), "str"), ("days".to_string(), "int")]);

        // No `properties` → no params.
        assert!(params_from_input_schema(&json!({"type": "object"})).is_empty());
    }
}
