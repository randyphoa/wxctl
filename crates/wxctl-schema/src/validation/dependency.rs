use super::types::ValidationError;
use crate::schema::{FieldDefinition, ResourceSchema};
use std::cell::OnceCell;
use std::collections::HashSet;
use wxctl_graph::{DependencyEdge, ResourceKey, extract_references_with_path, istr, parse_reference};

/// Walk `fields` (recursively into nested object schemas) collecting every
/// referenced resource kind — including `also_allows`. Nested refs (e.g.
/// `storage_registration.connection.name → cos_bucket`) are discovered
/// alongside top-level refs so cross-resource references at any depth are
/// recognised by dependency extraction.
fn collect_allowed_kinds<'a>(fields: &'a [FieldDefinition], out: &mut HashSet<&'a str>) {
    for field in fields {
        if let Some(refs) = &field.references {
            out.insert(refs.resource.as_str());
            for also in &refs.also_allows {
                out.insert(also.as_str());
            }
        }
        if let Some(nested) = &field.schema {
            collect_allowed_kinds(&nested.fields, out);
        }
    }
}

/// Result of dependency extraction.
pub struct DependencyExtractionResult {
    /// Successfully extracted dependencies.
    pub dependencies: Vec<ResourceKey>,
    /// Dependency edges with field path information (for error messages/visualization).
    pub edges: Vec<DependencyEdge>,
    /// Validation errors encountered (invalid dependency kinds).
    pub errors: Vec<ValidationError>,
}

/// Extract actual dependencies from ${kind.name} references in resource data.
/// Validates each dependency against schema's allowed dependencies.
///
/// Returns extracted dependencies, edges with field paths, and any validation errors,
/// allowing the caller to decide how to handle partial results.
///
/// # Arguments
/// * `from_key` - The resource key that has the dependencies (for building edges)
/// * `data` - The JSON data to extract references from
/// * `schema` - The resource schema for dependency validation
/// * `available_resources` - List of available resources for existence check
pub fn extract_dependencies(
    from_key: &ResourceKey,
    data: &serde_json::Value,
    schema: &ResourceSchema,
    available_resources: &[(ResourceKey, String)], // (key, kind) pairs
) -> DependencyExtractionResult {
    let mut seen: HashSet<ResourceKey> = HashSet::new();
    let mut dependencies = Vec::new();
    let mut edges = Vec::new();
    let mut errors: Vec<ValidationError> = Vec::new();

    // Build set of allowed dependency kinds from field-level references,
    // walking nested object schemas so refs at any depth are recognised.
    let mut allowed_kinds: HashSet<&str> = HashSet::new();
    collect_allowed_kinds(&schema.resource.schema.fields, &mut allowed_kinds);

    // Lazily allocate allowed_kinds_vec only when first error occurs
    let allowed_kinds_vec: OnceCell<Vec<String>> = OnceCell::new();

    // Build set of available resource keys for O(1) lookup
    let available_keys: HashSet<&ResourceKey> = available_resources.iter().map(|(k, _)| k).collect();

    // Extract all ${...} references using unified extractor with field paths
    extract_references_with_path(data, "", &mut |ref_str, field_path| {
        if let Some(dep_key) = parse_reference(ref_str) {
            // Validate: is this dependency kind allowed by schema?
            if !allowed_kinds.is_empty() && !allowed_kinds.contains(dep_key.kind.as_ref()) {
                // Only allocate allowed_kinds_vec on first error
                let kinds = allowed_kinds_vec.get_or_init(|| allowed_kinds.iter().map(|s| s.to_string()).collect());
                errors.push(ValidationError::InvalidDependency { field_path: field_path.to_string(), ref_kind: dep_key.kind.to_string(), ref_name: dep_key.name.to_string(), allowed_kinds: kinds.clone() });
                return;
            }

            // Validate: does the referenced resource exist? (O(1) lookup)
            if !available_keys.contains(&dep_key) {
                // Not an error - could be an external reference resolved at runtime
                return;
            }

            // O(1) dedup check
            if seen.insert(dep_key.clone()) {
                // Create edge with field path for visualization/error messages
                edges.push(DependencyEdge { from: from_key.clone(), to: dep_key.clone(), field_path: istr(field_path) });
                dependencies.push(dep_key);
            }
        }
    });

    DependencyExtractionResult { dependencies, edges, errors }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldReferences, FieldType, HookDefinition, HttpMethod, ReconciliationDefinition, ResourceDefinition, SchemaDefinition, UpdateStrategy};
    use serde_json::json;

    fn make_field_with_ref(name: &str, ref_resource: &str) -> FieldDefinition {
        FieldDefinition {
            name: name.into(),
            field_type: FieldType::String,
            required: false,
            immutable: false,
            location: crate::schema::FieldLocation::default(),
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references: Some(FieldReferences { resource: ref_resource.into(), field: "id".into(), also_allows: vec![], optional: false }),
            api_field: None,
            sensitive: false,
            also_query: false,
            is_path: false,
            properties: None,
        }
    }

    fn make_schema(fields: Vec<FieldDefinition>) -> ResourceSchema {
        ResourceSchema {
            resource: ResourceDefinition {
                name: "test".into(),
                service: "test".into(),
                kind: "test".into(),
                version: "v1".into(),
                api: ApiDefinition {
                    base_path: "/api/test".into(),
                    id_field: "id".into(),
                    list_endpoint: None,
                    get_endpoint: "/api/test/{id}".into(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: None,
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                },
                schema: SchemaDefinition { fields, ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::GetById, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, id_source: "id".into() },
                    state_fields: Some(vec![]),
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
    fn extract_dep_allowed_kind_builds_dependency_and_edge() {
        let schema = make_schema(vec![make_field_with_ref("catalog_id", "catalog")]);
        let from_key = ResourceKey::new("test", "my-test");
        let available = vec![(ResourceKey::new("catalog", "my-cat"), "catalog".into())];

        let result = extract_dependencies(&from_key, &json!({"catalog_id": "${catalog.my-cat}"}), &schema, &available);

        assert!(result.errors.is_empty());
        assert_eq!(result.dependencies, vec![ResourceKey::new("catalog", "my-cat")]);
        assert_eq!(result.edges.len(), 1);
        assert_eq!(result.edges[0].from, from_key);
        assert_eq!(result.edges[0].to, ResourceKey::new("catalog", "my-cat"));
    }

    #[test]
    fn extract_dep_disallowed_kind() {
        let schema = make_schema(vec![make_field_with_ref("catalog_id", "catalog")]);
        let from_key = ResourceKey::new("test", "my-test");
        let available = vec![(ResourceKey::new("connection", "my-conn"), "connection".into())];

        let result = extract_dependencies(&from_key, &json!({"catalog_id": "${connection.my-conn}"}), &schema, &available);

        assert_eq!(result.errors.len(), 1);
        assert!(matches!(
            &result.errors[0],
            ValidationError::InvalidDependency { ref_kind, .. } if ref_kind == "connection"
        ));
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn extract_dep_dependency_counts() {
        // Each row: (schema, from_key, available, data, expected dep count, why).
        // All must produce zero errors — only the resolved dependency set varies.
        let catalog_schema = || make_schema(vec![make_field_with_ref("catalog_id", "catalog")]);
        let test_key = ResourceKey::new("test", "my-test");
        let cat_avail = vec![(ResourceKey::new("catalog", "my-cat"), "catalog".to_string())];

        // also_allows: a wml_function ref accepted because it's in `also_allows`.
        let mut also_field = make_field_with_ref("asset", "ai_service");
        also_field.references.as_mut().unwrap().also_allows = vec!["wml_function".into()];
        let also_schema = make_schema(vec![also_field]);

        let cases: Vec<(ResourceSchema, ResourceKey, Vec<(ResourceKey, String)>, serde_json::Value, usize, &str)> = vec![
            // Ref not in `available` → skipped (resolved later), no dep, no error.
            (catalog_schema(), test_key.clone(), vec![], json!({"catalog_id": "${catalog.external}"}), 0, "ref absent from available"),
            // Same reference in two fields → deduped to one dependency.
            (catalog_schema(), test_key.clone(), cat_avail.clone(), json!({"catalog_id": "${catalog.my-cat}", "other_field": "${catalog.my-cat}"}), 1, "duplicate refs deduped"),
            // also_allows kind accepted as a dependency.
            (also_schema, ResourceKey::new("wml_deployment", "my-deploy"), vec![(ResourceKey::new("wml_function", "my-fn"), "wml_function".to_string())], json!({"asset": "${wml_function.my-fn}"}), 1, "also_allows kind accepted"),
            // Plain (non-template) value → no reference, no dep, no edge.
            (catalog_schema(), test_key.clone(), vec![], json!({"catalog_id": "plain-value"}), 0, "plain value is not a ref"),
        ];
        for (schema, from_key, available, data, want, why) in cases {
            let result = extract_dependencies(&from_key, &data, &schema, &available);
            assert!(result.errors.is_empty(), "{why}: unexpected errors {:?}", result.errors);
            assert_eq!(result.dependencies.len(), want, "{why}: dep count");
            if want == 0 {
                assert!(result.edges.is_empty(), "{why}: no edges when no deps");
            }
        }
    }
}
