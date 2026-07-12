use crate::schema::{FieldLocation, HttpMethod, ResourceSchema};
use anyhow::Result;

#[derive(Debug)]
pub struct ResourceDescriptor {
    pub name: String,
    pub service: String,
    pub kind: String,
    pub id_field: String,
    pub endpoints: Endpoints,
    pub fields: Vec<FieldDescriptor>,
    pub schema: ResourceSchema,
}

#[derive(Debug)]
pub struct Endpoints {
    pub base_path: String,
    pub list: Option<String>,
    pub get: String,
    pub create: String,
    pub update: Option<String>,
    pub update_method: Option<HttpMethod>,
    pub delete: String,
}

#[derive(Debug)]
pub struct FieldDescriptor {
    pub name: String,
    pub required: bool,
    pub immutable: bool,
    pub location: FieldLocation,
}

impl ResourceDescriptor {
    pub fn from_schema(schema: &ResourceSchema) -> Result<Self> {
        let def = &schema.resource;
        Ok(Self {
            name: def.name.clone(),
            service: def.service.clone(),
            kind: def.kind.clone(),
            id_field: def.api.id_field.clone(),
            endpoints: Endpoints {
                base_path: def.api.base_path.clone(),
                list: def.api.list_endpoint.clone(),
                get: def.api.get_endpoint.clone(),

                // Use custom create_endpoint or fall back to base_path
                create: def.api.create_endpoint.as_ref().unwrap_or(&def.api.base_path).clone(),

                // Use custom update_endpoint or fall back to get_endpoint
                update: def.api.update_method.as_ref().map(|_| def.api.update_endpoint.as_ref().unwrap_or(&def.api.get_endpoint).clone()),

                update_method: def.api.update_method.clone(),

                // Use custom delete_endpoint or fall back to get_endpoint
                delete: def.api.delete_endpoint.as_ref().unwrap_or(&def.api.get_endpoint).clone(),
            },
            fields: def.schema.fields.iter().map(|f| FieldDescriptor { name: f.name.clone(), required: f.required, immutable: f.immutable, location: f.location.clone() }).collect(),
            schema: schema.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    fn make_minimal_schema() -> ResourceSchema {
        ResourceSchema {
            resource: ResourceDefinition {
                name: "test_resource".to_string(),
                service: "test_service".to_string(),
                kind: "test_kind".to_string(),
                version: "v1".to_string(),
                api: ApiDefinition {
                    base_path: "/v1/resources".to_string(),
                    id_field: "resource_id".to_string(),
                    list_endpoint: None,
                    get_endpoint: "/v1/resources/{id}".to_string(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: Some(HttpMethod::Patch),
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                    readiness: None,
                },
                schema: SchemaDefinition {
                    fields: vec![FieldDefinition {
                        name: "name".to_string(),
                        field_type: FieldType::String,
                        required: true,
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
                        properties: None,
                        is_path: false,
                        synthesize: None,
                        synth_shape: None,
                    }],
                    ..Default::default()
                },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, list_map: false, id_source: "id".to_string() },
                    state_fields: None,
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec![],
                    reject_on_immutable_drift: false,
                    use_json_patch: true,
                    json_patch_path_prefix: None,
                    identity_hash: None,
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
    fn test_endpoint_fallbacks() {
        // With an update_method: create falls back to base_path, update/delete
        // fall back to get_endpoint.
        let desc = ResourceDescriptor::from_schema(&make_minimal_schema()).unwrap();
        assert_eq!(desc.endpoints.create, "/v1/resources");
        assert_eq!(desc.endpoints.update.unwrap(), "/v1/resources/{id}");
        assert_eq!(desc.endpoints.delete, "/v1/resources/{id}");

        // No update_method → no update endpoint at all.
        let mut no_update = make_minimal_schema();
        no_update.resource.api.update_method = None;
        let desc = ResourceDescriptor::from_schema(&no_update).unwrap();
        assert!(desc.endpoints.update.is_none());
    }
}
