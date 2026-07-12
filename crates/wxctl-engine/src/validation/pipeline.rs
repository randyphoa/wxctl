use super::cross_resource;
use super::dependency::extract_dependencies;
use super::schema::{apply_defaults, check_duplicate_names, validate_schema};
use super::types::{AnnotatedValidationError, ValidationError, ValidationResult};
use anyhow::Result;
use std::collections::HashSet;
use std::sync::Arc;
use tracing::{Instrument, info_span};
use wxctl_core::logging::redact_for_log;
use wxctl_core::registry::descriptor::ResourceDescriptor;
use wxctl_core::{ClientFactory, DependencyEdge, OnDestroyPolicy, RawResource, ResourceKey, ResourceRegistry, ValidatedResource};
use wxctl_schema::validation::{dereference_id_field, normalize_raw_resource_fields};

/// Resolve the deployment-effective `ResourceDescriptor` for validation.
///
/// When the kind has no `deployments:` block (the common case) the base
/// descriptor is returned unchanged. When an overlay applies, rebuilds the
/// descriptor from the merged schema so normalization/defaults/validation
/// see the overlay-applied field names — required for Variant B parallel
/// schemas (e.g. `ingestion_job.software.yaml`) whose field names diverge
/// from the SaaS base.
///
/// Returns the base descriptor unchanged when no `client_factory` is
/// available (e.g. `wxctl validate` without a profile) or when the active
/// deployment for the service can't be resolved — those paths surface
/// elsewhere as R006 rather than as a silent fallthrough.
fn effective_descriptor(base: &Arc<ResourceDescriptor>, client_factory: Option<&Arc<ClientFactory>>) -> Result<Arc<ResourceDescriptor>> {
    let base_def = &base.schema.resource;
    if base_def.deployments.is_none() {
        return Ok(base.clone());
    }
    let Some(cf) = client_factory else {
        return Ok(base.clone());
    };
    let deployment = match cf.deployment_for_service(&base_def.service) {
        Ok(d) => d,
        Err(_) => return Ok(base.clone()),
    };
    let effective_def = wxctl_schema::schema::effective_definition(base_def, &deployment)?;
    let mut merged_schema = base.schema.clone();
    merged_schema.resource = effective_def;
    Ok(Arc::new(ResourceDescriptor::from_schema(&merged_schema)?))
}

/// Build a resource identity string like "tool/my_tool" for error messages.
fn resource_label(resource: &RawResource) -> String {
    let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    format!("{}/{}", resource.kind, ref_name)
}

pub struct ValidationPipeline {
    registry: Arc<ResourceRegistry>,
    client_factory: Option<Arc<ClientFactory>>,
}

impl ValidationPipeline {
    pub fn new(registry: Arc<ResourceRegistry>, client_factory: Option<Arc<ClientFactory>>) -> Self {
        Self { registry, client_factory }
    }

    pub async fn validate(&self, operation_id: &str, resources: &mut [RawResource], skip_post_validate: bool) -> Result<ValidationResult> {
        let span = info_span!(
            target: "wxctl::stage::validation",
            "validation",
            operation_id = %operation_id,
            resource_count = resources.len(),
            status = tracing::field::Empty
        );

        async {
            let mut errors: Vec<AnnotatedValidationError> = Vec::new();
            let mut validated_resources = Vec::new();
            let mut all_edges: Vec<DependencyEdge> = Vec::new();

            // Stage 1: Check for duplicate names
            if let Err(e) = check_duplicate_names(resources) {
                wxctl_core::log_error!(operation_id, "validation", wxctl_core::logging::error_codes::V001, &e.to_string(), "Ensure all resources have unique names within their type");
                errors.push(AnnotatedValidationError { resource: String::new(), error: e });
                tracing::Span::current().record("status", "failed");
                tracing::debug!(target: "wxctl::substage::validation", status = "failed", "validation stage failed");
                return Ok(ValidationResult::failure(errors));
            }

            // Stage 2: Normalize all resources BEFORE schema validation
            let mut skip_indices = HashSet::new();

            for (idx, resource) in resources.iter_mut().enumerate() {
                let label = resource_label(resource);

                // Get descriptor for normalization
                let descriptor = match self.registry.get_descriptor(&resource.kind) {
                    Some(desc) => desc.clone(),
                    None => {
                        let err = ValidationError::UnknownResourceType { kind: resource.kind.clone() };
                        wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V002, &resource.kind, resource.ref_name(), "kind", &err.to_string(), "Register the resource type in the registry");
                        errors.push(AnnotatedValidationError { resource: label, error: err });
                        skip_indices.insert(idx);
                        continue;
                    }
                };

                // Resolve the deployment-effective descriptor so normalization /
                // id-dereferencing see overlay-applied field names. Variant B parallel
                // schemas (different field names per deployment) need this — a SaaS
                // descriptor's `id_field` won't match the Software schema's `job_id`.
                let descriptor = match effective_descriptor(&descriptor, self.client_factory.as_ref()) {
                    Ok(d) => d,
                    Err(e) => {
                        let err = ValidationError::Other(format!("could not resolve deployment overlay for kind '{}': {}", resource.kind, e));
                        wxctl_core::log_error_resource!(operation_id, "validation", wxctl_core::logging::error_codes::R006, &resource.kind, resource.ref_name(), &err.to_string(), "Check the schema's deployments overlay declaration");
                        errors.push(AnnotatedValidationError { resource: label, error: err });
                        skip_indices.insert(idx);
                        continue;
                    }
                };
                let schema = &descriptor.schema.resource.schema;

                // CRITICAL: Normalize raw data BEFORE schema validation (Runtime Stage 2)
                // Stage 2a: Normalize field aliases (e.g., orchestrate_connection → connection_id)
                if let Err(e) = normalize_raw_resource_fields(&mut resource.data, schema, &resource.kind) {
                    let err = ValidationError::InvalidFieldValue { field: "field_normalization".to_string(), message: format!("{e}") };
                    wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V004, &resource.kind, resource.ref_name(), "normalization", &err.to_string(), "Check for field conflicts or invalid alias usage");
                    errors.push(AnnotatedValidationError { resource: label, error: err });
                    skip_indices.insert(idx);
                    continue;
                }

                // Stage 2b: Dereference generic 'id' field to schema-specific id_source
                if let Err(e) = dereference_id_field(&mut resource.data, &descriptor.schema, &resource.kind) {
                    let err = ValidationError::InvalidFieldValue { field: "id_dereferencing".to_string(), message: format!("{e}") };
                    wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V008, &resource.kind, resource.ref_name(), "id", &err.to_string(), "Check that the 'id' field value is valid");
                    errors.push(AnnotatedValidationError { resource: label, error: err });
                    skip_indices.insert(idx);
                    continue;
                }
            }

            // Stage 3: Build list of available resources for dependency validation
            let available_resources: Vec<(ResourceKey, String)> = resources
                .iter()
                .enumerate()
                .filter(|(idx, _)| !skip_indices.contains(idx))
                .map(|(_, r)| {
                    let ref_name = r.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");
                    (ResourceKey::new(&r.kind, ref_name), r.kind.clone())
                })
                .collect();

            // O(1) lookup set for depends_on target-existence checks (mirrors the
            // available_keys set built inside extract_dependencies for field refs).
            let available_keys: HashSet<ResourceKey> = available_resources.iter().map(|(k, _)| k.clone()).collect();

            // Stage 4: Validate each resource against its schema and extract dependencies
            for (idx, resource) in resources.iter_mut().enumerate() {
                // Skip resources that had normalization errors
                if skip_indices.contains(&idx) {
                    continue;
                }

                let label = resource_label(resource);

                // Get descriptor for this resource type
                let descriptor = match self.registry.get_descriptor(&resource.kind) {
                    Some(desc) => desc.clone(),
                    None => {
                        let err = ValidationError::UnknownResourceType { kind: resource.kind.clone() };
                        wxctl_core::log_error_resource!(operation_id, "validation", wxctl_core::logging::error_codes::V002, &resource.kind, resource.ref_name(), &err.to_string(), "Register the resource type in the registry");
                        errors.push(AnnotatedValidationError { resource: label, error: err });
                        continue;
                    }
                };

                // Phase 3 — validate metadata.requires.deployment against the resource's
                // per-service active deployment. Errors with R006 if unsatisfied.
                // Skipped when no client_factory is present (e.g., `wxctl validate` without a profile).
                if let Some(ref cf) = self.client_factory {
                    match resource.required_deployment() {
                        Ok(Some(required)) => {
                            let service = &descriptor.schema.resource.service;
                            let active = match cf.deployment_for_service(service) {
                                Ok(d) => d,
                                Err(e) => {
                                    let err = ValidationError::Other(format!("could not resolve deployment for service '{}': {}", service, e));
                                    wxctl_core::log_error_resource!(operation_id, "validation", wxctl_core::logging::error_codes::R006, &resource.kind, resource.ref_name(), &err.to_string(), "Check the profile's `deployment` field");
                                    errors.push(AnnotatedValidationError { resource: label.clone(), error: err });
                                    skip_indices.insert(idx);
                                    continue;
                                }
                            };
                            if !required.matches(&active) {
                                let err = ValidationError::Other(format!("[{}] resource '{}' requires deployment matching '{}', active deployment is '{}'", wxctl_core::logging::error_codes::R006, resource.ref_name(), required, active,));
                                wxctl_core::log_error_resource!(operation_id, "validation", wxctl_core::logging::error_codes::R006, &resource.kind, resource.ref_name(), &err.to_string(), "Adjust metadata.requires.deployment or switch profiles");
                                errors.push(AnnotatedValidationError { resource: label.clone(), error: err });
                                skip_indices.insert(idx);
                                continue;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            let err = ValidationError::Other(format!("[{}] resource '{}' has malformed metadata.requires.deployment: {}", wxctl_core::logging::error_codes::R006, resource.ref_name(), e));
                            wxctl_core::log_error_resource!(operation_id, "validation", wxctl_core::logging::error_codes::R006, &resource.kind, resource.ref_name(), &err.to_string(), "Use 'saas', 'software', or 'software-X.Y[.Z]' constraints");
                            errors.push(AnnotatedValidationError { resource: label.clone(), error: err });
                            skip_indices.insert(idx);
                            continue;
                        }
                    }
                }

                // Resolve the deployment-effective descriptor again for defaults +
                // schema validation + dependency extraction. Variant B parallel
                // schemas declare different fields than the SaaS base, so validating
                // against the base would reject perfectly valid Software configs (and
                // vice versa). Reconciliation re-resolves the overlay from the base
                // descriptor stored on `ValidatedResource`, so we deliberately do NOT
                // overwrite `descriptor` on the validated record below.
                let effective = match effective_descriptor(&descriptor, self.client_factory.as_ref()) {
                    Ok(d) => d,
                    Err(e) => {
                        let err = ValidationError::Other(format!("could not resolve deployment overlay for kind '{}': {}", resource.kind, e));
                        wxctl_core::log_error_resource!(operation_id, "validation", wxctl_core::logging::error_codes::R006, &resource.kind, resource.ref_name(), &err.to_string(), "Check the schema's deployments overlay declaration");
                        errors.push(AnnotatedValidationError { resource: label, error: err });
                        continue;
                    }
                };

                // Apply default values before validation, against the effective schema.
                apply_defaults(resource, &effective.schema);

                // Validate schema against the deployment-effective schema.
                if let Err(e) = validate_schema(resource, &effective.schema) {
                    // Attribute the event to the offending field when the error names one;
                    // "schema" is only the fallback for field-less variants (e.g. cycles).
                    let field = if e.field().is_empty() { "schema" } else { e.field() };
                    wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V003, &resource.kind, resource.ref_name(), field, &e.to_string(), "Fix the resource schema to match the expected format");
                    errors.push(AnnotatedValidationError { resource: label, error: e });
                    continue;
                }

                // Call post_validate hook to allow handlers to enrich resource data
                // (e.g., compute source hashes for tools)
                // Skipped when --skip-post-validate is set (e.g., pre-scaffold validation
                // where source_path directories don't exist yet)
                if !skip_post_validate && let Some(handler) = self.registry.get_handler(&resource.kind) {
                    let data_before = resource.data.clone();
                    let post_validate_span = info_span!(target: "wxctl::substage::hook", "post_validate", operation_id = %operation_id, hook = "post_validate", handler_kind = %resource.kind, resource_kind = %resource.kind, resource_name = %resource.ref_name());
                    let post_validate_result = handler.post_validate(&mut resource.data, operation_id).instrument(post_validate_span).await;
                    if let Err(e) = post_validate_result {
                        let err = ValidationError::InvalidFieldValue { field: "post_validate".to_string(), message: e.to_string() };
                        wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V007, &resource.kind, resource.ref_name(), "post_validate", &err.to_string(), "Check that resource data is valid for enrichment");
                        errors.push(AnnotatedValidationError { resource: label, error: err });
                        continue;
                    }
                    let sensitive = effective.schema.resource.schema.sensitive_paths();
                    tracing::debug!(target: "wxctl::substage::hook", operation_id = %operation_id, hook = "post_validate", handler_kind = %resource.kind, before = %serde_json::to_string(&redact_for_log(&data_before, &sensitive)).unwrap_or_default(), after = %serde_json::to_string(&redact_for_log(&resource.data, &sensitive)).unwrap_or_default(), "hook payload diff");
                }

                // Generic identity-hash step: for kinds declaring
                // reconciliation.identity_hash, stamp a deterministic hash over the
                // declared input fields (+ optional nonce) as the synthetic
                // `identity_hash` field. For storage: name_suffix, rewrite the name
                // field to `<base>-<hash>` so discovery matches per-generation and the
                // create body carries the suffixed name; for storage: tag, add a
                // `run-hash:` tag; for storage: env_marker, inject a WXCTL_IDENTITY
                // entry into env_variables (job_run — the server clobbers names).
                // `identity_hash` is not a declared schema field, so
                // the request materializer never sends it to any API body; the nonce
                // field is `location: LocalOnly`, likewise omitted. Read from the
                // deployment-effective schema so a deployment overlay could add the block.
                if let Some(ih) = &effective.schema.resource.reconciliation.identity_hash {
                    // EnvMarker: hash a marker-stripped copy so the hash stays a function
                    // of the user-declared inputs only — the injected WXCTL_IDENTITY entry
                    // must never feed back into the hash it carries (a re-stamped resource
                    // would otherwise drift its own identity).
                    let hash = if matches!(ih.storage, wxctl_schema::schema::HashStorage::EnvMarker) {
                        let mut clean = resource.data.clone();
                        wxctl_providers::strip_identity_env_marker(&mut clean);
                        wxctl_providers::identity_hash(&clean, &ih.fields, ih.nonce_field.as_deref(), ih.length)
                    } else {
                        wxctl_providers::identity_hash(&resource.data, &ih.fields, ih.nonce_field.as_deref(), ih.length)
                    };
                    match ih.storage {
                        wxctl_schema::schema::HashStorage::NameSuffix => {
                            let name_field = effective.schema.resource.reconciliation.discovery.name_field.clone().unwrap_or_else(|| "name".to_string());
                            if let Some(base) = resource.data.get(&name_field).and_then(|v| v.as_str()).map(String::from) {
                                resource.data[name_field.as_str()] = serde_json::Value::String(format!("{base}-{hash}"));
                            }
                        }
                        wxctl_schema::schema::HashStorage::Tag => wxctl_providers::set_run_hash_tag(&mut resource.data, &hash),
                        // EnvMarker: plant WXCTL_IDENTITY=<hash> in env_variables — the one
                        // job_run field the server round-trips verbatim (it clobbers the
                        // submitted name to "Notebook Job" on both CPDaaS and CP4D).
                        wxctl_schema::schema::HashStorage::EnvMarker => wxctl_providers::set_identity_env_marker(&mut resource.data, &hash),
                        wxctl_schema::schema::HashStorage::ServerSide => {}
                        // Local: no remote carrier — the hash is stamped below; the
                        // Skip-discovery arm / handlers handle persistence.
                        wxctl_schema::schema::HashStorage::Local => {}
                    }
                    resource.data["identity_hash"] = serde_json::Value::String(hash);
                }

                // Extract resource ref_name for key
                let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();
                let key = ResourceKey::new(&resource.kind, &ref_name);

                // Extract and validate dependencies from ${...} references
                let dep_result = extract_dependencies(&key, &resource.data, &descriptor.schema, &available_resources);

                // Log and collect any dependency validation errors
                let mut has_dep_errors = false;
                for err in dep_result.errors {
                    // Rename the destructured target name: reusing `ref_name` here would
                    // shadow the resource's own `ref_name` above and attribute the error
                    // to the referenced TARGET instead of the failing resource. Both
                    // dependency errors surface as WXCTL-V005.
                    match &err {
                        ValidationError::InvalidDependency { field_path, ref_kind, ref_name: target_name, allowed_kinds } => {
                            wxctl_core::log_error_field!(
                                operation_id,
                                "validation",
                                wxctl_core::logging::error_codes::V005,
                                &resource.kind,
                                &ref_name,
                                field_path,
                                &format!("references '{}:{}', but schema only allows: [{}]", ref_kind, target_name, allowed_kinds.join(", ")),
                                "Check that all ${...} references are allowed by the schema"
                            );
                        }
                        ValidationError::UnresolvedReference { field_path, ref_kind, ref_name: target_name, .. } => {
                            wxctl_core::log_error_field!(
                                operation_id,
                                "validation",
                                wxctl_core::logging::error_codes::V005,
                                &resource.kind,
                                &ref_name,
                                field_path,
                                &format!("references '{}:{}', but no such resource is defined in this config", ref_kind, target_name),
                                &err.suggestion()
                            );
                        }
                        _ => {}
                    }
                    errors.push(AnnotatedValidationError { resource: label.clone(), error: err });
                    has_dep_errors = true;
                }
                if has_dep_errors {
                    continue;
                }

                // Collect edges with field paths for visualization/error messages
                all_edges.extend(dep_result.edges);
                let mut dependencies = dep_result.dependencies;

                // Parse + strip the `depends_on` meta-field (ordering-only edges,
                // no value resolved). Stripping happens before the data.clone()
                // below, so `depends_on` never reaches `ValidatedResource.data`
                // or any API request body.
                let declared = match resource.take_depends_on() {
                    Ok(keys) => keys,
                    Err(e) => {
                        wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V005, &resource.kind, &ref_name, "depends_on", &e.to_string(), "depends_on entries must be bare 'kind.ref_name' strings");
                        // Surface the helper's specific shape message, not the generic InvalidDependency Display.
                        errors.push(AnnotatedValidationError { resource: label.clone(), error: ValidationError::InvalidFieldValue { field: "depends_on".to_string(), message: format!("[{}] {}", wxctl_core::logging::error_codes::V005, e) } });
                        continue;
                    }
                };

                // Validate each declared prerequisite against the same
                // available-resources set field references use: dangling target
                // and self-dependency are hard errors (V005), caught before apply.
                let mut depends_on_ok = true;
                for dep in declared {
                    if dep == key {
                        let msg = format!("[{}] resource '{}:{}' lists itself in depends_on", wxctl_core::logging::error_codes::V005, resource.kind, ref_name);
                        wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V005, &resource.kind, &ref_name, "depends_on", &msg, "Remove the self-reference from depends_on");
                        errors.push(AnnotatedValidationError { resource: label.clone(), error: ValidationError::InvalidFieldValue { field: "depends_on".to_string(), message: msg } });
                        depends_on_ok = false;
                        continue;
                    }
                    if !available_keys.contains(&dep) {
                        let msg = format!("[{}] depends_on target '{}.{}' is not present in the config", wxctl_core::logging::error_codes::V005, dep.kind, dep.name);
                        wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V005, &resource.kind, &ref_name, "depends_on", &msg, "Add the target resource or fix the depends_on entry");
                        errors.push(AnnotatedValidationError { resource: label.clone(), error: ValidationError::InvalidFieldValue { field: "depends_on".to_string(), message: msg } });
                        depends_on_ok = false;
                        continue;
                    }
                    if !dependencies.contains(&dep) {
                        dependencies.push(dep);
                    }
                }
                if !depends_on_ok {
                    continue;
                }

                // Already validated to be "retain" | "delete" | absent in validate_schema.
                let on_destroy = match resource.data.get("on_destroy").and_then(|v| v.as_str()) {
                    Some("retain") => OnDestroyPolicy::Retain,
                    _ => OnDestroyPolicy::Delete,
                };

                validated_resources.push(ValidatedResource { key, data: resource.data.clone(), descriptor, dependencies, on_destroy });
            }

            // If any validation errors, return early
            if !errors.is_empty() {
                tracing::Span::current().record("status", "failed");
                tracing::debug!(target: "wxctl::substage::validation", status = "failed", "validation stage failed");
                return Ok(ValidationResult::failure(errors));
            }

            // Stage 4b: Cross-resource validators (e.g. WXCTL-V503 —
            // storage_class enum depends on linked storage_connection.type).
            // Run only when per-resource validation has passed so linked
            // resources exist and carry checked data.
            let cross_errors = cross_resource::run_all(&validated_resources);
            if !cross_errors.is_empty() {
                for ann in &cross_errors {
                    let message = ann.error.to_string();
                    wxctl_core::log_error_field!(operation_id, "validation", wxctl_core::logging::error_codes::V503, &ann.resource, &ann.resource, ann.error.field(), &message, "Align the field value with the linked resource's discriminator");
                }
                errors.extend(cross_errors);
                tracing::Span::current().record("status", "failed");
                tracing::debug!(target: "wxctl::substage::validation", status = "failed", "validation stage failed");
                return Ok(ValidationResult::failure(errors));
            }

            // Stage 5: Build ResourceSet with cycle detection
            // ResourceSetBuilder handles graph construction and cycle detection.
            // Wave computation is lazy - resources don't need to be pre-sorted.
            let resource_set = match wxctl_core::ResourceSetBuilder::new(validated_resources).with_edges(all_edges).use_preextracted_deps().build() {
                Ok(set) => set,
                Err(cycle_error) => {
                    let cycle_str = cycle_error.cycle.iter().map(|k| format!("{}:{}", k.kind, k.name)).collect::<Vec<_>>().join(" -> ");
                    let err = ValidationError::CircularDependency { path: cycle_error.cycle };
                    wxctl_core::log_error!(operation_id, "validation", wxctl_core::logging::error_codes::V006, &err.to_string(), &format!("Break the circular dependency chain: {}", cycle_str));
                    errors.push(AnnotatedValidationError { resource: String::new(), error: err });
                    tracing::Span::current().record("status", "failed");
                    tracing::debug!(target: "wxctl::substage::validation", status = "failed", "validation stage failed");
                    return Ok(ValidationResult::failure(errors));
                }
            };

            tracing::Span::current().record("status", "completed");
            Ok(ValidationResult::success(resource_set))
        }
        .instrument(span)
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::ResourceSchema;
    use wxctl_schema::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldReferences, FieldType, HookDefinition, HttpMethod, ReconciliationDefinition, ResourceDefinition, SchemaDefinition, UpdateStrategy};

    fn make_field(name: &str, field_type: FieldType, references: Option<FieldReferences>) -> FieldDefinition {
        FieldDefinition {
            name: name.into(),
            field_type,
            required: false,
            immutable: false,
            location: wxctl_schema::schema::FieldLocation::default(),
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references,
            api_field: None,
            sensitive: false,
            also_query: false,
            is_path: false,
            synthesize: None,
            synth_shape: None,
            properties: None,
        }
    }

    fn make_schema_def(fields: Vec<FieldDefinition>) -> SchemaDefinition {
        SchemaDefinition { fields, ..Default::default() }
    }

    fn make_resource_schema(id_source: &str, fields: Vec<FieldDefinition>) -> ResourceSchema {
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
                    readiness: None,
                },
                schema: SchemaDefinition { fields, ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::GetById, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, list_map: false, id_source: id_source.into() },
                    identity_hash: None,
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

    // ── normalize_raw_resource_fields ──

    #[test]
    fn normalize_raw_resource_fields_branches() {
        // field "connection_id" has references.resource = "orchestrate_connection"
        // → build_field_mapping produces {"orchestrate_connection" => "connection_id"}.
        let fields = || vec![make_field("connection_id", FieldType::String, Some(FieldReferences { resource: "orchestrate_connection".into(), field: "id".into(), also_allows: vec![], optional: false, require_ready: false, relationship: None }))];

        // Alias key renamed onto the api field; the alias key is removed.
        let schema = make_schema_def(fields());
        let mut data = json!({"orchestrate_connection": "${orchestrate_connection.my-conn}"});
        normalize_raw_resource_fields(&mut data, &schema, "test").unwrap();
        assert_eq!(data.get("connection_id"), Some(&json!("${orchestrate_connection.my-conn}")));
        assert!(data.get("orchestrate_connection").is_none());

        // api field already present (no alias) → left untouched.
        let schema = make_schema_def(fields());
        let mut data = json!({"connection_id": "existing-value"});
        normalize_raw_resource_fields(&mut data, &schema, "test").unwrap();
        assert_eq!(data.get("connection_id"), Some(&json!("existing-value")));

        // Both alias and api field set → "Field conflict" error.
        let schema = make_schema_def(fields());
        let mut data = json!({"orchestrate_connection": "val1", "connection_id": "val2"});
        let err = normalize_raw_resource_fields(&mut data, &schema, "test").unwrap_err();
        assert!(err.to_string().contains("Field conflict"));
    }

    // ── dereference_id_field ──

    #[test]
    fn dereference_id_field_branches() {
        // id_source != "id" → `id` renamed to id_source; `_from_id` marker set.
        let schema = make_resource_schema("app_id", vec![]);
        let mut data = json!({"id": "my-app-123"});
        dereference_id_field(&mut data, &schema, "test").unwrap();
        assert_eq!(data.get("app_id"), Some(&json!("my-app-123")));
        assert!(data.get("id").is_none());
        assert_eq!(data.get("_from_id"), Some(&json!(true)));

        // id_source == "id" → `id` kept as-is; `_from_id` marker still set.
        let schema = make_resource_schema("id", vec![]);
        let mut data = json!({"id": "model-456"});
        dereference_id_field(&mut data, &schema, "test").unwrap();
        assert_eq!(data.get("id"), Some(&json!("model-456")));
        assert_eq!(data.get("_from_id"), Some(&json!(true)));

        // Both `id` and id_source present → "Field conflict" error.
        let schema = make_resource_schema("app_id", vec![]);
        let mut data = json!({"id": "my-app", "app_id": "other-app"});
        assert!(dereference_id_field(&mut data, &schema, "test").unwrap_err().to_string().contains("Field conflict"));

        // Non-string `id` → "must be a string" error.
        let schema = make_resource_schema("app_id", vec![]);
        let mut data = json!({"id": 12345});
        assert!(dereference_id_field(&mut data, &schema, "test").unwrap_err().to_string().contains("must be a string"));
    }

    // ── effective_descriptor ──
    //
    // The deployment-resolution path that requires a live ClientFactory is
    // exercised end-to-end by the live integration tests. This unit test
    // covers the early-return branches that don't need network/profile setup.

    #[test]
    fn effective_descriptor_early_return_branches() {
        // Schema with no `deployments:` block — the common case. Returns the base
        // descriptor unchanged regardless of whether a client_factory is provided.
        let schema = make_resource_schema("id", vec![]);
        let base = Arc::new(ResourceDescriptor::from_schema(&schema).unwrap());
        let result = effective_descriptor(&base, None).unwrap();
        assert!(Arc::ptr_eq(&base, &result), "expected base Arc to be returned by clone, not a rebuilt descriptor");

        // Schema with a deployments overlay but no client_factory available (e.g.
        // `wxctl validate` without a profile). Helper falls back to the base — the
        // R006 path elsewhere will surface the missing profile if validation needs it.
        let mut schema = make_resource_schema("id", vec![]);
        let mut map = std::collections::HashMap::new();
        map.insert("software-5.3".to_string(), wxctl_schema::schema::definition::DeploymentOverlay::default());
        schema.resource.deployments = Some(map);
        let base = Arc::new(ResourceDescriptor::from_schema(&schema).unwrap());
        let result = effective_descriptor(&base, None).unwrap();
        assert!(Arc::ptr_eq(&base, &result), "no client_factory → must return base unchanged");
    }
}
