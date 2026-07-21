//! Pure, sans-IO config validation — the offline subset of the engine's
//! `ValidationPipeline`. Resolves schemas from the static IR (`crate::ir::RESOURCE_IR`),
//! runs schema/dependency/cross-resource checks with `client_factory = None` and
//! `skip_post_validate = true`, and returns a structured `{ valid, errors }` report.
//! No deployment-overlay resolution, no `post_validate`, no
//! `ClientFactory`/`ResourceRegistry`, no timing — this is what the remote MCP
//! `validate_config` tool calls.

use super::dependency::extract_dependencies;
use super::schema::{apply_defaults, check_duplicate_names, validate_schema};
use super::types::{AnnotatedValidationError, ValidationError};
use super::{cross_resource, dereference_id_field, normalize_raw_resource_fields};
use crate::descriptor::ResourceDescriptor;
use crate::resource::{OnDestroyPolicy, RawResource, ValidatedResource};
use serde::Serialize;
use std::collections::HashSet;
use std::sync::Arc;
use wxctl_graph::ResourceKey;

/// Structured result of an offline `validate_config` run.
#[derive(Serialize)]
pub struct ValidationReport {
    pub valid: bool,
    pub errors: Vec<AnnotatedValidationError>,
}

/// Build a resource identity string like "tool/my_tool" for error messages.
fn resource_label(resource: &RawResource) -> String {
    let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    format!("{}/{}", resource.kind, ref_name)
}

/// Validate an inline config (a YAML document or `---`-separated stream of
/// resource documents) against the compiled schema set, offline. Malformed
/// YAML returns `Err` (the wasm binding maps it to an `isError` tool result);
/// schema/ref problems return `Ok(ValidationReport { valid: false, errors })`
/// (a successful validation that found problems).
pub fn validate_config(yaml: &str) -> anyhow::Result<ValidationReport> {
    let mut resources: Vec<RawResource> = parse_resources(yaml)?;
    let mut errors: Vec<AnnotatedValidationError> = Vec::new();

    // Stage 1: duplicate names.
    if let Err(e) = check_duplicate_names(&resources) {
        errors.push(AnnotatedValidationError { resource: String::new(), error: e });
        return Ok(ValidationReport { valid: false, errors });
    }

    // Stage 2: normalize aliases + dereference generic `id`, against the BASE
    // schema (offline path has no deployment overlay).
    let mut skip: HashSet<usize> = HashSet::new();
    for (idx, resource) in resources.iter_mut().enumerate() {
        let label = resource_label(resource);
        let Some(ir) = crate::ir::RESOURCE_IR.get(resource.kind.as_str()) else {
            errors.push(AnnotatedValidationError { resource: label, error: ValidationError::UnknownResourceType { kind: resource.kind.clone() } });
            skip.insert(idx);
            continue;
        };
        if let Err(e) = normalize_raw_resource_fields(&mut resource.data, &ir.resource.schema, &resource.kind) {
            errors.push(AnnotatedValidationError { resource: label, error: ValidationError::InvalidFieldValue { field: "field_normalization".to_string(), message: e.to_string() } });
            skip.insert(idx);
            continue;
        }
        if let Err(e) = dereference_id_field(&mut resource.data, ir, &resource.kind) {
            errors.push(AnnotatedValidationError { resource: label, error: ValidationError::InvalidFieldValue { field: "id_dereferencing".to_string(), message: e.to_string() } });
            skip.insert(idx);
        }
    }

    // Stage 3: available-resource set for dependency existence checks.
    let available: Vec<(ResourceKey, String)> = resources
        .iter()
        .enumerate()
        .filter(|(idx, _)| !skip.contains(idx))
        .map(|(_, r)| {
            let ref_name = r.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");
            (ResourceKey::new(&r.kind, ref_name), r.kind.clone())
        })
        .collect();

    // O(1) lookup set for depends_on target-existence checks (mirrors the
    // available_keys set the engine pipeline builds for the same check).
    let available_keys: HashSet<ResourceKey> = available.iter().map(|(k, _)| k.clone()).collect();

    // Stage 4: per-resource defaults + schema validation + dependency extraction.
    let mut validated: Vec<ValidatedResource> = Vec::new();
    for (idx, resource) in resources.iter_mut().enumerate() {
        if skip.contains(&idx) {
            continue;
        }
        let label = resource_label(resource);
        let ir = crate::ir::RESOURCE_IR.get(resource.kind.as_str()).expect("kind present (Stage 2 inserted skip otherwise)");
        let descriptor = Arc::new(ResourceDescriptor::from_ir(ir));

        apply_defaults(resource, ir);
        if let Err(e) = validate_schema(resource, ir) {
            errors.push(AnnotatedValidationError { resource: label, error: e });
            continue;
        }

        let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();
        let key = ResourceKey::new(&resource.kind, &ref_name);
        let dep_result = extract_dependencies(&key, &resource.data, ir, &available);
        if !dep_result.errors.is_empty() {
            for err in dep_result.errors {
                errors.push(AnnotatedValidationError { resource: label.clone(), error: err });
            }
            continue;
        }

        // Parse + strip the `depends_on` meta-field (ordering-only edges, no value
        // resolved). Shares the Phase 1 helper for byte-parity with the engine
        // pipeline; stripping happens before the `data.clone()` below so `depends_on`
        // never reaches `ValidatedResource.data`. The offline surface builds no graph,
        // so unlike the engine pipeline it performs no merge into `dependencies`.
        match resource.take_depends_on() {
            Ok(declared) => {
                let mut depends_on_ok = true;
                for dep in declared {
                    if dep == key {
                        let msg = format!("[WXCTL-V005] resource '{}:{}' lists itself in depends_on", resource.kind, ref_name);
                        errors.push(AnnotatedValidationError { resource: label.clone(), error: ValidationError::InvalidFieldValue { field: "depends_on".to_string(), message: msg } });
                        depends_on_ok = false;
                        continue;
                    }
                    if !available_keys.contains(&dep) {
                        let msg = format!("[WXCTL-V005] depends_on target '{}.{}' is not present in the config", dep.kind, dep.name);
                        errors.push(AnnotatedValidationError { resource: label.clone(), error: ValidationError::InvalidFieldValue { field: "depends_on".to_string(), message: msg } });
                        depends_on_ok = false;
                        continue;
                    }
                }
                if !depends_on_ok {
                    continue;
                }
            }
            Err(e) => {
                errors.push(AnnotatedValidationError { resource: label.clone(), error: ValidationError::InvalidFieldValue { field: "depends_on".to_string(), message: format!("[WXCTL-V005] {}", e) } });
                continue;
            }
        }

        let on_destroy = match resource.data.get("on_destroy").and_then(|v| v.as_str()) {
            Some("retain") => OnDestroyPolicy::Retain,
            _ => OnDestroyPolicy::Delete,
        };
        validated.push(ValidatedResource { key, data: resource.data.clone(), descriptor, dependencies: dep_result.dependencies, on_destroy });
    }

    if !errors.is_empty() {
        return Ok(ValidationReport { valid: false, errors });
    }

    // Stage 4b: cross-resource validators (e.g. WXCTL-V503).
    let cross = cross_resource::run_all(&validated);
    if !cross.is_empty() {
        errors.extend(cross);
        return Ok(ValidationReport { valid: false, errors });
    }

    Ok(ValidationReport { valid: true, errors })
}

/// Parse a YAML document or `---`-separated stream into `RawResource`s. A single
/// document may be a mapping (one resource), a sequence (many), or a top-level
/// `{ resources: [...] }` envelope.
fn parse_resources(yaml: &str) -> anyhow::Result<Vec<RawResource>> {
    use serde::Deserialize;
    use serde_norway::Value as Yaml;
    let mut out = Vec::new();
    for doc in serde_norway::Deserializer::from_str(yaml) {
        let value = Yaml::deserialize(doc)?;
        match value {
            Yaml::Null => continue,
            Yaml::Sequence(items) => {
                for item in items {
                    out.push(serde_norway::from_value(item)?);
                }
            }
            Yaml::Mapping(ref map) if map.contains_key(Yaml::from("resources")) => {
                let envelope: ResourceEnvelope = serde_norway::from_value(value)?;
                out.extend(envelope.resources);
            }
            other => out.push(serde_norway::from_value(other)?),
        }
    }
    Ok(out)
}

#[derive(serde::Deserialize)]
struct ResourceEnvelope {
    resources: Vec<RawResource>,
}

#[cfg(test)]
mod depends_on_offline_tests {
    use super::validate_config;

    // Two `space` resources; only `name` is required and `space` references no
    // other kind, so `depends_on` is the sole dependency source — keeps the
    // offline-vs-engine comparison clean.
    const VALID: &str = "kind: space\nref_name: a\nname: space-a\n---\nkind: space\nref_name: b\nname: space-b\ndepends_on:\n  - space.a\n";
    const DANGLING: &str = "kind: space\nref_name: b\nname: space-b\ndepends_on:\n  - space.ghost\n";
    const SELF: &str = "kind: space\nref_name: b\nname: space-b\ndepends_on:\n  - space.b\n";
    const MALFORMED: &str = "kind: space\nref_name: b\nname: space-b\ndepends_on:\n  - not_a_pair\n";

    fn messages(yaml: &str) -> (bool, Vec<String>) {
        let report = validate_config(yaml).expect("validate_config runs without credentials");
        (report.valid, report.errors.iter().map(|e| e.error.to_string()).collect())
    }

    // AC8 (offline) — a valid depends_on passes the offline validator and is never
    // flagged as an unknown field.
    #[test]
    fn ac8_valid_depends_on_passes_offline() {
        let (valid, msgs) = messages(VALID);
        assert!(valid, "AC8 offline: valid depends_on must pass; got {:?}", msgs);
        assert!(msgs.iter().all(|m| !m.to_lowercase().contains("unknown field")), "AC8 offline: depends_on must not be an unknown field");
    }

    // AC7 (offline) — every malformed/invalid depends_on shape is rejected without
    // credentials, with the error naming the offending entry. Each row covers a
    // distinct rejection branch (matching the engine's behavior):
    //   - dangling target (AC4)
    //   - self-dependency (AC5)
    //   - malformed entry (AC4)
    #[test]
    fn ac7_invalid_depends_on_rejected_offline() {
        let cases: &[(&str, &str, &str)] = &[(DANGLING, "space.ghost", "dangling target must be named"), (SELF, "lists itself", "self-dependency must be reported"), (MALFORMED, "not_a_pair", "malformed entry must be named")];
        for (yaml, needle, why) in cases {
            let (valid, msgs) = messages(yaml);
            assert!(!valid, "AC7: offline validator must reject — {why}");
            assert!(msgs.iter().any(|m| m.contains(needle)), "AC7: {why}; got {:?}", msgs);
        }
    }
}
