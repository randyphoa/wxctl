use super::definition::ResourceSchema;
use anyhow::{Context, Result};
use serde_norway::Value;

pub struct SchemaParser;

impl SchemaParser {
    pub fn parse_str(yaml: &str) -> Result<ResourceSchema> {
        let mut raw: Value = serde_norway::from_str(yaml).context("Failed to parse YAML")?;
        normalize_properties(&mut raw);
        let mut schema: ResourceSchema = serde_norway::from_value(raw).context("Failed to deserialize schema")?;

        // Auto-compute state_fields when not explicitly set in YAML
        if schema.resource.reconciliation.state_fields.is_none() {
            let computed = schema.resource.schema.compute_state_fields();
            schema.resource.reconciliation.state_fields = Some(computed);
        }

        Ok(schema)
    }
}

/// Recursively convert YAML `properties:` maps into `schema: {fields: [...]}`.
///
/// The YAML schema format uses `properties:` as a map of name -> field attrs for nested
/// field definitions (matching JSON Schema conventions). But our Rust types expect
/// `schema: {fields: [{name: ..., ...}]}`. This normalizer bridges the gap so that
/// nested `references:` metadata is preserved through deserialization.
///
/// NOTE: Fields named "properties" (e.g. JSON Schema's `properties` keyword inside
/// `input_schema`) are also converted to nested FieldDefinitions. This is safe because
/// only fields with `references` annotations trigger extraction — nested fields without
/// `references` have no side effects.
fn normalize_properties(value: &mut Value) {
    match value {
        Value::Mapping(map) => {
            if let Some(props_val) = map.remove(Value::String("properties".into()))
                && let Value::Mapping(props_map) = props_val
            {
                let fields: Vec<Value> = props_map
                    .into_iter()
                    .map(|(key, mut attrs)| {
                        normalize_properties(&mut attrs);
                        if let Value::Mapping(ref mut attr_map) = attrs {
                            attr_map.insert(Value::String("name".into()), key);
                        }
                        attrs
                    })
                    .collect();

                // Explicit `schema:` takes precedence over `properties:`
                if !map.contains_key(Value::String("schema".into())) {
                    let mut schema_map = serde_norway::Mapping::new();
                    schema_map.insert(Value::String("fields".into()), Value::Sequence(fields));
                    map.insert(Value::String("schema".into()), Value::Mapping(schema_map));
                }
            }

            for val in map.values_mut() {
                normalize_properties(val);
            }
        }
        Value::Sequence(seq) => {
            for item in seq.iter_mut() {
                normalize_properties(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nested_properties_parsed_into_schema() {
        let yaml = r#"
resource:
    name: test_tool
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
            - name: binding
              type: object
              properties:
                  python:
                      type: object
                      properties:
                          function:
                              type: string
                          connections:
                              type: object
                              default: {}
                              references:
                                  resource: orchestrate_connection
                                  field: connection_id
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        update_strategy: replace
"#;
        let schema = SchemaParser::parse_str(yaml).unwrap();
        let binding = schema.resource.schema.fields.iter().find(|f| f.name == "binding").unwrap();
        let binding_schema = binding.schema.as_ref().expect("binding should have nested schema from properties");
        let python = binding_schema.fields.iter().find(|f| f.name == "python").unwrap();
        let python_schema = python.schema.as_ref().expect("python should have nested schema from properties");
        let connections = python_schema.fields.iter().find(|f| f.name == "connections").unwrap();
        let refs = connections.references.as_ref().expect("connections should have references");
        assert_eq!(refs.resource, "orchestrate_connection");
        assert_eq!(refs.field, "connection_id");
    }

    #[test]
    fn test_top_level_schema_fields_still_work() {
        let yaml = r#"
resource:
    name: test
    service: test
    kind: test
    version: v1
    api:
        base_path: /v1/test
        id_field: id
        get_endpoint: /v1/test/{id}
        create_method: POST
        delete_method: DELETE
    schema:
        fields:
            - name: name
              type: string
              required: true
            - name: connection_id
              type: string
              references:
                  resource: orchestrate_connection
                  field: asset_id
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        update_strategy: replace
"#;
        let schema = SchemaParser::parse_str(yaml).unwrap();
        assert_eq!(schema.resource.schema.fields.len(), 2);
        let conn = schema.resource.schema.fields.iter().find(|f| f.name == "connection_id").unwrap();
        let refs = conn.references.as_ref().unwrap();
        assert_eq!(refs.resource, "orchestrate_connection");
        assert_eq!(refs.field, "asset_id");
    }

    #[test]
    fn business_term_does_not_immutable_compare_parent_category() {
        let yaml = include_str!("../schemas/common_core/business_term.yaml");
        let schema = SchemaParser::parse_str(yaml).expect("parse business_term schema");
        assert!(!schema.resource.reconciliation.immutable_fields.iter().any(|f| f == "parent_category"));
    }

    #[test]
    fn test_deployments_and_unsupported_on_parse() {
        let yaml = r#"
resource:
    name: test_kind
    service: test
    kind: test_kind
    version: v1
    api:
        base_path: /v2/test
        id_field: id
        get_endpoint: /v2/test/{id}
        create_method: POST
        delete_method: DELETE
    schema:
        fields:
            - name: name
              type: string
              required: true
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        update_strategy: replace
    deployments:
        software:
            api:
                base_path: /v2/zen-test
        "software-5.3":
            api:
                base_path: /v2/zen-test-5-3
    unsupported_on:
        - "software-<5.3"
"#;
        let schema = SchemaParser::parse_str(yaml).unwrap();
        let r = &schema.resource;
        let deployments = r.deployments.as_ref().expect("deployments map should parse");
        assert_eq!(deployments.len(), 2);
        assert!(deployments.contains_key("software"));
        assert!(deployments.contains_key("software-5.3"));
        assert_eq!(r.unsupported_on.len(), 1);
    }
}
