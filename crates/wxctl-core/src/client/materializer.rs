use super::request::{BodyKind, RequestSpec};
use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value};
use wxctl_schema::ir::{FieldIr, FieldLocationIr};

/// Insert a value at a nested path in a JSON object (e.g., "additional_properties.icon")
/// When both the existing value and new value are objects, they are merged (new values take precedence).
fn insert_nested(obj: &mut Map<String, Value>, path: &str, value: Value) -> Result<()> {
    let parts: Vec<&str> = path.split('.').collect();

    if parts.len() == 1 {
        // Simple case: no nesting - merge if both are objects, otherwise replace
        let key = parts[0].to_string();
        if let Some(Value::Object(existing)) = obj.get_mut(&key)
            && let Value::Object(new_map) = value
        {
            // Merge: new values take precedence
            for (k, v) in new_map {
                existing.insert(k, v);
            }
            return Ok(());
        }
        obj.insert(key, value);
        return Ok(());
    }

    // Navigate/create nested structure (all but last part)
    let mut current = obj;
    for part in parts.iter().take(parts.len() - 1) {
        current = current.entry(part.to_string()).or_insert_with(|| Value::Object(Map::new())).as_object_mut().ok_or_else(|| anyhow!("Expected object at path segment '{}' in field '{}', found non-object value", part, path))?;
    }

    // Insert value at the final part
    if let Some(last_part) = parts.last() {
        current.insert(last_part.to_string(), value);
    }

    Ok(())
}

/// Partition resource data by field location and build RequestSpec
pub struct RequestMaterializer {
    method: Method,
    path_template: String,
}

impl RequestMaterializer {
    /// Create new materializer for an HTTP request
    pub fn new(method: Method, path_template: impl Into<String>) -> Self {
        Self { method, path_template: path_template.into() }
    }

    /// Materialize RequestSpec from resource data and schema fields
    /// Partitions fields by location: Body, Query, Path, Header
    /// Excludes Computed and LocalOnly fields
    pub fn materialize(self, data: &Value, fields: &[FieldIr], body_kind: BodyKindSelector) -> Result<RequestSpec> {
        let obj = data.as_object().ok_or_else(|| anyhow!("Resource data must be a JSON object"))?;

        let mut spec = RequestSpec::new(self.method, self.path_template);
        spec.sensitive_paths = crate::logging::sensitive_paths_from_fields(fields);
        let mut body_fields = Map::new();
        let bodyless = matches!(body_kind, BodyKindSelector::None);

        for field in fields {
            // Skip if field not present in data
            let value = match obj.get(field.name) {
                Some(v) => v,
                None => continue,
            };

            match field.location {
                FieldLocationIr::Body => {
                    if bodyless && field.also_query {
                        // Bodyless verb (GET/DELETE) — straddle field becomes a query
                        // param instead of being silently dropped with the body.
                        spec.query.push((field.name.to_string(), coerce_to_string(value)?));
                    } else {
                        // Use api_field if specified, otherwise use field.name
                        let target_path = field.api_field.unwrap_or(field.name);
                        insert_nested(&mut body_fields, target_path, value.clone())?;
                    }
                }
                FieldLocationIr::Query => {
                    let string_value = coerce_to_string(value)?;
                    spec.query.push((field.name.to_string(), string_value));
                }
                FieldLocationIr::Path => {
                    let string_value = coerce_to_string(value)?;
                    spec.path_vars.insert(field.name.to_string(), string_value);
                }
                FieldLocationIr::Header => {
                    let string_value = coerce_to_string(value)?;
                    spec.headers.insert(field.name.to_string(), string_value);
                }
                FieldLocationIr::Computed | FieldLocationIr::LocalOnly => {
                    // Skip - not sent to API
                }
            }
        }

        // Set body based on selector
        spec.body = body_kind.select(Value::Object(body_fields));

        Ok(spec)
    }
}

/// Selector for body kind based on operation context
pub enum BodyKindSelector<'a> {
    /// No body
    None,
    /// Standard JSON body
    Json,
    /// JSON Patch with specific changed fields
    JsonPatch { changed_fields: Vec<String>, path_prefix: String, fields: &'a [FieldIr] },
}

impl<'a> BodyKindSelector<'a> {
    fn select(self, body_data: Value) -> BodyKind {
        match self {
            Self::None => BodyKind::None,
            Self::Json => BodyKind::Json(body_data),
            Self::JsonPatch { changed_fields, path_prefix, fields } => {
                let patch = generate_json_patch(&body_data, &changed_fields, &path_prefix, fields);
                BodyKind::JsonPatch(patch)
            }
        }
    }
}

/// Coerce JSON value to string for query/path/header parameters
fn coerce_to_string(value: &Value) -> Result<String> {
    match value {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        Value::Null => Ok(String::new()),
        _ => Err(anyhow!("Cannot coerce complex JSON value to string: {}", value)),
    }
}

/// Traverse a dotted JSON path (e.g. `"additional_properties.icon"` or
/// `"associated_catalogs.0.catalog_name"`) — the single source of truth for
/// nested lookups across the codebase.
///
/// - Object segments look up by key.
/// - Numeric segments index into arrays; a non-numeric segment against an array,
///   or any segment against a scalar, is a miss (`None`).
/// - A present-but-`null` value is a hit (`Some(Value::Null)`), not a miss.
/// - When `envelope_fallback` is set and a **single-segment** path misses at the
///   top level, the CP4D/watsonx.data `entity.<field>` then `metadata.<field>`
///   envelopes are probed. This mirrors how those responses wrap fields.
///
/// Returns `None` when the path is absent everywhere consulted.
pub fn lookup_nested<'a>(value: &'a Value, path: &str, envelope_fallback: bool) -> Option<&'a Value> {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = value;
    let mut missed = false;

    for part in &parts {
        match current {
            Value::Object(map) => match map.get(*part) {
                Some(v) => current = v,
                None => {
                    missed = true;
                    break;
                }
            },
            Value::Array(arr) => match part.parse::<usize>() {
                Ok(idx) => match arr.get(idx) {
                    Some(v) => current = v,
                    None => {
                        missed = true;
                        break;
                    }
                },
                Err(_) => {
                    missed = true;
                    break;
                }
            },
            _ => {
                missed = true;
                break;
            }
        }
    }

    if !missed {
        return Some(current);
    }

    // Single-segment miss → probe the CP4D/watsonx.data envelope.
    if envelope_fallback
        && parts.len() == 1
        && let Value::Object(map) = value
    {
        for envelope in ["entity", "metadata"] {
            if let Some(Value::Object(inner)) = map.get(envelope)
                && let Some(found) = inner.get(parts[0])
            {
                return Some(found);
            }
        }
    }

    None
}

/// Extract a value from a nested JSON path — objects and array indices, no
/// envelope fallback. Thin wrapper over [`lookup_nested`].
pub fn extract_nested<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    lookup_nested(value, path, false)
}

/// Generate JSON Patch operations (RFC 6902) for specified fields
/// Path prefix can be configured per resource:
/// - "/entity" for CP4D compatibility
/// - "" for standard RFC 6902 paths
fn generate_json_patch(data: &Value, changed_fields: &[String], path_prefix: &str, fields: &[FieldIr]) -> Value {
    let mut operations = Vec::new();

    for field_name in changed_fields {
        // Find the field definition to get api_field path if present
        let field_def = fields.iter().find(|f| f.name == field_name.as_str());
        let api_path = field_def.and_then(|f| f.api_field).unwrap_or(field_name.as_str());

        // Extract value from the nested structure using the api_path
        if let Some(value) = extract_nested(data, api_path) {
            // Convert dot notation to JSON Pointer format (slashes)
            let json_pointer = api_path.replace('.', "/");
            let path = if path_prefix.is_empty() { format!("/{}", json_pointer) } else { format!("{}/{}", path_prefix, json_pointer) };

            operations.push(serde_json::json!({
                "op": "replace",
                "path": path,
                "value": value
            }));
        }
    }

    Value::Array(operations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_schema::ir::FieldTypeIr;

    fn make_field(name: &'static str, location: FieldLocationIr) -> FieldIr {
        FieldIr {
            name,
            field_type: FieldTypeIr::String,
            required: false,
            immutable: false,
            location,
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references: None,
            api_field: None,
            sensitive: false,
            also_query: false,
            is_path: false,
            synthesize: None,
            synth_shape: None,
        }
    }

    #[test]
    fn test_fields_partition_by_location_and_exclude_computed_local_only() {
        // One materialize call exercises every FieldLocation branch: Body lands in
        // the body, Query/Path/Header land in their partitions, and Computed +
        // LocalOnly are dropped (never sent to the API).
        let fields = vec![
            make_field("name", FieldLocationIr::Body),
            make_field("version", FieldLocationIr::Query),
            make_field("id", FieldLocationIr::Path),
            make_field("x-custom", FieldLocationIr::Header),
            make_field("status", FieldLocationIr::Computed),
            make_field("source_path", FieldLocationIr::LocalOnly),
        ];
        let data = json!({"name": "my-agent", "version": "2024-01-01", "id": "abc-123", "x-custom": "value", "status": "active", "source_path": "/tmp/tool"});

        let spec = RequestMaterializer::new(Method::POST, "/agents/{id}").materialize(&data, &fields, BodyKindSelector::Json).unwrap();

        let body = spec.body.as_json().unwrap();
        assert_eq!(body["name"], "my-agent");
        assert_eq!(spec.query, vec![("version".to_string(), "2024-01-01".to_string())]);
        assert_eq!(spec.path_vars.get("id").unwrap(), "abc-123");
        assert_eq!(spec.headers.get("x-custom").unwrap(), "value");
        // Computed + LocalOnly excluded from the body.
        assert!(body.get("status").is_none(), "computed field leaked into body");
        assert!(body.get("source_path").is_none(), "local-only field leaked into body");
    }

    #[test]
    fn test_api_field_nesting_and_merge_shared_parent() {
        // Two fields with api_field paths under the same parent must merge into one
        // nested object rather than the second overwriting the first.
        let mut field1 = make_field("icon", FieldLocationIr::Body);
        field1.api_field = Some("additional_properties.icon");
        let mut field2 = make_field("color", FieldLocationIr::Body);
        field2.api_field = Some("additional_properties.color");
        let fields = vec![field1, field2];
        let data = json!({"icon": "star", "color": "blue"});

        let spec = RequestMaterializer::new(Method::POST, "/agents").materialize(&data, &fields, BodyKindSelector::Json).unwrap();

        let body = spec.body.as_json().unwrap();
        assert_eq!(body["additional_properties"]["icon"], "star");
        assert_eq!(body["additional_properties"]["color"], "blue");
    }

    #[test]
    fn test_json_patch_generation_with_and_without_prefix() {
        let fields = vec![make_field("description", FieldLocationIr::Body)];
        let data = json!({"description": "updated"});

        // Empty prefix → standard RFC 6902 path "/description".
        let spec = RequestMaterializer::new(Method::PATCH, "/agents/{id}").materialize(&data, &fields, BodyKindSelector::JsonPatch { changed_fields: vec!["description".to_string()], path_prefix: String::new(), fields: &fields }).unwrap();
        let ops = spec.body.as_json().unwrap().as_array().unwrap().clone();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0]["op"], "replace");
        assert_eq!(ops[0]["path"], "/description");
        assert_eq!(ops[0]["value"], "updated");

        // "/entity" prefix (CP4D compatibility) → "/entity/description".
        let spec = RequestMaterializer::new(Method::PATCH, "/agents/{id}").materialize(&data, &fields, BodyKindSelector::JsonPatch { changed_fields: vec!["description".to_string()], path_prefix: "/entity".to_string(), fields: &fields }).unwrap();
        let ops = spec.body.as_json().unwrap().as_array().unwrap().clone();
        assert_eq!(ops[0]["path"], "/entity/description");
    }

    #[test]
    fn test_coerce_to_string() {
        // Primitives stringify (null → empty); complex JSON is rejected.
        assert_eq!(coerce_to_string(&json!("hello")).unwrap(), "hello");
        assert_eq!(coerce_to_string(&json!(42)).unwrap(), "42");
        assert_eq!(coerce_to_string(&json!(true)).unwrap(), "true");
        assert_eq!(coerce_to_string(&json!(null)).unwrap(), "");
        assert!(coerce_to_string(&json!({"key": "value"})).is_err());
        assert!(coerce_to_string(&json!([1, 2, 3])).is_err());
    }

    #[test]
    fn test_also_query_field_routes_by_verb() {
        // A Body field with also_query stays in the body for body-bearing verbs (POST),
        // but becomes a query param for bodyless verbs (DELETE) instead of being dropped.
        let mut field = make_field("space_id", FieldLocationIr::Body);
        field.also_query = true;
        let fields = vec![field];
        let data = json!({"space_id": "abc"});

        let spec = RequestMaterializer::new(Method::POST, "/agents").materialize(&data, &fields, BodyKindSelector::Json).unwrap();
        let body = spec.body.as_json().unwrap();
        assert_eq!(body["space_id"], "abc");
        assert!(spec.query.is_empty());

        let spec = RequestMaterializer::new(Method::DELETE, "/agents/{id}").materialize(&data, &fields, BodyKindSelector::None).unwrap();
        assert_eq!(spec.query, vec![("space_id".to_string(), "abc".to_string())]);
        assert!(matches!(spec.body, BodyKind::None));
    }

    #[test]
    fn test_lookup_nested_objects_and_arrays() {
        // Objects by key; numeric segments index arrays; array + non-numeric or
        // scalar + any = miss; present-but-null is a hit (Some(Null)).
        let v = json!({"storage": {"bucket": "b1"}, "catalogs": [{"name": "cat_a"}, {"name": "cat_b"}], "empty": null});
        assert_eq!(lookup_nested(&v, "storage.bucket", false).and_then(Value::as_str), Some("b1"));
        assert_eq!(lookup_nested(&v, "catalogs.0.name", false).and_then(Value::as_str), Some("cat_a"));
        assert_eq!(lookup_nested(&v, "catalogs.1.name", false).and_then(Value::as_str), Some("cat_b"));
        assert!(lookup_nested(&v, "catalogs.2.name", false).is_none(), "out-of-bounds index misses");
        assert!(lookup_nested(&v, "catalogs.nope", false).is_none(), "non-numeric segment on array misses");
        assert!(lookup_nested(&v, "storage.bucket.deep", false).is_none(), "descending into a scalar misses");
        assert!(lookup_nested(&v, "absent", false).is_none(), "absent key misses");
        assert_eq!(lookup_nested(&v, "empty", false), Some(&Value::Null), "present-but-null is a hit, not a miss");
    }

    #[test]
    fn test_lookup_nested_envelope_fallback_single_segment_only() {
        // Single-segment miss at top level falls through entity.<field> then
        // metadata.<field> only when envelope_fallback is on; multi-segment paths
        // never trigger the fallback.
        let v = json!({"entity": {"description": "from-entity"}, "metadata": {"id": "meta-id"}});
        assert_eq!(lookup_nested(&v, "description", true).and_then(Value::as_str), Some("from-entity"));
        assert_eq!(lookup_nested(&v, "id", true).and_then(Value::as_str), Some("meta-id"));
        // Fallback disabled → top-level miss stays a miss.
        assert!(lookup_nested(&v, "description", false).is_none());
        // Multi-segment paths do not consult the envelope.
        assert!(lookup_nested(&v, "foo.description", true).is_none());
        // A top-level hit wins over the envelope.
        let v2 = json!({"description": "top", "entity": {"description": "nested"}});
        assert_eq!(lookup_nested(&v2, "description", true).and_then(Value::as_str), Some("top"));
    }

    #[test]
    fn test_insert_nested_non_object_intermediate_returns_error() {
        // First field writes "additional_properties" as a plain string into body
        let field1 = make_field("additional_properties", FieldLocationIr::Body);
        // Second field tries to nest under "additional_properties.icon"
        let mut field2 = make_field("icon", FieldLocationIr::Body);
        field2.api_field = Some("additional_properties.icon");
        let fields = vec![field1, field2];
        let data = json!({"additional_properties": "not-an-object", "icon": "star"});

        let result = RequestMaterializer::new(Method::POST, "/agents").materialize(&data, &fields, BodyKindSelector::Json);

        assert!(result.is_err());
    }
}
