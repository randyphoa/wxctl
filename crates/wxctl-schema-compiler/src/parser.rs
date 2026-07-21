use crate::definition::ResourceSchema;
use anyhow::{Context, Result};
use serde_norway::Value;

pub struct SchemaParser;

impl SchemaParser {
    pub fn parse_str(yaml: &str) -> Result<ResourceSchema> {
        let mut raw: Value = serde_norway::from_str(yaml).context("Failed to parse YAML")?;
        normalize_properties(&mut raw);
        let mut schema: ResourceSchema = serde_norway::from_value(raw).context("Failed to deserialize schema")?;

        // Auto-compute state_fields when not explicitly set in YAML.
        if schema.resource.reconciliation.state_fields.is_none() {
            let mut computed = schema.resource.schema.compute_state_fields();
            // Identity-hash kinds: the hash IS the identity. Drop the discovery name
            // field and every hashed input field (both subsumed by the hash) from the
            // default state_fields, and inject the synthetic `identity_hash` field.
            // A matched remote does not echo the input fields, so leaving them in
            // would produce a phantom Update (see openscale-monitor-instance-reapply
            // troubleshoot). A changed input still re-runs — it changes the hash,
            // hence the suffixed name / tag, so discovery finds no match → Create.
            if let Some(ih) = &schema.resource.reconciliation.identity_hash {
                let name_field = schema.resource.reconciliation.discovery.name_field.clone().unwrap_or_else(|| "name".to_string());
                computed.retain(|f| f != &name_field && !ih.fields.contains(f));
                computed.push("identity_hash".to_string());
            }
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
/// The transform is scoped to field-definition contexts: it only rewrites a
/// `properties:` key when the same mapping also declares `type:` (every field
/// definition does — `type` is a required attribute), and it never descends into
/// `default:` values. A `properties` key inside a `default:` literal (e.g. a
/// JSON-Schema default) or in other free-form data is left untouched.
fn normalize_properties(value: &mut Value) {
    match value {
        Value::Mapping(map) => {
            if map.contains_key(Value::String("type".into()))
                && let Some(props_val) = map.remove(Value::String("properties".into()))
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

            for (key, val) in map.iter_mut() {
                // `default:` holds a literal value, not field definitions.
                if key.as_str() == Some("default") {
                    continue;
                }
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
    fn normalize_properties_leaves_default_literals_alone() {
        // A JSON-Schema literal inside `default:` carries its own `type` +
        // `properties` keys — it must survive verbatim, not be rewritten into a
        // nested `schema:` block.
        let yaml = r#"
resource:
    name: t
    service: s
    kind: t
    version: v1
    api:
        base_path: /v1/t
        id_field: id
        get_endpoint: /v1/t/{id}
        create_method: POST
        delete_method: DELETE
    schema:
        fields:
            - name: input_schema
              type: object
              default:
                  type: object
                  properties:
                      query:
                          type: string
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        update_strategy: replace
"#;
        let schema = SchemaParser::parse_str(yaml).unwrap();
        let field = schema.resource.schema.fields.iter().find(|f| f.name == "input_schema").unwrap();
        assert!(field.schema.is_none(), "default literal must not synthesize a nested schema");
        let default = field.default.as_ref().unwrap();
        assert_eq!(default["properties"]["query"]["type"], serde_json::json!("string"), "default literal must be preserved verbatim, got {default}");
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

    #[test]
    fn identity_hash_block_round_trips_and_reshapes_state_fields() {
        use crate::definition::HashStorage;
        let yaml = r#"
resource:
    name: autoai_experiment
    service: watsonx_ai
    kind: autoai_experiment
    version: v1
    api:
        base_path: /ml/v4/trainings
        id_field: id
        get_endpoint: /ml/v4/trainings/{id}
        create_method: POST
        delete_method: DELETE
    schema:
        fields:
            - name: name
              type: string
            - name: training_data
              type: string
            - name: scoring
              type: string
            - name: description
              type: string
            - name: generation
              type: string
              location: LocalOnly
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        update_strategy: recreate
        identity_hash:
            fields: [training_data, scoring]
            nonce_field: generation
            storage: name_suffix
            length: 8
"#;
        let schema = SchemaParser::parse_str(yaml).unwrap();
        let ih = schema.resource.reconciliation.identity_hash.as_ref().expect("identity_hash block parsed");
        assert_eq!(ih.fields, vec!["training_data".to_string(), "scoring".to_string()]);
        assert_eq!(ih.nonce_field.as_deref(), Some("generation"));
        assert!(matches!(ih.storage, HashStorage::NameSuffix));
        assert_eq!(ih.length, 8);

        let sf = schema.resource.reconciliation.state_fields.as_ref().unwrap();
        assert!(!sf.contains(&"name".to_string()), "name excluded from state_fields");
        assert!(!sf.contains(&"training_data".to_string()), "hashed field folded into identity_hash → excluded");
        assert!(!sf.contains(&"scoring".to_string()), "hashed field folded into identity_hash → excluded");
        assert!(!sf.contains(&"generation".to_string()), "LocalOnly nonce never in state_fields");
        assert!(sf.contains(&"identity_hash".to_string()), "synthetic identity_hash injected");
        assert!(sf.contains(&"description".to_string()), "non-hashed body field still compared");
    }

    #[test]
    fn identity_hash_storage_local_parses() {
        use crate::definition::HashStorage;
        let yaml = r#"
resource:
  name: sal_like
  service: watsonx_data
  kind: sal_like
  version: v1
  api:
    base_path: /v3/x
    id_field: id
    get_endpoint: /v3/x
    create_endpoint: /v3/x
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: id
        type: string
        required: false
        location: Computed
      - name: changes
        type: array
        required: true
      - name: generation
        type: string
        required: false
        location: LocalOnly
  reconciliation:
    discovery:
      method: skip
    identity_hash:
      fields: [changes]
      nonce_field: generation
      storage: local
    state_fields: []
    update_strategy: recreate
    use_json_patch: false
    immutable_fields: []
  deployments: {}
  unsupported_on: []
"#;
        let schema = SchemaParser::parse_str(yaml).unwrap();
        let recon = &schema.resource.reconciliation;
        let ih = recon.identity_hash.as_ref().expect("identity_hash parsed");
        assert!(matches!(ih.storage, HashStorage::Local));
        assert_eq!(ih.nonce_field.as_deref(), Some("generation"));
        // Explicit state_fields: [] bypasses the parser reshaping — no synthetic
        // identity_hash state field (compare over [] ⇒ NoChange on identical data).
        assert_eq!(recon.state_fields.as_ref().unwrap(), &Vec::<String>::new());
    }

    #[test]
    fn identity_hash_length_defaults_to_eight_and_storage_defaults_name_suffix() {
        use crate::definition::HashStorage;
        let yaml = r#"
resource:
    name: k
    service: s
    kind: k
    version: v1
    api:
        base_path: /v1/k
        id_field: id
        get_endpoint: /v1/k/{id}
        create_method: POST
        delete_method: DELETE
    schema:
        fields:
            - name: name
              type: string
            - name: input
              type: string
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        update_strategy: recreate
        identity_hash:
            fields: [input]
"#;
        let ih = SchemaParser::parse_str(yaml).unwrap().resource.reconciliation.identity_hash.unwrap();
        assert_eq!(ih.length, 8);
        assert!(matches!(ih.storage, HashStorage::NameSuffix));
        assert!(ih.nonce_field.is_none());
    }

    #[test]
    fn schema_without_identity_hash_block_is_unchanged() {
        // A kind with no identity_hash block: identity_hash is None and `name`
        // stays in the default state_fields (no regression — Invariant I3 / AC6).
        let yaml = r#"
resource:
    name: plain
    service: s
    kind: plain
    version: v1
    api:
        base_path: /v1/plain
        id_field: id
        get_endpoint: /v1/plain/{id}
        create_method: POST
        delete_method: DELETE
    schema:
        fields:
            - name: name
              type: string
            - name: value
              type: string
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        update_strategy: patch
"#;
        let schema = SchemaParser::parse_str(yaml).unwrap();
        assert!(schema.resource.reconciliation.identity_hash.is_none());
        let sf = schema.resource.reconciliation.state_fields.as_ref().unwrap();
        assert!(sf.contains(&"name".to_string()));
        assert!(!sf.contains(&"identity_hash".to_string()));
    }
}
