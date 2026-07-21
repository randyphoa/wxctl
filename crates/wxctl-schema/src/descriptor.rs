use crate::ir::{FieldLocationIr, HttpMethodIr, SchemaIr};

#[derive(Debug)]
pub struct ResourceDescriptor {
    pub name: String,
    pub service: String,
    pub kind: String,
    pub id_field: String,
    pub endpoints: Endpoints,
    pub fields: Vec<FieldDescriptor>,
    pub schema: &'static SchemaIr,
}

#[derive(Debug)]
pub struct Endpoints {
    pub base_path: String,
    pub list: Option<String>,
    pub get: String,
    pub create: String,
    pub update: Option<String>,
    pub update_method: Option<HttpMethodIr>,
    pub delete: String,
}

#[derive(Debug)]
pub struct FieldDescriptor {
    pub name: String,
    pub required: bool,
    pub immutable: bool,
    pub location: FieldLocationIr,
}

impl ResourceDescriptor {
    pub fn from_ir(schema: &'static SchemaIr) -> Self {
        let def = &schema.resource;
        Self {
            name: def.name.to_string(),
            service: def.service.to_string(),
            kind: def.kind.to_string(),
            id_field: def.api.id_field.to_string(),
            endpoints: Endpoints {
                base_path: def.api.base_path.to_string(),
                list: def.api.list_endpoint.map(str::to_string),
                get: def.api.get_endpoint.to_string(),

                // Use custom create_endpoint or fall back to base_path
                create: def.api.create_endpoint.unwrap_or(def.api.base_path).to_string(),

                // Use custom update_endpoint or fall back to get_endpoint
                update: def.api.update_method.map(|_| def.api.update_endpoint.unwrap_or(def.api.get_endpoint).to_string()),

                update_method: def.api.update_method,

                // Use custom delete_endpoint or fall back to get_endpoint
                delete: def.api.delete_endpoint.unwrap_or(def.api.get_endpoint).to_string(),
            },
            fields: def.schema.fields.iter().map(|f| FieldDescriptor { name: f.name.to_string(), required: f.required, immutable: f.immutable, location: f.location }).collect(),
            schema,
        }
    }
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;

    const MINIMAL_YAML: &str = r#"
resource:
  name: test_resource
  service: test_service
  kind: test_kind
  version: v1
  api:
    base_path: /v1/resources
    id_field: resource_id
    get_endpoint: /v1/resources/{id}
    create_method: POST
    update_method: PATCH
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
    update_strategy: patch
"#;

    const MINIMAL_YAML_NO_UPDATE: &str = r#"
resource:
  name: test_resource
  service: test_service
  kind: test_kind
  version: v1
  api:
    base_path: /v1/resources
    id_field: resource_id
    get_endpoint: /v1/resources/{id}
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
    update_strategy: patch
"#;

    #[test]
    fn test_endpoint_fallbacks() {
        // With an update_method: create falls back to base_path, update/delete
        // fall back to get_endpoint.
        let schema = crate::ir_support::compile_to_static_ir(MINIMAL_YAML).unwrap();
        let desc = ResourceDescriptor::from_ir(schema);
        assert_eq!(desc.endpoints.create, "/v1/resources");
        assert_eq!(desc.endpoints.update.unwrap(), "/v1/resources/{id}");
        assert_eq!(desc.endpoints.delete, "/v1/resources/{id}");

        // No update_method → no update endpoint at all.
        let no_update = crate::ir_support::compile_to_static_ir(MINIMAL_YAML_NO_UPDATE).unwrap();
        let desc = ResourceDescriptor::from_ir(no_update);
        assert!(desc.endpoints.update.is_none());
    }
}
