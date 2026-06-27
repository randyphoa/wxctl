use crate::context::RuntimeIdStore;
use crate::templates::{CompiledTemplate, TemplateResolver};
use anyhow::Result;
use serde_json::Value;
use wxctl_core::{ResourceKey, parse_reference_with_path};

/// Prefix used when the engine injects full resolved linked-resource specs
/// into a resource's data right before a handler is invoked. Handlers that
/// need to walk DAG edges at apply time (e.g. `s3_bucket` reading creds
/// from its linked `storage_connection`, or `storage_registration`
/// assembling the wire body from its bucket and the bucket's connection)
/// read from `resource["__ref__<field>"]`.
///
/// The materializer walks only schema-declared fields so these synthetic
/// keys are never emitted to the API body.
pub const REF_ENRICH_PREFIX: &str = "__ref__";

pub(super) fn extract_resource_id<'a>(data: &'a Value, id_field: &str) -> Option<&'a str> {
    data.get(id_field).or_else(|| data.get("metadata").and_then(|m| m.get(id_field))).or_else(|| data.get("entity").and_then(|e| e.get(id_field))).and_then(|v| v.as_str())
}

pub(crate) fn resolve_dependencies(data: &Value, runtime_ids: &RuntimeIdStore, schema: &wxctl_core::schema::ResourceSchema) -> Result<Value> {
    let template = CompiledTemplate::compile(data.clone())?;
    let mut resolver = TemplateResolver::new(runtime_ids);
    let mut resolved = resolver.resolve(&template)?;

    // Apply schema-aware field extraction for references
    // This handles cases like llm: ${model.name} where we need to extract just the "name" field
    apply_field_references(&mut resolved, schema);

    Ok(resolved)
}

/// Inject full resolved linked-resource specs under synthetic `__ref__<field>`
/// keys so handlers can read upstream resources' spec (including sensitive
/// fields) when assembling wire bodies. The materializer ignores unknown
/// keys, so the injected data never leaks into API requests.
///
/// Enrichment recurses one hop: if a linked resource itself has reference
/// fields (e.g. `s3_bucket.connection` → `storage_connection`), the
/// enrichment walker resolves those too and stores them nested inside the
/// first-level enriched object (`__ref__bucket.__ref__connection`). This
/// lets an `s3_object` handler reach creds via
/// `resource["__ref__bucket"]["__ref__connection"]` without needing direct
/// access to the runtime store.
///
/// Silent when a reference is unresolvable — the existing validation and
/// reconciliation layers surface dependency errors separately; this helper
/// is purely best-effort enrichment for handler convenience.
pub(crate) fn enrich_with_linked_refs(data: &mut Value, raw_data: &Value, runtime_ids: &RuntimeIdStore, schema: &wxctl_core::schema::ResourceSchema, registry: &wxctl_core::ResourceRegistry) {
    enrich_recursive(data, Some(raw_data), runtime_ids, &schema.resource.schema, registry, 0);
}

const MAX_ENRICH_DEPTH: usize = 2;

fn enrich_recursive(data: &mut Value, raw_data: Option<&Value>, runtime_ids: &RuntimeIdStore, schema: &wxctl_core::schema::SchemaDefinition, registry: &wxctl_core::ResourceRegistry, depth: usize) {
    if depth > MAX_ENRICH_DEPTH {
        return;
    }
    let Value::Object(map) = data else { return };
    let raw_map_opt = raw_data.and_then(Value::as_object);

    for field in schema.all_fields() {
        let Some(refs) = &field.references else { continue };

        // Prefer raw (pre-template) value for ref-name extraction; fall
        // back to the enriched data after template resolution.
        let raw_val_opt = raw_map_opt.and_then(|m| m.get(&field.name)).or_else(|| map.get(&field.name));
        let Some(raw_val) = raw_val_opt else { continue };
        let Some(ref_name) = extract_ref_name(raw_val) else { continue };

        let candidates: Vec<&str> = std::iter::once(refs.resource.as_str()).chain(refs.also_allows.iter().map(|s| s.as_str())).collect();
        for kind in candidates {
            let key = ResourceKey::new(kind, &ref_name);
            if let Some(mut full) = runtime_ids.get(&key) {
                if let Some(descriptor) = registry.get_descriptor(kind) {
                    // SAFETY: we mutate `full` while reading its earlier
                    // structure via a separate borrow; serde_json::Value
                    // does not move on mutation so the read borrow of the
                    // starting map stays valid for extract_ref_name calls
                    // during this recursion. We pass `None` to disable the
                    // raw-data fallback on the deeper hop.
                    enrich_recursive(&mut full, None, runtime_ids, &descriptor.schema.resource.schema, registry, depth + 1);
                }
                map.insert(format!("{}{}", REF_ENRICH_PREFIX, field.name), full);
                break;
            }
        }
    }
}

/// Extract the target `ref_name` from a raw field value — either a template
/// (`${kind.name[.path]}`) or a plain string. Returns None for other shapes.
fn extract_ref_name(raw: &Value) -> Option<String> {
    let s = raw.as_str()?;
    if s.starts_with("${") {
        return parse_reference_with_path(s).map(|r| r.key.name.to_string());
    }
    Some(s.to_string())
}

/// Apply schema-aware field extraction based on references metadata.
/// Converts resolved objects to extracted field values where schema specifies references.
pub(crate) fn apply_field_references(value: &mut Value, schema: &wxctl_core::schema::ResourceSchema) {
    apply_field_references_recursive(value, &schema.resource.schema, "");
}

fn apply_field_references_recursive(value: &mut Value, schema: &wxctl_core::schema::SchemaDefinition, field_path: &str) {
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let current_path = if field_path.is_empty() { key.clone() } else { format!("{}.{}", field_path, key) };

                if let Some(field_def) = schema.fields.iter().find(|f| f.name == *key) {
                    if let Some(refs) = &field_def.references {
                        match field_def.field_type {
                            wxctl_core::schema::FieldType::Object => {
                                // Map-of-references: values are terminal UUIDs after extraction
                                if let Value::Object(inner_map) = val {
                                    for inner_val in inner_map.values_mut() {
                                        extract_reference_field(inner_val, &refs.field);
                                    }
                                }
                                continue;
                            }
                            _ => {
                                extract_reference_field(val, &refs.field);
                            }
                        }
                    }

                    if let Some(nested_schema) = &field_def.schema {
                        apply_field_references_recursive(val, nested_schema, &current_path);
                    } else {
                        apply_field_references_recursive(val, schema, &current_path);
                    }
                } else {
                    apply_field_references_recursive(val, schema, &current_path);
                }
            }
        }
        Value::Array(arr) => {
            let field_name = field_path.split('.').next().unwrap_or(field_path);
            let field_def = schema.fields.iter().find(|f| f.name == field_name);

            for item in arr.iter_mut() {
                if let Some(fd) = field_def
                    && let Some(refs) = &fd.references
                {
                    extract_reference_field(item, &refs.field);
                }
                apply_field_references_recursive(item, schema, field_path);
            }
        }
        _ => {}
    }
}

/// Extract a specific field from a resolved reference value.
/// Converts {"id": "abc", "name": "foo", ...} to "abc" if target_field is "id".
/// Also handles entity/metadata patterns (common_core) where the field may be
/// nested under `metadata.<field>` or `entity.<field>`.
fn extract_reference_field(value: &mut Value, target_field: &str) {
    if let Value::Object(obj) = value {
        // Try top-level first
        if let Some(field_value) = obj.get(target_field).and_then(|v| v.as_str()) {
            *value = Value::String(field_value.to_string());
            return;
        }
        // Try metadata.<field> (common_core entity/metadata pattern)
        if let Some(Value::Object(metadata)) = obj.get("metadata")
            && let Some(field_value) = metadata.get(target_field).and_then(|v| v.as_str())
        {
            *value = Value::String(field_value.to_string());
            return;
        }
        // Try entity.<field>
        if let Some(Value::Object(entity)) = obj.get("entity")
            && let Some(field_value) = entity.get(target_field).and_then(|v| v.as_str())
        {
            *value = Value::String(field_value.to_string());
        }
    }
}

/// Merge request data with API response.
/// Request fields are used as base, response fields override them.
/// This ensures all fields (including those only in request like "name") are available
/// for dependent resource reference resolution.
pub(super) fn merge_request_response(request: &Value, response: &Value) -> Value {
    match (request, response) {
        (Value::Object(req_obj), Value::Object(resp_obj)) => {
            let mut merged = req_obj.clone();
            // Response fields override request fields
            for (key, value) in resp_obj {
                merged.insert(key.clone(), value.clone());
            }
            Value::Object(merged)
        }
        // If not both objects, prefer response
        _ => response.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::ResourceKey;
    use wxctl_core::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldLocation, FieldType, HookDefinition, HttpMethod, ReconciliationDefinition, ResourceDefinition, ResourceSchema, SchemaDefinition, UpdateStrategy};

    fn make_store(entries: Vec<(&str, &str, Value)>) -> RuntimeIdStore {
        let store = RuntimeIdStore::new();
        for (kind, name, data) in entries {
            store.insert(ResourceKey::new(kind, name), data);
        }
        store
    }

    fn make_field(name: &str, field_type: FieldType) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type,
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
            sensitive: false,
            also_query: false,
            is_path: false,
            properties: None,
        }
    }

    fn minimal_schema() -> ResourceSchema {
        ResourceSchema {
            resource: ResourceDefinition {
                name: "agent".to_string(),
                service: "test".to_string(),
                kind: "agent".to_string(),
                version: "v1".to_string(),
                api: ApiDefinition {
                    base_path: "/v1/agents".to_string(),
                    id_field: "id".to_string(),
                    list_endpoint: Some("/v1/agents".to_string()),
                    get_endpoint: "/v1/agents/{id}".to_string(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: None,
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                },
                schema: SchemaDefinition { fields: vec![make_field("name", FieldType::String), make_field("tools", FieldType::Array), make_field("style", FieldType::String)], ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, id_source: "id".to_string() },
                    state_fields: None,
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec![],
                    reject_on_immutable_drift: false,
                    use_json_patch: false,
                    json_patch_path_prefix: None,
                },
                hooks: HookDefinition::default(),
                deployments: None,
                unsupported_on: vec![],
                description: None,
                prompt: None,
            },
        }
    }

    #[test]
    fn resolve_dependencies_cross_kind_field_path_and_missing() {
        let schema = minimal_schema();

        // Simulates reconciliation path: RuntimeIdStore built from cache, agent data
        // with a ${toolkit.x.tools.hello} template; literals pass through unchanged.
        let store = make_store(vec![("toolkit", "hello_toolkit", json!({"id": "tk-123", "name": "hello_toolkit", "tools": {"hello": "tool-uuid-1"}}))]);
        let agent_data = json!({
            "name": "hello_agent",
            "tools": ["${toolkit.hello_toolkit.tools.hello}"],
            "style": "default"
        });
        let resolved = resolve_dependencies(&agent_data, &store, &schema).unwrap();
        assert_eq!(resolved["tools"][0], json!("tool-uuid-1"));
        assert_eq!(resolved["name"], json!("hello_agent"));
        assert_eq!(resolved["style"], json!("default"));

        // Missing referenced resource → error.
        let store = RuntimeIdStore::new();
        let result = resolve_dependencies(&json!({"tool": "${toolkit.missing.tools.hello}"}), &store, &schema);
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn nested_string_reference_extracted() {
        let store = make_store(vec![("orchestrate_connection", "my_conn", json!({"app_id": "my-app", "connection_id": "uuid-123", "name": "my_conn"}))]);

        let conn_id_field = {
            let mut f = make_field("connection_id", FieldType::String);
            f.references = Some(wxctl_core::schema::FieldReferences { resource: "orchestrate_connection".to_string(), field: "app_id".to_string(), also_allows: vec![], optional: false });
            f
        };
        let openapi_field = {
            let mut f = make_field("openapi", FieldType::Object);
            f.schema = Some(Box::new(wxctl_core::schema::SchemaDefinition { fields: vec![conn_id_field], ..Default::default() }));
            f
        };
        let binding_field = {
            let mut f = make_field("binding", FieldType::Object);
            f.schema = Some(Box::new(wxctl_core::schema::SchemaDefinition { fields: vec![openapi_field], ..Default::default() }));
            f
        };
        let mut schema = minimal_schema();
        schema.resource.schema.fields.push(binding_field);

        let data = json!({
            "name": "my_tool",
            "binding": {
                "openapi": {
                    "connection_id": "${orchestrate_connection.my_conn}"
                }
            }
        });

        let resolved = resolve_dependencies(&data, &store, &schema).unwrap();
        assert_eq!(resolved["binding"]["openapi"]["connection_id"], "my-app");
    }

    #[test]
    fn map_of_references_branches() {
        // Build a schema with a `connections` map-of-references field nested under
        // `<wrapper>` (e.g. binding.python / mcp), referencing orchestrate_connection.connection_id.
        let schema_with_connections = |outer: &str, inner: Option<&str>| {
            let connections_field = {
                let mut f = make_field("connections", FieldType::Object);
                f.references = Some(wxctl_core::schema::FieldReferences { resource: "orchestrate_connection".to_string(), field: "connection_id".to_string(), also_allows: vec![], optional: false });
                f
            };
            let inner_field = {
                let mut f = make_field(inner.unwrap_or(outer), FieldType::Object);
                f.schema = Some(Box::new(wxctl_core::schema::SchemaDefinition { fields: vec![connections_field], ..Default::default() }));
                f
            };
            let wrapper = if inner.is_some() {
                let mut f = make_field(outer, FieldType::Object);
                f.schema = Some(Box::new(wxctl_core::schema::SchemaDefinition { fields: vec![inner_field], ..Default::default() }));
                f
            } else {
                inner_field
            };
            let mut schema = minimal_schema();
            schema.resource.schema.fields.push(wrapper);
            schema
        };

        // Single map entry whose ${...} value is replaced by the referenced field (connection_id).
        let store = make_store(vec![("orchestrate_connection", "my_conn", json!({"connection_id": "uuid-abc", "app_id": "my-svc", "name": "my_conn"}))]);
        let schema = schema_with_connections("binding", Some("python"));
        let data = json!({"name": "my_tool", "binding": {"python": {"connections": {"my-svc": "${orchestrate_connection.my_conn}"}}}});
        let resolved = resolve_dependencies(&data, &store, &schema).unwrap();
        assert_eq!(resolved["binding"]["python"]["connections"]["my-svc"], "uuid-abc");

        // Multiple map entries each independently resolved.
        let store = make_store(vec![("orchestrate_connection", "conn_a", json!({"connection_id": "uuid-a"})), ("orchestrate_connection", "conn_b", json!({"connection_id": "uuid-b"}))]);
        let schema = schema_with_connections("mcp", None);
        let data = json!({"name": "my_toolkit", "mcp": {"connections": {"svc-a": "${orchestrate_connection.conn_a}", "svc-b": "${orchestrate_connection.conn_b}"}}});
        let resolved = resolve_dependencies(&data, &store, &schema).unwrap();
        assert_eq!(resolved["mcp"]["connections"]["svc-a"], "uuid-a");
        assert_eq!(resolved["mcp"]["connections"]["svc-b"], "uuid-b");

        // A plain (non-template) string value is left unchanged.
        let store = RuntimeIdStore::new();
        let schema = schema_with_connections("mcp", None);
        let data = json!({"name": "my_toolkit", "mcp": {"connections": {"svc-a": "already-a-uuid"}}});
        let resolved = resolve_dependencies(&data, &store, &schema).unwrap();
        assert_eq!(resolved["mcp"]["connections"]["svc-a"], "already-a-uuid");
    }
}
