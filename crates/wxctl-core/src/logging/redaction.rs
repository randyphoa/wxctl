use serde_json::Value;

/// Sensitive field names to redact
const SENSITIVE_FIELDS: &[&str] = &["password", "token", "secret", "key", "auth", "authorization", "api_key", "apikey", "access_token", "refresh_token"];

/// Redact sensitive fields from JSON value based on a keyword heuristic.
/// First line of defence; `redact_by_schema` layers precision on top.
pub fn redact_sensitive(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (k, v) in map {
                let key_lower = k.to_lowercase();
                let is_sensitive = SENSITIVE_FIELDS.iter().any(|s| key_lower.contains(s));
                if is_sensitive {
                    redacted.insert(k.clone(), Value::String("***REDACTED***".to_string()));
                } else {
                    redacted.insert(k.clone(), redact_sensitive(v));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(redact_sensitive).collect()),
        _ => value.clone(),
    }
}

/// Redact fields whose dotted path appears in `sensitive_paths`.
/// Complements `redact_sensitive` when the schema marks fields explicitly
/// — precise, and masks fields whose names don't match the keyword list.
/// Array indices are skipped in the path (arrays are traversed but the
/// path does not gain an `[i]` segment), matching the dotted-path syntax
/// used elsewhere in the schema (`connection.password`, not
/// `connection.password[0]`).
pub fn redact_by_schema(value: &Value, sensitive_paths: &[String]) -> Value {
    if sensitive_paths.is_empty() {
        return value.clone();
    }
    redact_by_schema_at(value, sensitive_paths, "")
}

fn redact_by_schema_at(value: &Value, paths: &[String], current: &str) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let child = if current.is_empty() { k.clone() } else { format!("{current}.{k}") };
                if paths.iter().any(|p| p == &child) {
                    out.insert(k.clone(), Value::String("***".to_string()));
                } else {
                    out.insert(k.clone(), redact_by_schema_at(v, paths, &child));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|item| redact_by_schema_at(item, paths, current)).collect()),
        _ => value.clone(),
    }
}

use crate::schema::FieldDefinition;

/// Collect dotted paths of fields marked `sensitive: true` from a field slice,
/// recursing into nested object schemas. Mirrors `SchemaDefinition::sensitive_paths`
/// but operates on a bare field slice (what the request materializer holds).
pub fn sensitive_paths_from_fields(fields: &[FieldDefinition]) -> Vec<String> {
    let mut out = Vec::new();
    collect(fields, "", &mut out);
    return out;

    fn collect(fields: &[FieldDefinition], prefix: &str, out: &mut Vec<String>) {
        for field in fields {
            let path = if prefix.is_empty() { field.name.clone() } else { format!("{prefix}.{}", field.name) };
            if field.sensitive {
                out.push(path.clone());
            }
            if let Some(inner) = &field.schema {
                collect(&inner.fields, &path, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn redact_by_schema_masks_listed_paths_only() {
        // Top-level listed path masked, siblings retained.
        let v = json!({"name": "foo", "api_key": "12345"});
        let out = redact_by_schema(&v, &["api_key".into()]);
        assert_eq!(out["api_key"], json!("***"));
        assert_eq!(out["name"], json!("foo"));

        // Nested dotted path masked, sibling retained.
        let v = json!({"connection": {"host": "h", "password": "p"}});
        let out = redact_by_schema(&v, &["connection.password".into()]);
        assert_eq!(out["connection"]["password"], json!("***"));
        assert_eq!(out["connection"]["host"], json!("h"));

        // Unlisted field left untouched (precise — no keyword heuristic here).
        let v = json!({"credit_card": "4111"});
        let out = redact_by_schema(&v, &["password".into()]);
        assert_eq!(out["credit_card"], json!("4111"));

        // Arrays traversed: index is skipped in the dotted path, so every element matches.
        let v = json!({"connections": [{"password": "p1"}, {"password": "p2"}]});
        let out = redact_by_schema(&v, &["connections.password".into()]);
        assert_eq!(out["connections"][0]["password"], json!("***"));
        assert_eq!(out["connections"][1]["password"], json!("***"));
    }

    #[test]
    fn redact_sensitive_still_catches_keyword_hits() {
        let v = json!({"api_key": "secret", "normal": "plain"});
        let out = redact_sensitive(&v);
        assert_eq!(out["api_key"], json!("***REDACTED***"));
        assert_eq!(out["normal"], json!("plain"));
    }

    #[test]
    fn schema_then_keyword_double_redacts() {
        let body = serde_json::json!({"username": "u", "api_key": "SEEDED-SECRET", "nested": {"password": "SEEDED-SECRET"}});
        let by_schema = redact_by_schema(&body, &["nested.password".to_string()]);
        let out = redact_sensitive(&by_schema);
        let s = serde_json::to_string(&out).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "secret leaked: {s}");
        assert!(s.contains("\"username\":\"u\""), "non-sensitive retained: {s}");
    }

    #[test]
    fn sensitive_paths_from_fields_collects_nested() {
        use crate::schema::{FieldDefinition, FieldLocation, FieldType, SchemaDefinition};
        let mut pwd = FieldDefinition {
            name: "password".into(),
            field_type: FieldType::String,
            required: false,
            immutable: false,
            location: FieldLocation::Body,
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references: None,
            api_field: None,
            sensitive: true,
            also_query: false,
            properties: None,
            is_path: false,
        };
        let host = FieldDefinition { name: "host".into(), sensitive: false, ..pwd.clone() };
        let conn = FieldDefinition { name: "connection".into(), field_type: FieldType::Object, sensitive: false, schema: Some(Box::new(SchemaDefinition { fields: vec![host, pwd.clone()], ..Default::default() })), ..pwd.clone() };
        pwd.name = "api_key".into();
        let paths = sensitive_paths_from_fields(&[conn, pwd]);
        assert!(paths.contains(&"connection.password".to_string()));
        assert!(paths.contains(&"api_key".to_string()));
        assert_eq!(paths.len(), 2);
    }
}
