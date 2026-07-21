//! Build-time schema-set validation: field-type validity, reference
//! resolution, and cross-service linkage checks. Ported from
//! `wxctl-schema/build.rs:369-489` (plus `first_sentence` at `:501-531` and
//! `VALID_FIELD_TYPES` at `:33`), rewritten to walk the full model types
//! (`crate::definition::ResourceDefinition`/`FieldDefinition`) instead of the
//! deleted reduced `build.rs` structs. All checks panic with the schema
//! identity on failure — same contract as today's `build.rs` guards.

use crate::definition::{FieldDefinition, FieldType, UpdateStrategy, VariantDefinition};
use std::collections::{HashMap, HashSet};

/// Valid field types in schema YAML definitions.
const VALID_FIELD_TYPES: &[&str] = &["string", "integer", "float", "boolean", "object", "array", "timestamp"];

fn field_type_str(field_type: &FieldType) -> &'static str {
    match field_type {
        FieldType::String => "string",
        FieldType::Integer => "integer",
        FieldType::Float => "float",
        FieldType::Boolean => "boolean",
        FieldType::Object => "object",
        FieldType::Array => "array",
        FieldType::Timestamp => "timestamp",
    }
}

/// Variant groups in sorted-key order — `variants` is a HashMap, so iterating
/// `values()` directly would make validation order (and thus which schema's
/// panic fires first among several bad ones) nondeterministic.
fn sorted_variant_values(variants: &HashMap<String, VariantDefinition>) -> Vec<&VariantDefinition> {
    let mut keys: Vec<&String> = variants.keys().collect();
    keys.sort_unstable();
    keys.into_iter().map(|k| &variants[k]).collect()
}

fn validate_references_recursive(schema_name: &str, fields: &[FieldDefinition], prefix: &str, all_names: &HashSet<&str>) {
    for field in fields {
        let field_path = if prefix.is_empty() { field.name.clone() } else { format!("{}.{}", prefix, field.name) };

        if let Some(ref refs) = field.references {
            if let Some(rel) = refs.relationship.as_deref()
                && rel != "containment"
            {
                panic!("Schema '{}', field '{}': references.relationship '{}' is not a known value (only 'containment' is supported)", schema_name, field_path, rel);
            }
            if !all_names.contains(refs.resource.as_str()) {
                panic!("Schema '{}', field '{}': references unknown resource '{}'", schema_name, field_path, refs.resource);
            }
            for also in &refs.also_allows {
                if !all_names.contains(also.as_str()) {
                    panic!("Schema '{}', field '{}': references.also_allows lists unknown resource '{}'", schema_name, field_path, also);
                }
            }
        }

        if let Some(nested) = field.schema.as_deref() {
            validate_references_recursive(schema_name, &nested.fields, &field_path, all_names);
        }
    }
}

/// Validate the full schema set: non-empty `service`/`kind`, field-type
/// validity, and reference resolution (including nested and variant-scoped
/// fields) against the set of known resource names. Panics with schema/field
/// identity on the first violation found.
pub fn validate_schemas(schemas: &[crate::build_meta::ParsedSchema]) {
    let all_names: HashSet<&str> = schemas.iter().map(|s| s.schema.resource.name.as_str()).collect();

    for parsed in schemas {
        let resource = &parsed.schema.resource;

        if resource.service.is_empty() {
            panic!("Schema '{}': 'service' field must not be empty", resource.name);
        }
        if resource.kind.is_empty() {
            panic!("Schema '{}': 'kind' field must not be empty", resource.name);
        }

        // A schema that PATCHes with JSON-Patch must declare
        // `reconciliation.json_patch_path_prefix`. Re-homed from the deleted
        // runtime guard `wxctl-schema/src/validation/schema.rs`
        // `validate_reconciliation_patch_prefix` (git show 0df4c125) — the
        // engine's `update.rs` errors `json_patch_path_prefix required` at
        // apply time when it is `None`; this turns that runtime failure into a
        // build error. `""` (RFC-6902 entity-relative) is a valid prefix and passes.
        let recon = &resource.reconciliation;
        if matches!(recon.update_strategy, UpdateStrategy::Patch) && recon.use_json_patch && recon.json_patch_path_prefix.is_none() {
            panic!(
                "Schema '{}', field 'reconciliation.json_patch_path_prefix': schema '{}' uses update_strategy: patch with use_json_patch: true but has no reconciliation.json_patch_path_prefix — set `reconciliation.json_patch_path_prefix` (use \"\" for RFC-6902 entity-relative paths)",
                resource.name, resource.kind
            );
        }

        // Validate common and variant field types.
        let mut all_field_refs: Vec<&FieldDefinition> = resource.schema.fields.iter().collect();
        if let Some(variants) = &resource.schema.variants {
            for variant in sorted_variant_values(variants) {
                for f in &variant.fields {
                    all_field_refs.push(f);
                }
            }
        }
        for field in &all_field_refs {
            let ty = field_type_str(&field.field_type);
            if !VALID_FIELD_TYPES.contains(&ty) {
                panic!("Schema '{}', field '{}': invalid type '{}'. Must be one of: {}", resource.name, field.name, ty, VALID_FIELD_TYPES.join(", "));
            }
        }

        // Validate references (including nested and variant-scoped) resolve to known schemas.
        validate_references_recursive(&resource.name, &resource.schema.fields, "", &all_names);
        if let Some(variants) = &resource.schema.variants {
            for variant in sorted_variant_values(variants) {
                validate_references_recursive(&resource.name, &variant.fields, "", &all_names);
            }
        }
    }
}

/// Validate cross-service linkage bridges: source/target resolve, constraint
/// fields exist on their resource schemas, and `field_mapping` path roots are
/// declared top-level fields on their respective resources.
pub fn validate_linkages(linkages: &crate::build_meta::LinkagesFile, all_names: &HashSet<String>, schemas: &[crate::build_meta::ParsedSchema]) {
    // Build a lookup: resource_name → set of top-level field names
    let field_lookup: HashMap<&str, HashSet<&str>> = schemas
        .iter()
        .map(|s| {
            let fields: HashSet<&str> = s.schema.resource.schema.fields.iter().map(|f| f.name.as_str()).collect();
            (s.schema.resource.name.as_str(), fields)
        })
        .collect();

    for bridge in &linkages.bridges {
        if !all_names.contains(&bridge.source) {
            panic!("Linkage '{}': source '{}' is not a known resource", bridge.name, bridge.source);
        }
        if !all_names.contains(&bridge.target) {
            panic!("Linkage '{}': target '{}' is not a known resource", bridge.name, bridge.target);
        }

        // Validate constraint fields exist on their resource schemas
        for (resource_name, fields) in &bridge.constraints {
            if !all_names.contains(resource_name) {
                panic!("Linkage '{}': constraint references unknown resource '{}'", bridge.name, resource_name);
            }
            if let Some(known_fields) = field_lookup.get(resource_name.as_str()) {
                for field_name in fields.keys() {
                    if !known_fields.contains(field_name.as_str()) {
                        panic!("Linkage '{}': constraint field '{}' does not exist on resource '{}'", bridge.name, field_name, resource_name);
                    }
                }
            }
        }

        // Validate field_mapping path roots. Only the first path segment is checked:
        // it must be a declared top-level field on the respective resource. Deeper
        // segments are intentionally NOT validated — connection `credentials` /
        // `properties` are free-form objects whose keys vary by datasource_type, so
        // descending would false-positive on legitimate open-object access. This still
        // catches a typo'd or renamed root (e.g. `properties` → `config`).
        let path_root = |p: &str| p.split('.').next().unwrap_or(p).to_string();
        if let Some(src_fields) = field_lookup.get(bridge.source.as_str()) {
            for fm in &bridge.field_mapping {
                let root = path_root(&fm.source);
                if !src_fields.contains(root.as_str()) {
                    panic!("Linkage '{}': field_mapping source '{}' — root field '{}' is not declared on source resource '{}'", bridge.name, fm.source, root, bridge.source);
                }
            }
        }
        if let Some(tgt_fields) = field_lookup.get(bridge.target.as_str()) {
            for fm in &bridge.field_mapping {
                let root = path_root(&fm.target);
                if !tgt_fields.contains(root.as_str()) {
                    panic!("Linkage '{}': field_mapping target '{}' — root field '{}' is not declared on target resource '{}'", bridge.name, fm.target, root, bridge.target);
                }
            }
        }
    }
}

/// Extract the first sentence from a description string.
///
/// Splits at the first `". "` that is a real sentence boundary — i.e. the token
/// ending at that period is not a known abbreviation. Without this guard,
/// descriptions like `… model (incl. AutoAI) …` or `… search (e.g. Milvus). …`
/// truncate mid-sentence at the abbreviation's period. `etc.` is deliberately
/// NOT listed: a trailing `etc.` is a legitimate sentence end, and mid-sentence
/// it appears as `etc.)` (no following space), so it never mis-splits.
pub fn first_sentence(desc: &str) -> String {
    // Lowercased alphabetic tokens whose trailing '.' is never a sentence end.
    // Interior dots are stripped before the compare, so "e.g." → "eg", "i.e." → "ie".
    const ABBREVIATIONS: &[&str] = &["eg", "ie", "incl", "vs", "cf", "approx", "resp", "al", "esp", "viz"];

    let trimmed = desc.trim().replace('\n', " ");
    let trimmed = trimmed.trim();

    let mut from = 0;
    while let Some(rel) = trimmed[from..].find(". ") {
        let idx = from + rel; // byte index of the candidate boundary '.'
        // The alphabetic token ending at this period, with surrounding
        // punctuation ('(', interior dots) stripped for the abbreviation compare.
        let word_start = trimmed[..idx].rfind(char::is_whitespace).map_or(0, |p| p + 1);
        let word: String = trimmed[word_start..idx].chars().filter(char::is_ascii_alphabetic).collect::<String>().to_ascii_lowercase();
        if !ABBREVIATIONS.contains(&word.as_str()) {
            return trimmed[..=idx].to_string();
        }
        from = idx + 2; // resume past this ". "
    }

    if trimmed.ends_with('.') { trimmed.to_string() } else { format!("{}.", trimmed) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal single-kind schema. `patch_block` is spliced into the
    /// `reconciliation:` block by each test.
    fn schema_yaml(patch_block: &str) -> String {
        format!(
            r#"
resource:
  name: guard_probe
  service: guard
  kind: guard_probe
  version: v1
  api:
    base_path: /Probes
    id_field: name
    get_endpoint: /Probes('{{name}}')
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: name
        type: string
        required: true
  reconciliation:
    discovery:
      method: get_by_id
      id_source: name
    update_strategy: patch
{patch_block}
"#
        )
    }

    fn parse(patch_block: &str) -> crate::build_meta::ParsedSchema {
        crate::build_meta::parse_schema_file(&schema_yaml(patch_block), "guard_probe.yaml".to_string()).expect("fixture parses")
    }

    #[test]
    #[should_panic(expected = "reconciliation.json_patch_path_prefix")]
    fn json_patch_without_prefix_fails_the_build() {
        validate_schemas(&[parse("    use_json_patch: true")]);
    }

    #[test]
    fn empty_prefix_is_a_valid_prefix() {
        validate_schemas(&[parse("    use_json_patch: true\n    json_patch_path_prefix: \"\"")]);
    }

    #[test]
    fn plain_patch_without_json_patch_passes() {
        validate_schemas(&[parse("    use_json_patch: false")]);
    }
}
