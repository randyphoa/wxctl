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

pub(super) fn extract_resource_id(data: &Value, id_field: &str) -> Option<String> {
    wxctl_core::resource_id_field(data, id_field)
}

pub(crate) fn resolve_dependencies(data: &Value, runtime_ids: &RuntimeIdStore, schema: &wxctl_schema::ir::SchemaIr) -> Result<Value> {
    let template = CompiledTemplate::compile(data.clone())?;
    let mut resolver = TemplateResolver::new(runtime_ids);
    let mut resolved = resolver.resolve(&template)?;

    // Apply schema-aware field extraction for references
    // This handles cases like llm: ${model.name} where we need to extract just the "name" field
    apply_field_references(&mut resolved, schema);

    Ok(resolved)
}

/// Partial (lenient) variant of [`resolve_dependencies`] for the reconciliation
/// Deferred path: resolve every `${kind.ref[.field]}` the runtime store can
/// satisfy (deps already reconciled earlier in topo order) and keep the rest as
/// literal `${...}` strings. Field extraction (`apply_field_references`) runs on
/// the resolved subset exactly as in the strict path; unresolved templates are
/// plain strings it never touches. Infallible by design — any structural
/// failure falls back to the original data unchanged, which is byte-identical
/// to the pre-partial-resolution behavior.
pub(crate) fn resolve_dependencies_partial(data: &Value, runtime_ids: &RuntimeIdStore, schema: &wxctl_schema::ir::SchemaIr) -> Value {
    let Ok(template) = CompiledTemplate::compile(data.clone()) else {
        return data.clone();
    };
    let mut resolver = TemplateResolver::new(runtime_ids);
    let Ok(mut resolved) = resolver.resolve_partial(&template) else {
        return data.clone();
    };
    apply_field_references(&mut resolved, schema);
    resolved
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
pub(crate) fn enrich_with_linked_refs(data: &mut Value, raw_data: &Value, runtime_ids: &RuntimeIdStore, schema: &wxctl_schema::ir::SchemaIr, registry: &wxctl_core::ResourceRegistry) {
    enrich_recursive(data, Some(raw_data), runtime_ids, &schema.resource.schema, registry, 0);
}

const MAX_ENRICH_DEPTH: usize = 2;

fn enrich_recursive(data: &mut Value, raw_data: Option<&Value>, runtime_ids: &RuntimeIdStore, schema: &wxctl_schema::ir::SchemaBodyIr, registry: &wxctl_core::ResourceRegistry, depth: usize) {
    if depth > MAX_ENRICH_DEPTH {
        return;
    }
    let Value::Object(map) = data else { return };
    let raw_map_opt = raw_data.and_then(Value::as_object);

    for field in schema.all_fields() {
        let Some(refs) = &field.references else { continue };

        // Prefer raw (pre-template) value for ref-name extraction; fall
        // back to the enriched data after template resolution.
        let raw_val_opt = raw_map_opt.and_then(|m| m.get(field.name)).or_else(|| map.get(field.name));
        let Some(raw_val) = raw_val_opt else { continue };
        let Some(ref_name) = extract_ref_name(raw_val) else { continue };

        let candidates: Vec<&str> = std::iter::once(refs.resource).chain(refs.also_allows.iter().copied()).collect();
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
pub(crate) fn apply_field_references(value: &mut Value, schema: &wxctl_schema::ir::SchemaIr) {
    apply_field_references_recursive(value, &schema.resource.schema, "");
}

fn apply_field_references_recursive(value: &mut Value, schema: &wxctl_schema::ir::SchemaBodyIr, field_path: &str) {
    match value {
        Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                let current_path = if field_path.is_empty() { key.clone() } else { format!("{}.{}", field_path, key) };

                if let Some(field_def) = schema.fields.iter().find(|f| f.name == key.as_str()) {
                    if let Some(refs) = &field_def.references {
                        match field_def.field_type {
                            wxctl_schema::ir::FieldTypeIr::Object => {
                                // Map-of-references: values are terminal UUIDs after extraction
                                if let Value::Object(inner_map) = val {
                                    for inner_val in inner_map.values_mut() {
                                        extract_reference_field(inner_val, refs.field);
                                    }
                                }
                                continue;
                            }
                            _ => {
                                extract_reference_field(val, refs.field);
                            }
                        }
                    }

                    if let Some(nested_schema) = field_def.schema {
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
            // The array's own field name is the LAST segment of the accumulated
            // path — `schema` here is the level that declares it (the Object arm
            // recursed with the nested schema when one exists). Resolving the
            // FIRST segment only worked for top-level arrays: for an array nested
            // deeper (e.g. binding.tools), the first segment names an ancestor the
            // in-scope schema doesn't declare, so a bare `${kind.ref}` item
            // silently skipped id-extraction and leaked the whole resolved object.
            let field_name = field_path.rsplit('.').next().unwrap_or(field_path);
            let field_def = schema.fields.iter().find(|f| f.name == field_name);

            for item in arr.iter_mut() {
                if let Some(fd) = field_def
                    && let Some(refs) = &fd.references
                {
                    extract_reference_field(item, refs.field);
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
        // A create that returns no body (HTTP 204 → response = Null, e.g. every
        // HashiCorp Vault write) must NOT erase the request: downstream
        // `${kind.ref.field}` references resolve against the created resource's own
        // declared fields (a Vault policy's `name`, a mount's `path`), which live only
        // in the request. Keep the request when the response carries nothing.
        (Value::Object(_), Value::Null) => request.clone(),
        // Any other non-object response (a bare scalar/array body) is the server's
        // authoritative representation — prefer it, as before.
        _ => response.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::ResourceKey;
    use wxctl_schema::ir_support::compile_to_static_ir;

    fn make_store(entries: Vec<(&str, &str, Value)>) -> RuntimeIdStore {
        let store = RuntimeIdStore::new();
        for (kind, name, data) in entries {
            store.insert(ResourceKey::new(kind, name), data);
        }
        store
    }

    /// `agent` kind: `name`/`tools`/`style` fields, none carrying `references`.
    const AGENT_YAML: &str = r#"
resource:
  name: agent
  service: test
  kind: agent
  version: v1
  api:
    base_path: /v1/agents
    id_field: id
    list_endpoint: /v1/agents
    get_endpoint: /v1/agents/{id}
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: name
        type: string
      - name: tools
        type: array
      - name: style
        type: string
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    update_strategy: patch
"#;

    /// Same `agent` kind, but `tools` carries a `references` block onto `toolkit.id`,
    /// so whole-object refs resolved into `tools` get field-extracted.
    const AGENT_TOOLS_REF_YAML: &str = r#"
resource:
  name: agent
  service: test
  kind: agent
  version: v1
  api:
    base_path: /v1/agents
    id_field: id
    list_endpoint: /v1/agents
    get_endpoint: /v1/agents/{id}
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: name
        type: string
      - name: tools
        type: array
        references:
          resource: toolkit
          field: id
      - name: style
        type: string
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    update_strategy: patch
"#;

    /// `tool` kind with a two-level nested reference: `binding.openapi.connection_id`
    /// references `orchestrate_connection.app_id`.
    const TOOL_NESTED_STRING_REF_YAML: &str = r#"
resource:
  name: tool
  service: test
  kind: tool
  version: v1
  api:
    base_path: /v1/tools
    id_field: id
    get_endpoint: /v1/tools/{id}
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: name
        type: string
      - name: binding
        type: object
        schema:
          fields:
            - name: openapi
              type: object
              schema:
                fields:
                  - name: connection_id
                    type: string
                    references:
                      resource: orchestrate_connection
                      field: app_id
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    update_strategy: patch
"#;

    /// `agent` kind with `binding.tools`: an array of references to `toolkit.id`
    /// nested one level deep.
    const AGENT_NESTED_ARRAY_REF_YAML: &str = r#"
resource:
  name: agent
  service: test
  kind: agent
  version: v1
  api:
    base_path: /v1/agents
    id_field: id
    get_endpoint: /v1/agents/{id}
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: name
        type: string
      - name: binding
        type: object
        schema:
          fields:
            - name: tools
              type: array
              references:
                resource: toolkit
                field: id
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    update_strategy: patch
"#;

    /// `tool` kind with `binding.python.connections`: a map-of-references field
    /// (values are `orchestrate_connection.connection_id` refs) nested two levels deep.
    const TOOL_BINDING_PYTHON_CONNECTIONS_YAML: &str = r#"
resource:
  name: tool
  service: test
  kind: tool
  version: v1
  api:
    base_path: /v1/tools
    id_field: id
    get_endpoint: /v1/tools/{id}
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: name
        type: string
      - name: binding
        type: object
        schema:
          fields:
            - name: python
              type: object
              schema:
                fields:
                  - name: connections
                    type: object
                    references:
                      resource: orchestrate_connection
                      field: connection_id
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    update_strategy: patch
"#;

    /// `toolkit` kind with `mcp.connections`: a map-of-references field nested one level deep.
    const TOOLKIT_MCP_CONNECTIONS_YAML: &str = r#"
resource:
  name: toolkit
  service: test
  kind: toolkit
  version: v1
  api:
    base_path: /v1/toolkits
    id_field: id
    get_endpoint: /v1/toolkits/{id}
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: name
        type: string
      - name: mcp
        type: object
        schema:
          fields:
            - name: connections
              type: object
              references:
                resource: orchestrate_connection
                field: connection_id
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    update_strategy: patch
"#;

    #[test]
    fn resolve_dependencies_cross_kind_field_path_and_missing() {
        let schema = compile_to_static_ir(AGENT_YAML).unwrap();

        // Simulates reconciliation path: RuntimeIdStore built from cache, agent data
        // with a ${toolkit.x.tools.hello} template; literals pass through unchanged.
        let store = make_store(vec![("toolkit", "hello_toolkit", json!({"id": "tk-123", "name": "hello_toolkit", "tools": {"hello": "tool-uuid-1"}}))]);
        let agent_data = json!({
            "name": "hello_agent",
            "tools": ["${toolkit.hello_toolkit.tools.hello}"],
            "style": "default"
        });
        let resolved = resolve_dependencies(&agent_data, &store, schema).unwrap();
        assert_eq!(resolved["tools"][0], json!("tool-uuid-1"));
        assert_eq!(resolved["name"], json!("hello_agent"));
        assert_eq!(resolved["style"], json!("default"));

        // Missing referenced resource → error.
        let store = RuntimeIdStore::new();
        let result = resolve_dependencies(&json!({"tool": "${toolkit.missing.tools.hello}"}), &store, schema);
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[test]
    fn resolve_dependencies_partial_mixed_store() {
        // Schema with a `tools` ref field so whole-object refs get field-extracted,
        // mirroring the strict path. `style` refs nothing; `name` is a literal.
        let schema = compile_to_static_ir(AGENT_TOOLS_REF_YAML).unwrap();

        // toolkit resolvable, ghost not: the toolkit ref resolves AND extracts the
        // referenced field; the ghost ref survives verbatim; literals unchanged.
        let store = make_store(vec![("toolkit", "tk1", json!({"id": "tk-uuid-1", "name": "tk1"}))]);
        let data = json!({"name": "agent-1", "tools": ["${toolkit.tk1}"], "style": "${ghost.g1.id}"});
        let resolved = resolve_dependencies_partial(&data, &store, schema);
        assert_eq!(resolved["tools"][0], json!("tk-uuid-1"));
        assert_eq!(resolved["style"], json!("${ghost.g1.id}"));
        assert_eq!(resolved["name"], json!("agent-1"));

        // Empty store (first apply): output is byte-identical to the input.
        let empty = RuntimeIdStore::new();
        assert_eq!(resolve_dependencies_partial(&data, &empty, schema), data);
    }

    #[test]
    fn nested_string_reference_extracted() {
        let store = make_store(vec![("orchestrate_connection", "my_conn", json!({"app_id": "my-app", "connection_id": "uuid-123", "name": "my_conn"}))]);
        let schema = compile_to_static_ir(TOOL_NESTED_STRING_REF_YAML).unwrap();

        let data = json!({
            "name": "my_tool",
            "binding": {
                "openapi": {
                    "connection_id": "${orchestrate_connection.my_conn}"
                }
            }
        });

        let resolved = resolve_dependencies(&data, &store, schema).unwrap();
        assert_eq!(resolved["binding"]["openapi"]["connection_id"], "my-app");
    }

    #[test]
    fn nested_array_of_references_extracted() {
        // Array of refs nested one level down (binding.tools): the Array arm must
        // resolve the field definition from the LAST path segment against the
        // schema in scope — with the first segment, a bare ${kind.ref} item kept
        // the whole resolved object instead of the extracted id.
        let store = make_store(vec![("toolkit", "tk1", json!({"id": "tk-uuid-1", "name": "tk1"}))]);
        let schema = compile_to_static_ir(AGENT_NESTED_ARRAY_REF_YAML).unwrap();

        let data = json!({"name": "my_agent", "binding": {"tools": ["${toolkit.tk1}"]}});
        let resolved = resolve_dependencies(&data, &store, schema).unwrap();
        assert_eq!(resolved["binding"]["tools"][0], json!("tk-uuid-1"));
    }

    #[test]
    fn map_of_references_branches() {
        // Single map entry whose ${...} value is replaced by the referenced field (connection_id).
        let store = make_store(vec![("orchestrate_connection", "my_conn", json!({"connection_id": "uuid-abc", "app_id": "my-svc", "name": "my_conn"}))]);
        let schema = compile_to_static_ir(TOOL_BINDING_PYTHON_CONNECTIONS_YAML).unwrap();
        let data = json!({"name": "my_tool", "binding": {"python": {"connections": {"my-svc": "${orchestrate_connection.my_conn}"}}}});
        let resolved = resolve_dependencies(&data, &store, schema).unwrap();
        assert_eq!(resolved["binding"]["python"]["connections"]["my-svc"], "uuid-abc");

        // Multiple map entries each independently resolved.
        let store = make_store(vec![("orchestrate_connection", "conn_a", json!({"connection_id": "uuid-a"})), ("orchestrate_connection", "conn_b", json!({"connection_id": "uuid-b"}))]);
        let schema = compile_to_static_ir(TOOLKIT_MCP_CONNECTIONS_YAML).unwrap();
        let data = json!({"name": "my_toolkit", "mcp": {"connections": {"svc-a": "${orchestrate_connection.conn_a}", "svc-b": "${orchestrate_connection.conn_b}"}}});
        let resolved = resolve_dependencies(&data, &store, schema).unwrap();
        assert_eq!(resolved["mcp"]["connections"]["svc-a"], "uuid-a");
        assert_eq!(resolved["mcp"]["connections"]["svc-b"], "uuid-b");

        // A plain (non-template) string value is left unchanged.
        let store = RuntimeIdStore::new();
        let schema = compile_to_static_ir(TOOLKIT_MCP_CONNECTIONS_YAML).unwrap();
        let data = json!({"name": "my_toolkit", "mcp": {"connections": {"svc-a": "already-a-uuid"}}});
        let resolved = resolve_dependencies(&data, &store, schema).unwrap();
        assert_eq!(resolved["mcp"]["connections"]["svc-a"], "already-a-uuid");
    }
}
