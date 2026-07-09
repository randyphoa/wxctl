use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::Path;
use wxctl_graph::IndexGraph;

// ============================================================================
// Schema Directory Configuration
// ============================================================================

/// Schema directories to scan.
/// First element: fs path for `read_dir` (relative to crate root).
/// Second element: `include_str!` path prefix (relative to `src/dependency_graph.rs`).
const SCHEMA_DIRS: &[(&str, &str)] = &[
    ("src/schemas/common_core", "schemas/common_core"),
    ("src/schemas/watsonx_data", "schemas/watsonx_data"),
    ("src/schemas/watsonx_orchestrate", "schemas/watsonx_orchestrate"),
    ("src/schemas/watsonx_ai", "schemas/watsonx_ai"),
    ("src/schemas/cloud_object_storage", "schemas/cloud_object_storage"),
    ("src/schemas/openscale", "schemas/openscale"),
    ("src/schemas/factsheets", "schemas/factsheets"),
    ("src/schemas/concert", "schemas/concert"),
    ("src/schemas/concert_workflows", "schemas/concert_workflows"),
    ("src/schemas/instana", "schemas/instana"),
    ("src/schemas/planning_analytics", "schemas/planning_analytics"),
];

/// Valid field types in schema YAML definitions.
const VALID_FIELD_TYPES: &[&str] = &["string", "integer", "float", "boolean", "object", "array", "timestamp"];

// ============================================================================
// Schema Parsing Structures
// ============================================================================

#[derive(Deserialize)]
struct SchemaFile {
    resource: ResourceDef,
}

#[derive(Deserialize)]
struct ResourceDef {
    name: String,
    service: String,
    kind: String,
    #[serde(default)]
    description: String,
    schema: SchemaDef,
    /// Constraints under which this resource kind is not supported.
    /// Mirrors `wxctl_schema::schema::ResourceDefinition::unsupported_on`.
    #[serde(default)]
    unsupported_on: Vec<String>,
    /// Optional prompt-authoring block. Only `notes` is baked into the build
    /// graph (via `RESOURCE_PROMPT_NOTES`); other prompt sub-keys are ignored.
    #[serde(default)]
    prompt: Option<PromptDef>,
}

#[derive(Deserialize)]
struct PromptDef {
    #[serde(default)]
    notes: Vec<String>,
}

#[derive(Deserialize)]
struct SchemaDef {
    fields: Vec<FieldDef>,
    #[serde(default)]
    #[allow(dead_code)]
    discriminator: Option<String>,
    #[serde(default)]
    variants: Option<HashMap<String, VariantDef>>,
}

#[derive(Deserialize, Clone)]
#[allow(dead_code)]
struct VariantDef {
    #[serde(default)]
    applies_to: Vec<String>,
    #[serde(default)]
    fields: Vec<FieldDef>,
}

#[derive(Deserialize, Clone)]
struct FieldDef {
    #[serde(default)]
    name: String,
    #[serde(rename = "type")]
    field_type: String,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    location: Option<String>,
    #[serde(default)]
    references: Option<FieldReferences>,
    #[serde(default)]
    properties: Option<HashMap<String, FieldDef>>,
    /// Explicit nested `schema: {fields: [...]}` block (the other spelling of
    /// `properties:`). Parsed here only so the PATH_FIELDS extractor can see
    /// `is_path` flags inside it — dependency edges still come from `properties`.
    #[serde(default)]
    schema: Option<NestedSchemaDef>,
    #[serde(default)]
    is_path: bool,
    /// Data-provisioning marker: `Some(true)` opts this field into synthesized-data
    /// detection, `Some(false)` suppresses inference. Threaded into SYNTH_FIELDS.
    #[serde(default)]
    synthesize: Option<bool>,
    /// Optional shape hint (e.g. "csv") paired with `synthesize`.
    #[serde(default)]
    synth_shape: Option<String>,
}

#[derive(Deserialize, Clone)]
struct NestedSchemaDef {
    #[serde(default)]
    fields: Vec<FieldDef>,
}

#[derive(Deserialize, Clone)]
struct FieldReferences {
    resource: String,
    #[serde(rename = "field")]
    _field: String,
    /// Additional target kinds accepted for this reference. Used by union-
    /// reference fields such as `storage_registration.bucket` which may point
    /// at `s3_bucket`, `adls_container`, or `gcs_bucket`. The primary
    /// `resource` still drives dependency graph edges; `also_allows` are
    /// tracked as secondary edges for completeness.
    #[serde(default)]
    also_allows: Vec<String>,
    /// When true, the reference dependency is optional even if the field is required.
    /// Use for fields like agent.llm that accept both literal strings and ${model.xxx} refs.
    #[serde(default)]
    optional: bool,
}

// ============================================================================
// Linkage Parsing Structures
// ============================================================================

#[derive(Deserialize)]
struct LinkagesFile {
    bridges: Vec<BridgeDef>,
}

#[derive(Deserialize)]
struct BridgeDef {
    name: String,
    source: String,
    target: String,
    constraints: HashMap<String, HashMap<String, serde_norway::Value>>,
    field_mapping: Vec<FieldMappingDef>,
    /// Phase 3 — optional deployment scope. Stored as the raw constraint
    /// string (or list-as-comma-string); parsed at runtime via
    /// `DeploymentConstraintList::from_str`. Empty string = always active.
    #[serde(default)]
    when: Option<BridgeWhenDef>,
}

#[derive(Deserialize, Clone)]
struct BridgeWhenDef {
    #[serde(default)]
    deployment: Option<serde_norway::Value>,
}

fn bridge_when_string(when: &Option<BridgeWhenDef>) -> String {
    let Some(w) = when else {
        return String::new();
    };
    let Some(d) = &w.deployment else {
        return String::new();
    };
    match d {
        serde_norway::Value::String(s) => s.clone(),
        serde_norway::Value::Sequence(items) => items.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

#[derive(Deserialize)]
struct FieldMappingDef {
    source: String,
    target: String,
}

// ============================================================================
// Processed Resource Data
// ============================================================================

/// A dependency edge from a field to a target resource kind.
struct ProcessedEdge {
    field_name: String,
    target_resource: String,
    required: bool,
}

struct ProcessedResource {
    name: String,
    required_fields: Vec<String>,
    optional_fields: Vec<String>,
    depends_on: Vec<String>,
    /// Per-field dependency edges with required/optional metadata.
    edges: Vec<ProcessedEdge>,
}

/// Recursively collect dependency edges from fields, tracking the dot-separated path
/// and whether all ancestors are required.
fn collect_edges_recursive(fields: &[FieldDef], prefix: &str, ancestors_required: bool, deps_set: &mut HashSet<String>, edges: &mut Vec<ProcessedEdge>) {
    for field in fields {
        if field.location.as_deref() == Some("Computed") || field.location.as_deref() == Some("LocalOnly") {
            continue;
        }

        let field_path = if prefix.is_empty() { field.name.clone() } else { format!("{}.{}", prefix, field.name) };

        let effectively_required = ancestors_required && field.required;

        // Check for references on this field — primary resource drives the
        // graph edge; `also_allows` kinds emit secondary edges so the DAG
        // knows the union reference is valid.
        if let Some(ref refs) = field.references {
            deps_set.insert(refs.resource.clone());
            edges.push(ProcessedEdge { field_name: field_path.clone(), target_resource: refs.resource.clone(), required: effectively_required && !refs.optional });
            for also in &refs.also_allows {
                deps_set.insert(also.clone());
                edges.push(ProcessedEdge { field_name: field_path.clone(), target_resource: also.clone(), required: false });
            }
        }

        // Recurse into properties (named sub-fields of an object)
        if let Some(ref props) = field.properties {
            let child_fields = properties_as_fields(props);
            collect_edges_recursive(&child_fields, &field_path, effectively_required, deps_set, edges);
        }
    }
}

/// Convert a field's `properties` map into a `Vec<FieldDef>` in sorted-key order
/// (HashMap iteration is nondeterministic and this feeds generated code), filling
/// each child's empty `name` from its map key.
fn properties_as_fields(props: &HashMap<String, FieldDef>) -> Vec<FieldDef> {
    let mut keys: Vec<&String> = props.keys().collect();
    keys.sort_unstable();
    keys.into_iter()
        .map(|key| {
            let mut fd = props[key].clone();
            if fd.name.is_empty() {
                fd.name = key.clone();
            }
            fd
        })
        .collect()
}

/// Variant groups in sorted-key order — `variants` is a HashMap, so iterating
/// `values()` directly would make the generated code nondeterministic.
fn sorted_variants(variants: &HashMap<String, VariantDef>) -> Vec<&VariantDef> {
    let mut keys: Vec<&String> = variants.keys().collect();
    keys.sort_unstable();
    keys.into_iter().map(|k| &variants[k]).collect()
}

fn process_schemas(schemas: &[ResourceDef]) -> Vec<ProcessedResource> {
    schemas
        .iter()
        .map(|schema| {
            let mut required_fields = Vec::new();
            let mut optional_fields = Vec::new();

            // Merge common and variant fields for the field inventory. Variant
            // fields are never "required" at the flat-spec level (they are
            // only required within their variant), so they go in optional.
            let mut all_fields: Vec<&FieldDef> = schema.schema.fields.iter().collect();
            if let Some(variants) = &schema.schema.variants {
                for variant in sorted_variants(variants) {
                    for f in &variant.fields {
                        all_fields.push(f);
                    }
                }
            }

            let mut seen_names: HashSet<&str> = HashSet::new();
            for field in &all_fields {
                if field.location.as_deref() == Some("Computed") {
                    continue;
                }
                if field.location.as_deref() == Some("LocalOnly") {
                    continue;
                }
                if !seen_names.insert(field.name.as_str()) {
                    continue;
                }
                if field.required && schema.schema.fields.iter().any(|f| f.name == field.name) {
                    required_fields.push(field.name.clone());
                } else {
                    optional_fields.push(field.name.clone());
                }
            }

            let mut deps_set: HashSet<String> = HashSet::new();
            let mut edges = Vec::new();
            collect_edges_recursive(&schema.schema.fields, "", true, &mut deps_set, &mut edges);
            if let Some(variants) = &schema.schema.variants {
                for variant in sorted_variants(variants) {
                    collect_edges_recursive(&variant.fields, "", false, &mut deps_set, &mut edges);
                }
            }

            // Sorted so the emitted `_DEPS` arrays (and graph edge insertion order,
            // which feeds topo-sort tie-breaking) are deterministic.
            let mut depends_on: Vec<String> = deps_set.into_iter().collect();
            depends_on.sort_unstable();

            ProcessedResource { name: schema.name.clone(), required_fields, optional_fields, depends_on, edges }
        })
        .collect()
}

// ============================================================================
// Build-Time Validation
// ============================================================================

fn validate_references_recursive(schema_name: &str, fields: &[FieldDef], prefix: &str, all_names: &HashSet<&str>) {
    for field in fields {
        let field_path = if prefix.is_empty() { field.name.clone() } else { format!("{}.{}", prefix, field.name) };

        if let Some(ref refs) = field.references {
            if !all_names.contains(refs.resource.as_str()) {
                panic!("Schema '{}', field '{}': references unknown resource '{}'", schema_name, field_path, refs.resource);
            }
            for also in &refs.also_allows {
                if !all_names.contains(also.as_str()) {
                    panic!("Schema '{}', field '{}': references.also_allows lists unknown resource '{}'", schema_name, field_path, also);
                }
            }
        }

        if let Some(ref props) = field.properties {
            let child_fields = properties_as_fields(props);
            validate_references_recursive(schema_name, &child_fields, &field_path, all_names);
        }
    }
}

fn validate_schemas(schemas: &[ResourceDef]) {
    let all_names: HashSet<&str> = schemas.iter().map(|s| s.name.as_str()).collect();

    for schema in schemas {
        // Validate service and kind are non-empty
        if schema.service.is_empty() {
            panic!("Schema '{}': 'service' field must not be empty", schema.name);
        }
        if schema.kind.is_empty() {
            panic!("Schema '{}': 'kind' field must not be empty", schema.name);
        }

        // Validate common and variant field types
        let mut all_field_refs: Vec<&FieldDef> = schema.schema.fields.iter().collect();
        if let Some(variants) = &schema.schema.variants {
            for variant in sorted_variants(variants) {
                for f in &variant.fields {
                    all_field_refs.push(f);
                }
            }
        }
        for field in &all_field_refs {
            if !VALID_FIELD_TYPES.contains(&field.field_type.as_str()) {
                panic!("Schema '{}', field '{}': invalid type '{}'. Must be one of: {}", schema.name, field.name, field.field_type, VALID_FIELD_TYPES.join(", "));
            }
        }

        // Validate references (including nested and variant-scoped) resolve to known schemas
        validate_references_recursive(&schema.name, &schema.schema.fields, "", &all_names);
        if let Some(variants) = &schema.schema.variants {
            for variant in sorted_variants(variants) {
                validate_references_recursive(&schema.name, &variant.fields, "", &all_names);
            }
        }
    }
}

fn validate_linkages(linkages: &LinkagesFile, all_names: &HashSet<String>, schemas: &[ResourceDef]) {
    // Build a lookup: resource_name → set of top-level field names
    let field_lookup: HashMap<&str, HashSet<&str>> = schemas
        .iter()
        .map(|s| {
            let fields: HashSet<&str> = s.schema.fields.iter().map(|f| f.name.as_str()).collect();
            (s.name.as_str(), fields)
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

// ============================================================================
// Rust Code Generator
// ============================================================================

/// Mangle a schema name into a Rust constant name.
/// e.g. "agent" → "SCHEMA_AGENT"
fn mangle_name(name: &str) -> String {
    format!("SCHEMA_{}", name.to_uppercase().replace('-', "_"))
}

/// Extract the first sentence from a description string.
///
/// Splits at the first `". "` that is a real sentence boundary — i.e. the token
/// ending at that period is not a known abbreviation. Without this guard,
/// descriptions like `… model (incl. AutoAI) …` or `… search (e.g. Milvus). …`
/// truncate mid-sentence at the abbreviation's period. `etc.` is deliberately
/// NOT listed: a trailing `etc.` is a legitimate sentence end, and mid-sentence
/// it appears as `etc.)` (no following space), so it never mis-splits.
fn first_sentence(desc: &str) -> String {
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

fn generate_rust_code(order: &[String], resources: &[ProcessedResource], include_paths: &HashMap<String, String>, schemas: &[ResourceDef]) -> String {
    // Build name → index mapping from topological order
    let name_to_idx: HashMap<&str, usize> = order.iter().enumerate().map(|(i, name)| (name.as_str(), i)).collect();

    let mut code = String::new();

    // Header
    writeln!(code, "// Auto-generated by build.rs - DO NOT EDIT").unwrap();
    writeln!(code).unwrap();

    // ── Schema string constants (include_str!) ──
    for name in order {
        let path = &include_paths[name];
        let const_name = mangle_name(name);
        writeln!(code, "const {}: &str = include_str!(\"{}\");", const_name, path).unwrap();
    }
    writeln!(code).unwrap();

    // ── load_all_schemas() function ──
    // Each parse is wrapped so a strict-parse failure names the offending schema
    // (an anonymous "Failed to deserialize schema" across ~100 embedded files is
    // undebuggable).
    writeln!(code, "pub fn load_all_schemas() -> ::anyhow::Result<Vec<crate::schema::ResourceSchema>> {{").unwrap();
    writeln!(code, "    Ok(vec![").unwrap();
    for name in order {
        let const_name = mangle_name(name);
        let include_path = &include_paths[name];
        writeln!(code, "        crate::schema::SchemaParser::parse_str({}).map_err(|e| e.context(\"schema '{}' ({})\"))?,", const_name, name, include_path.replace('\\', "\\\\").replace('"', "\\\"")).unwrap();
    }
    writeln!(code, "    ])").unwrap();
    writeln!(code, "}}").unwrap();
    writeln!(code).unwrap();

    // ── Dependency graph statics (unchanged logic) ──

    // Type alias for resource tuple to reduce complexity
    writeln!(code, "/// Type alias for resource data tuple: (name, required_fields, optional_fields, dependency_indices)").unwrap();
    writeln!(code, "pub type ResourceTuple = (&'static str, &'static [&'static str], &'static [&'static str], &'static [usize]);").unwrap();
    writeln!(code).unwrap();

    // Type alias for edge tuple: (field_name, target_resource_index, required)
    writeln!(code, "/// Dependency edge: (field_name, target_resource_index, field_is_required)").unwrap();
    writeln!(code, "pub type EdgeTuple = (&'static str, usize, bool);").unwrap();
    writeln!(code).unwrap();

    // Generate phf map for O(1) name → index lookup
    let mut phf_map = phf_codegen::Map::new();
    let entries: Vec<(&str, String)> = name_to_idx.iter().map(|(name, &idx)| (*name, idx.to_string())).collect();
    for (name, val) in &entries {
        phf_map.entry(name, val);
    }
    writeln!(code, "/// O(1) lookup from resource name to index.\npub static RESOURCE_INDEX: phf::Map<&'static str, usize> = {};", phf_map.build()).unwrap();
    writeln!(code).unwrap();

    // Generate TOPOLOGICAL_ORDER
    writeln!(code, "pub static TOPOLOGICAL_ORDER: &[&str] = &[").unwrap();
    for name in order {
        writeln!(code, "    \"{}\",", name).unwrap();
    }
    writeln!(code, "];").unwrap();
    writeln!(code).unwrap();

    // Generate RESOURCE_COUNT
    writeln!(code, "pub const RESOURCE_COUNT: usize = {};", order.len()).unwrap();
    writeln!(code).unwrap();

    // Generate individual resource data as static arrays
    for res in resources {
        let upper_name = res.name.to_uppercase().replace('-', "_");

        // Required fields
        writeln!(code, "static {}_REQUIRED: &[&str] = &[", upper_name).unwrap();
        for f in &res.required_fields {
            writeln!(code, "    \"{}\",", f).unwrap();
        }
        writeln!(code, "];").unwrap();

        // Optional fields
        writeln!(code, "static {}_OPTIONAL: &[&str] = &[", upper_name).unwrap();
        for f in &res.optional_fields {
            writeln!(code, "    \"{}\",", f).unwrap();
        }
        writeln!(code, "];").unwrap();

        // Dependencies as indices (not strings)
        writeln!(code, "static {}_DEPS: &[usize] = &[", upper_name).unwrap();
        for dep_name in &res.depends_on {
            if let Some(&idx) = name_to_idx.get(dep_name.as_str()) {
                writeln!(code, "    {},", idx).unwrap();
            }
        }
        writeln!(code, "];").unwrap();

        // Dependency edges with field-level metadata (for conditional edge activation)
        writeln!(code, "static {}_EDGES: &[EdgeTuple] = &[", upper_name).unwrap();
        for edge in &res.edges {
            if let Some(&idx) = name_to_idx.get(edge.target_resource.as_str()) {
                writeln!(code, "    (\"{}\", {}, {}),", edge.field_name, idx, edge.required).unwrap();
            }
        }
        writeln!(code, "];").unwrap();
        writeln!(code).unwrap();
    }

    // Generate RESOURCES array with index-based dependencies
    writeln!(code, "pub static RESOURCES: &[ResourceTuple] = &[").unwrap();
    for name in order {
        let upper_name = name.to_uppercase().replace('-', "_");
        writeln!(code, "    (\"{}\", {}_REQUIRED, {}_OPTIONAL, {}_DEPS),", name, upper_name, upper_name, upper_name).unwrap();
    }
    writeln!(code, "];").unwrap();
    writeln!(code).unwrap();

    // Generate RESOURCE_EDGES array (parallel to RESOURCES) for conditional edge activation
    writeln!(code, "/// Per-resource dependency edges with field-level metadata.").unwrap();
    writeln!(code, "/// Parallel array to RESOURCES: RESOURCE_EDGES[i] contains edges for RESOURCES[i].").unwrap();
    writeln!(code, "pub static RESOURCE_EDGES: &[&[EdgeTuple]] = &[").unwrap();
    for name in order {
        let upper_name = name.to_uppercase().replace('-', "_");
        writeln!(code, "    {}_EDGES,", upper_name).unwrap();
    }
    writeln!(code, "];").unwrap();
    writeln!(code).unwrap();

    // ── Resource catalog: (kind, service, first_sentence_of_description) ──
    let schema_lookup: HashMap<&str, &ResourceDef> = schemas.iter().map(|s| (s.name.as_str(), s)).collect();
    writeln!(code, "/// Resource catalog: (kind, service, description).").unwrap();
    writeln!(code, "pub static RESOURCE_CATALOG: &[(&str, &str, &str)] = &[").unwrap();
    for name in order {
        if let Some(schema) = schema_lookup.get(name.as_str()) {
            let desc = first_sentence(&schema.description);
            writeln!(code, "    (\"{}\", \"{}\", \"{}\"),", schema.kind, schema.service, desc.replace('\\', "\\\\").replace('"', "\\\"")).unwrap();
        }
    }
    writeln!(code, "];").unwrap();
    writeln!(code).unwrap();

    // ── RESOURCE_PROMPT_NOTES: (kind, &[note]) parallel to RESOURCE_CATALOG ──
    // Iterated over `order` with the same `schema_lookup` so the two tables stay
    // index-aligned. `{:?}` debug-formats each &str into a correctly escaped Rust
    // literal (notes contain backticks, quotes, and `${...}`).
    writeln!(code, "/// Prompt authoring notes per kind: (kind, &[note]). Parallel to RESOURCE_CATALOG.").unwrap();
    writeln!(code, "pub static RESOURCE_PROMPT_NOTES: &[(&str, &[&str])] = &[").unwrap();
    for name in order {
        if let Some(schema) = schema_lookup.get(name.as_str()) {
            let notes = schema.prompt.as_ref().map(|p| p.notes.as_slice()).unwrap_or(&[]);
            let mut entry = format!("    ({:?}, &[", schema.kind);
            for note in notes {
                entry.push_str(&format!("{:?}, ", note));
            }
            entry.push_str("]),");
            writeln!(code, "{}", entry).unwrap();
        }
    }
    writeln!(code, "];").unwrap();
    writeln!(code).unwrap();

    // ── Per-kind unsupported_on constraints (parallel to RESOURCES) ──
    writeln!(code, "/// Per-kind `unsupported_on` constraints, indexed parallel to `RESOURCES`.").unwrap();
    writeln!(code, "/// Each entry is the raw comma-joined constraint string (empty = supported everywhere).").unwrap();
    writeln!(code, "pub static UNSUPPORTED_ON: &[&str] = &[").unwrap();
    for name in order {
        let raw = if let Some(schema) = schema_lookup.get(name.as_str()) { schema.unsupported_on.join(", ") } else { String::new() };
        writeln!(code, "    \"{}\",", raw).unwrap();
    }
    writeln!(code, "];").unwrap();

    // ── PATH_FIELDS: (kind, field_name, parent_array_field) for `is_path` fields ──
    // Drives the config-time relative-path resolver (`resolve_file_paths`).
    // `parent_array_field` is `Some(arr)` for a path nested in an array's items
    // (e.g. `documents[].path`); else `None`. The `is_path ⇒ LocalOnly` guard
    // panics the build if a path field could be sent to the API. Positions the
    // (kind, field, parent) format cannot express — nesting deeper than one level,
    // or one level under a non-array parent (the runtime resolver only rewrites
    // inside array items) — panic the build: a loud failure here beats the silent
    // resolve-against-CWD trap at runtime.
    writeln!(code).unwrap();
    writeln!(code, "/// Schema-declared local path fields: (kind, field_name, parent_array_field).").unwrap();
    writeln!(code, "pub static PATH_FIELDS: &[(&str, &str, Option<&str>)] = &[").unwrap();
    for schema in schemas {
        emit_path_fields(&mut code, &schema.kind, &schema.schema.fields);
        if let Some(variants) = &schema.schema.variants {
            // Variant fields sit at the resource's top level in config data, so
            // the same (kind, field, parent) entries work unchanged.
            for variant in sorted_variants(variants) {
                emit_path_fields(&mut code, &schema.kind, &variant.fields);
            }
        }
    }
    writeln!(code, "];").unwrap();

    // ── SYNTH_FIELDS: (kind, field, parent_field, synthesize, synth_shape) ──
    // Drives config-time data-need detection (wxctl-compose-core::data). Unlike
    // PATH_FIELDS this allows object-parent nesting and does not require LocalOnly —
    // detection only reports the field; it never resolves the path at runtime.
    writeln!(code).unwrap();
    writeln!(code, "/// One synthesize marker: (kind, field, parent_field, synthesize, synth_shape).").unwrap();
    writeln!(code, "pub type SynthFieldEntry = (&'static str, &'static str, Option<&'static str>, bool, Option<&'static str>);").unwrap();
    writeln!(code, "/// Schema-declared synthesize markers: (kind, field, parent_field, synthesize, synth_shape).").unwrap();
    writeln!(code, "pub static SYNTH_FIELDS: &[SynthFieldEntry] = &[").unwrap();
    for schema in schemas {
        emit_synth_fields(&mut code, &schema.kind, &schema.schema.fields);
        if let Some(variants) = &schema.schema.variants {
            for variant in sorted_variants(variants) {
                emit_synth_fields(&mut code, &schema.kind, &variant.fields);
            }
        }
    }
    writeln!(code, "];").unwrap();

    code
}

/// Child field definitions of a field: `properties:` (sorted) plus an explicit
/// nested `schema.fields` list, both spellings of one level of nesting.
fn child_field_defs(field: &FieldDef) -> Vec<FieldDef> {
    let mut children = field.properties.as_ref().map(properties_as_fields).unwrap_or_default();
    if let Some(nested) = &field.schema {
        children.extend(nested.fields.iter().cloned());
    }
    children
}

/// Panic if `is_path: true` appears anywhere in this subtree — used for depth ≥ 2,
/// which `PATH_FIELDS` cannot express (the runtime resolver would silently fall
/// back to resolving against the CWD).
fn assert_no_deep_is_path(kind: &str, path: &str, field: &FieldDef) {
    assert!(!field.is_path, "schema '{}': field '{}.{}' has `is_path: true` at a depth PATH_FIELDS cannot express (max one level, inside an array field) — the path would silently resolve against the CWD at runtime; restructure the schema or extend resolve_file_paths first", kind, path, field.name);
    for child in child_field_defs(field) {
        assert_no_deep_is_path(kind, &format!("{}.{}", path, field.name), &child);
    }
}

/// Emit PATH_FIELDS entries for one top-level field list (common or variant).
fn emit_path_fields(code: &mut String, kind: &str, fields: &[FieldDef]) {
    for field in fields {
        if field.is_path {
            assert_eq!(field.location.as_deref(), Some("LocalOnly"), "schema '{}': field '{}' has `is_path: true` but location is {:?} (must be LocalOnly — a path must never be sent to the API)", kind, field.name, field.location);
            writeln!(code, "    (\"{}\", \"{}\", None),", kind, field.name).unwrap();
        }
        for sub in child_field_defs(field) {
            if sub.is_path {
                assert_eq!(field.location.as_deref(), Some("LocalOnly"), "schema '{}': sub-field '{}.{}' has `is_path: true` but parent '{}' location is {:?} (must be LocalOnly)", kind, field.name, sub.name, field.name, field.location);
                assert_eq!(
                    field.field_type, "array",
                    "schema '{}': sub-field '{}.{}' has `is_path: true` but parent '{}' is type '{}' — the runtime resolver (resolve_file_paths) only rewrites paths one level deep inside an ARRAY field, so this path would silently resolve against the CWD",
                    kind, field.name, sub.name, field.name, field.field_type
                );
                writeln!(code, "    (\"{}\", \"{}\", Some(\"{}\")),", kind, sub.name, field.name).unwrap();
            }
            for grand in child_field_defs(&sub) {
                assert_no_deep_is_path(kind, &format!("{}.{}", field.name, sub.name), &grand);
            }
        }
    }
}

/// Rust-literal form of an optional string: `None` or `Some("x")`.
fn opt_str(v: &Option<String>) -> String {
    match v {
        Some(s) => format!("Some(\"{}\")", s),
        None => "None".to_string(),
    }
}

/// Emit SYNTH_FIELDS for one field list: any field (or one-level child) with an
/// explicit `synthesize:` marker (Some(_)). Object- and array-parents both allowed.
fn emit_synth_fields(code: &mut String, kind: &str, fields: &[FieldDef]) {
    for field in fields {
        if let Some(syn) = field.synthesize {
            writeln!(code, "    (\"{}\", \"{}\", None, {}, {}),", kind, field.name, syn, opt_str(&field.synth_shape)).unwrap();
        }
        for sub in child_field_defs(field) {
            if let Some(syn) = sub.synthesize {
                writeln!(code, "    (\"{}\", \"{}\", Some(\"{}\"), {}, {}),", kind, sub.name, field.name, syn, opt_str(&sub.synth_shape)).unwrap();
            }
        }
    }
}

fn generate_bridge_code(linkages: &LinkagesFile, name_to_idx: &HashMap<&str, usize>) -> String {
    let mut code = String::new();

    // Type aliases
    writeln!(code, "/// Bridge field mapping: (source_field_path, target_field_path)").unwrap();
    writeln!(code, "pub type FieldMappingTuple = (&'static str, &'static str);").unwrap();
    writeln!(code).unwrap();
    writeln!(code, "/// Bridge constraint: (resource_index, field_name, allowed_values)").unwrap();
    writeln!(code, "pub type ConstraintTuple = (usize, &'static str, &'static [&'static str]);").unwrap();
    writeln!(code).unwrap();
    writeln!(code, "/// Bridge: (name, source_index, target_index, constraints, field_mappings, when_deployment)").unwrap();
    writeln!(code, "pub type BridgeTuple = (&'static str, usize, usize, &'static [ConstraintTuple], &'static [FieldMappingTuple], &'static str);").unwrap();
    writeln!(code).unwrap();

    // Generate per-bridge statics
    for bridge in &linkages.bridges {
        let upper = bridge.name.to_uppercase();
        let _source_idx = name_to_idx[bridge.source.as_str()];
        let _target_idx = name_to_idx[bridge.target.as_str()];

        // Field mappings
        writeln!(code, "static {}_FIELD_MAPPINGS: &[FieldMappingTuple] = &[", upper).unwrap();
        for fm in &bridge.field_mapping {
            writeln!(code, "    (\"{}\", \"{}\"),", fm.source, fm.target).unwrap();
        }
        writeln!(code, "];").unwrap();

        // Constraints — generate values arrays first, then the constraint array.
        // Sorted-key iteration: `constraints` is a nested HashMap, and the emission
        // order must be deterministic.
        let mut constraint_entries = Vec::new();
        let mut resource_names: Vec<&String> = bridge.constraints.keys().collect();
        resource_names.sort_unstable();
        for resource_name in resource_names {
            let fields = &bridge.constraints[resource_name];
            let res_idx = name_to_idx[resource_name.as_str()];
            let mut field_names: Vec<&String> = fields.keys().collect();
            field_names.sort_unstable();
            for field_name in field_names {
                let values = &fields[field_name];
                let upper_constraint = format!("{}_{}_{}", upper, resource_name.to_uppercase().replace('-', "_"), field_name.to_uppercase());
                let values_array = match values {
                    serde_norway::Value::Sequence(seq) => {
                        let strs: Vec<String> = seq.iter().filter_map(|v| v.as_str().map(|s| format!("\"{}\"", s))).collect();
                        strs.join(", ")
                    }
                    serde_norway::Value::String(s) => format!("\"{}\"", s),
                    _ => panic!("Linkage '{}': constraint value must be string or array", bridge.name),
                };
                writeln!(code, "static {}_VALUES: &[&str] = &[{}];", upper_constraint, values_array).unwrap();
                constraint_entries.push((res_idx, field_name.clone(), upper_constraint));
            }
        }
        writeln!(code, "static {}_CONSTRAINTS: &[ConstraintTuple] = &[", upper).unwrap();
        for (res_idx, field_name, upper_constraint) in &constraint_entries {
            writeln!(code, "    ({}, \"{}\", {}_VALUES),", res_idx, field_name, upper_constraint).unwrap();
        }
        writeln!(code, "];").unwrap();
        writeln!(code).unwrap();
    }

    // Generate BRIDGES array
    writeln!(code, "pub static BRIDGES: &[BridgeTuple] = &[").unwrap();
    for bridge in &linkages.bridges {
        let upper = bridge.name.to_uppercase();
        let source_idx = name_to_idx[bridge.source.as_str()];
        let target_idx = name_to_idx[bridge.target.as_str()];
        let when = bridge_when_string(&bridge.when);
        assert!(!when.contains('"'), "bridge '{}' when_deployment contains a quote character — escape it first", bridge.name);
        writeln!(code, "    (\"{}\", {}, {}, {}_CONSTRAINTS, {}_FIELD_MAPPINGS, \"{}\"),", bridge.name, source_idx, target_idx, upper, upper, when).unwrap();
    }
    writeln!(code, "];").unwrap();

    code
}

// ============================================================================
// Main Build Script
// ============================================================================

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let src_dir = Path::new(&manifest_dir).join("src");

    // ── Collect schemas from all directories ──
    let mut all_schemas: Vec<ResourceDef> = Vec::new();
    let mut include_paths: HashMap<String, String> = HashMap::new();
    let mut seen_names: HashSet<String> = HashSet::new();

    for &(fs_dir, include_prefix) in SCHEMA_DIRS {
        let dir = Path::new(fs_dir);
        if !dir.exists() {
            continue;
        }

        let entries = fs::read_dir(dir).unwrap_or_else(|e| panic!("Failed to read schema directory {}: {}", dir.display(), e));

        // Sort by path: read_dir order is filesystem-dependent, and file order feeds
        // node insertion (hence topo-sort tie-breaking) and every emitted table.
        let mut paths: Vec<std::path::PathBuf> = entries.map(|entry| entry.unwrap_or_else(|e| panic!("Failed to read directory entry: {}", e)).path()).collect();
        paths.sort();

        for path in paths {
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
            let is_yaml = path.extension().is_some_and(|ext| ext == "yaml");
            // Parallel-schema sibling files (e.g. `ingestion_job.software.yaml`) would
            // share `resource.name` with the base file; no loader exists for them yet,
            // so fail loudly instead of silently dropping the file.
            let is_parallel_sibling = file_name.matches('.').count() >= 2 && (file_name.ends_with(".software.yaml") || file_name.ends_with(".saas.yaml"));
            if is_parallel_sibling {
                panic!("parallel-sibling schema files are not supported yet: {}", path.display());
            }
            if is_yaml {
                let content = fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read schema file {}: {}", path.display(), e));

                let schema_file: SchemaFile = serde_norway::from_str(&content).unwrap_or_else(|e| panic!("Failed to parse schema {}: {}", path.display(), e));

                let name = schema_file.resource.name.clone();

                // Check for duplicate resource names across directories
                if !seen_names.insert(name.clone()) {
                    panic!("Duplicate schema name '{}' found in {}", name, path.display());
                }

                // Build absolute include_str! path (include_str! in generated files
                // resolves relative to OUT_DIR, so we must use absolute paths).
                // Normalize to forward slashes: the path is embedded verbatim into an
                // include_str!("...") literal, and on Windows the backslash separators
                // would be parsed as invalid string escapes (\a, \w, ...). rustc accepts
                // forward slashes in include paths on all platforms.
                let file_name = path.file_name().unwrap().to_str().unwrap();
                let abs_path = src_dir.join(include_prefix).join(file_name).to_str().unwrap().replace('\\', "/");
                include_paths.insert(name, abs_path);

                all_schemas.push(schema_file.resource);
            }
        }
    }

    // ── Build-time validation ──
    validate_schemas(&all_schemas);

    // ── Load and validate linkages ──
    // rerun-if-changed is emitted unconditionally: if the file is absent now but
    // appears later, the build must still rerun to pick it up.
    println!("cargo:rerun-if-changed=src/linkages.yaml");
    let linkages_path = Path::new("src/linkages.yaml");
    let linkages: LinkagesFile = if linkages_path.exists() {
        let content = fs::read_to_string(linkages_path).expect("Failed to read linkages.yaml");
        let parsed: LinkagesFile = serde_norway::from_str(&content).expect("Failed to parse linkages.yaml");
        validate_linkages(&parsed, &seen_names, &all_schemas);
        parsed
    } else {
        LinkagesFile { bridges: Vec::new() }
    };

    // ── Topological sort using wxctl-graph ──
    let mut graph: IndexGraph<String> = IndexGraph::with_capacity(all_schemas.len());

    // Add all schema nodes
    for schema in &all_schemas {
        graph.add_node(schema.name.clone());
    }

    // Process schemas once — collects dependencies and edges via recursive traversal
    let resources = process_schemas(&all_schemas);

    // Build graph edges from already-computed dependencies (no second traversal)
    for resource in &resources {
        for dep in &resource.depends_on {
            if *dep != resource.name && seen_names.contains(dep) {
                graph.add_edge(resource.name.clone(), dep.clone());
            }
        }
    }

    // Add bridge edges (source depends on target)
    for bridge in &linkages.bridges {
        if bridge.source != bridge.target && seen_names.contains(&bridge.source) && seen_names.contains(&bridge.target) {
            graph.add_edge(bridge.source.clone(), bridge.target.clone());
        }
    }

    let order = graph.topological_sort().unwrap_or_else(|e| {
        // `CycleError` carries the offending nodes in order — surface the path so the
        // failure names which references/bridges to break, instead of "somewhere".
        panic!("Circular dependency detected among resource schemas — break one of these references or bridges:\n    {}", e.cycle.join(" → "));
    });

    // Build name → index mapping from topological order
    let name_to_idx: HashMap<&str, usize> = order.iter().enumerate().map(|(i, name)| (name.as_str(), i)).collect();

    let rust_code = generate_rust_code(&order, &resources, &include_paths, &all_schemas);
    let bridge_code = generate_bridge_code(&linkages, &name_to_idx);

    let rust_code = format!("{}\n{}", rust_code, bridge_code);

    let dest_path = Path::new(&out_dir).join("dependency_graph_generated.rs");
    fs::write(&dest_path, rust_code).expect("Failed to write dependency_graph_generated.rs");

    // ── Emit rerun-if-changed for all schema directories and build.rs ──
    println!("cargo:rerun-if-changed=build.rs");
    for &(fs_dir, _) in SCHEMA_DIRS {
        println!("cargo:rerun-if-changed={}", fs_dir);
    }
}
