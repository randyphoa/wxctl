//! Flat-table codegen emitters (graph/catalog tables) — ported from
//! `wxctl-schema/build.rs` (`generate_rust_code`/`generate_bridge_code` and their
//! helpers: `process_schemas`/`ProcessedResource`/`ProcessedEdge`/
//! `collect_edges_recursive`, `sorted_variants`, `child_field_defs`,
//! `assert_no_deep_is_path`, `emit_path_fields`, `opt_str`, `emit_synth_fields`),
//! rewritten to walk the full schema model (`crate::definition::ResourceDefinition`/
//! `FieldDefinition`) instead of the reduced YAML-only structs `build.rs` used.
//! The dependency-graph/catalog statics changed once, deliberately, on
//! 2026-07-20: edge collection moved off the legacy `properties:`-only view
//! onto the unified model, so `references:` nested under an explicit `schema:`
//! block now emits an edge (7 kinds). The `load_all_schemas()` function and its
//! per-schema `include_str!` consts that file also used to carry are no longer
//! emitted here (the runtime parse path was deleted — static IR only).

use crate::build_meta::{LinkagesFile, ParsedSchema, bridge_when_string};
use crate::definition::{FieldDefinition, FieldLocation, FieldType, VariantDefinition};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as FmtWrite;
use wxctl_graph::IndexGraph;

// ============================================================================
// Processed Resource Data
// ============================================================================

/// A dependency edge from a field to a target resource kind.
struct ProcessedEdge {
    field_name: String,
    target_resource: String,
    /// Derived: `ancestors_required && field.required && !ref.optional`. UNCHANGED — the golden reads this.
    required: bool,
    /// `ancestors_required && field.required` (field-side strength).
    field_required: bool,
    /// The reference's `optional:` flag (ref-side strength).
    ref_optional: bool,
    /// Edge minted from `also_allows` (union alternative).
    union_secondary: bool,
    /// Execution-time readiness gate.
    require_ready: bool,
    /// `relationship: containment` marker → `mechanism: containment` in the export.
    containment: bool,
}

struct ProcessedResource {
    name: String,
    required_fields: Vec<String>,
    optional_fields: Vec<String>,
    depends_on: Vec<String>,
    /// Per-field dependency edges with required/optional metadata.
    edges: Vec<ProcessedEdge>,
}

/// Nested children of a field = its `schema.fields` (post-normalize), sorted by
/// name to reproduce the old `properties_as_fields` key-sort (D7). Explicit-`schema:`
/// and `properties:` both land here after normalize; sorting is a no-op for
/// already-sorted maps.
fn child_field_defs(field: &FieldDefinition) -> Vec<&FieldDefinition> {
    let mut kids: Vec<&FieldDefinition> = field.schema.as_ref().map(|s| s.fields.iter().collect()).unwrap_or_default();
    kids.sort_by(|a, b| a.name.cmp(&b.name));
    kids
}

/// Variant groups in sorted-key order — `variants` is a HashMap, so iterating
/// `values()` directly would make the generated code nondeterministic.
fn sorted_variants(variants: &HashMap<String, VariantDefinition>) -> Vec<&VariantDefinition> {
    let mut keys: Vec<&String> = variants.keys().collect();
    keys.sort_unstable();
    keys.into_iter().map(|k| &variants[k]).collect()
}

/// Recursively collect dependency edges from a field list, tracking the
/// dot-separated path and whether all ancestors are required.
///
/// Walks the SAME typed model (`crate::definition::FieldDefinition` +
/// `child_field_defs`) that `PATH_FIELDS`/`SYNTH_FIELDS` already walk, so a
/// `references:` block nested under an explicit `schema: {fields: [...]}` now
/// produces an edge (the pre-2026-07-20 `properties:`-only recursion silently
/// dropped it for 7 kinds). `Computed`/`LocalOnly` fields are still skipped —
/// see `docs/troubleshoot/ordering-only-reference-fields-localonly-no-edge-fix.md`.
fn collect_edges_recursive(fields: &[FieldDefinition], prefix: &str, ancestors_required: bool, deps_set: &mut HashSet<String>, edges: &mut Vec<ProcessedEdge>) {
    for field in fields {
        collect_field_edges(field, prefix, ancestors_required, deps_set, edges);
    }
}

/// One field of `collect_edges_recursive`'s walk: emit its own edges, then
/// descend into its normalized children (`child_field_defs`).
fn collect_field_edges(field: &FieldDefinition, prefix: &str, ancestors_required: bool, deps_set: &mut HashSet<String>, edges: &mut Vec<ProcessedEdge>) {
    if field.location == FieldLocation::Computed || field.location == FieldLocation::LocalOnly {
        return;
    }

    let field_path = if prefix.is_empty() { field.name.clone() } else { format!("{}.{}", prefix, field.name) };

    let effectively_required = ancestors_required && field.required;

    // Check for references on this field — primary resource drives the
    // graph edge; `also_allows` kinds emit secondary edges so the DAG
    // knows the union reference is valid.
    if let Some(refs) = &field.references {
        let containment = matches!(refs.relationship.as_deref(), Some("containment"));
        deps_set.insert(refs.resource.clone());
        edges.push(ProcessedEdge { field_name: field_path.clone(), target_resource: refs.resource.clone(), required: effectively_required && !refs.optional, field_required: effectively_required, ref_optional: refs.optional, union_secondary: false, require_ready: refs.require_ready, containment });
        for also in &refs.also_allows {
            deps_set.insert(also.clone());
            edges.push(ProcessedEdge { field_name: field_path.clone(), target_resource: also.clone(), required: false, field_required: effectively_required, ref_optional: refs.optional, union_secondary: true, require_ready: refs.require_ready, containment });
        }
    }

    for child in child_field_defs(field) {
        collect_field_edges(child, &field_path, effectively_required, deps_set, edges);
    }
}

/// Collapse edges that address the SAME config path and target kind.
/// `monitor_instance` declares both a top-level field literally named
/// `target.target_id` (`location: Query`, discovery scoping) and the nested
/// body leaf `target.target_id`; both resolve to one config location, so they
/// must contribute one edge. Keeps the first occurrence's position and merges
/// strength: `required`/`field_required`/`require_ready`/`containment` OR
/// together, `ref_optional` ANDs (the strongest constraint wins).
fn dedupe_edges(edges: Vec<ProcessedEdge>) -> Vec<ProcessedEdge> {
    let mut out: Vec<ProcessedEdge> = Vec::with_capacity(edges.len());
    for edge in edges {
        if let Some(existing) = out.iter_mut().find(|e| e.field_name == edge.field_name && e.target_resource == edge.target_resource && e.union_secondary == edge.union_secondary) {
            existing.required |= edge.required;
            existing.field_required |= edge.field_required;
            existing.ref_optional &= edge.ref_optional;
            existing.require_ready |= edge.require_ready;
            existing.containment |= edge.containment;
            continue;
        }
        out.push(edge);
    }
    out
}

fn process_schemas(schemas: &[ParsedSchema]) -> Vec<ProcessedResource> {
    schemas
        .iter()
        .map(|parsed| {
            let resource = &parsed.schema.resource;
            let mut required_fields = Vec::new();
            let mut optional_fields = Vec::new();

            // Merge common and variant fields for the field inventory. Variant
            // fields are never "required" at the flat-spec level (they are
            // only required within their variant), so they go in optional.
            let mut all_fields: Vec<&FieldDefinition> = resource.schema.fields.iter().collect();
            if let Some(variants) = &resource.schema.variants {
                for variant in sorted_variants(variants) {
                    for f in &variant.fields {
                        all_fields.push(f);
                    }
                }
            }

            let mut seen_names: HashSet<&str> = HashSet::new();
            for field in &all_fields {
                if field.location == FieldLocation::Computed || field.location == FieldLocation::LocalOnly {
                    continue;
                }
                if !seen_names.insert(field.name.as_str()) {
                    continue;
                }
                if field.required && resource.schema.fields.iter().any(|f| f.name == field.name) {
                    required_fields.push(field.name.clone());
                } else {
                    optional_fields.push(field.name.clone());
                }
            }

            // Edge collection walks the full unified model — the same
            // `child_field_defs` view PATH_FIELDS/SYNTH_FIELDS use.
            let mut deps_set: HashSet<String> = HashSet::new();
            let mut edges = Vec::new();
            collect_edges_recursive(&resource.schema.fields, "", true, &mut deps_set, &mut edges);
            if let Some(variants) = &resource.schema.variants {
                for variant in sorted_variants(variants) {
                    collect_edges_recursive(&variant.fields, "", false, &mut deps_set, &mut edges);
                }
            }
            let edges = dedupe_edges(edges);

            // Sorted so the emitted `_DEPS` arrays (and graph edge insertion order,
            // which feeds topo-sort tie-breaking) are deterministic.
            let mut depends_on: Vec<String> = deps_set.into_iter().collect();
            depends_on.sort_unstable();

            ProcessedResource { name: resource.name.clone(), required_fields, optional_fields, depends_on, edges }
        })
        .collect()
}

// ============================================================================
// Public entry points
// ============================================================================

/// Topological order over schema deps + bridge edges — the compiler owns this now
/// (moved from `wxctl-schema/build.rs:1002-1032`; uses `wxctl_graph::IndexGraph`).
/// Panics on a cycle with the offending path, same message as `build.rs:1028-1032`.
pub fn topo_order(schemas: &[ParsedSchema], linkages: &LinkagesFile) -> Vec<String> {
    let seen_names: HashSet<&str> = schemas.iter().map(|s| s.schema.resource.name.as_str()).collect();

    let mut graph: IndexGraph<String> = IndexGraph::with_capacity(schemas.len());

    // Add all schema nodes.
    for parsed in schemas {
        graph.add_node(parsed.schema.resource.name.clone());
    }

    // Process schemas once — collects dependencies and edges via recursive traversal.
    let resources = process_schemas(schemas);

    // Build graph edges from already-computed dependencies (no second traversal).
    for resource in &resources {
        for dep in &resource.depends_on {
            if *dep != resource.name && seen_names.contains(dep.as_str()) {
                graph.add_edge(resource.name.clone(), dep.clone());
            }
        }
    }

    // Add bridge edges (source depends on target).
    for bridge in &linkages.bridges {
        if bridge.source != bridge.target && seen_names.contains(bridge.source.as_str()) && seen_names.contains(bridge.target.as_str()) {
            graph.add_edge(bridge.source.clone(), bridge.target.clone());
        }
    }

    graph.topological_sort().unwrap_or_else(|e| {
        // `CycleError` carries the offending nodes in order — surface the path so the
        // failure names which references/bridges to break, instead of "somewhere".
        panic!("Circular dependency detected among resource schemas — break one of these references or bridges:\n    {}", e.cycle.join(" → "));
    })
}

/// Full generated string: dependency-graph/catalog statics, followed by
/// `generate_bridge_code`'s bridge statics. `load_all_schemas()` and its
/// per-schema `include_str!` consts are no longer emitted here (the runtime
/// parse path was replaced by the static IR — see `codegen::ir::generate_ir`).
pub fn generate_tables(order: &[String], schemas: &[ParsedSchema], linkages: &LinkagesFile) -> String {
    let name_to_idx: HashMap<&str, usize> = order.iter().enumerate().map(|(i, name)| (name.as_str(), i)).collect();
    let resources = process_schemas(schemas);

    let rust_code = generate_rust_code(order, &resources, schemas, &name_to_idx);
    let bridge_code = generate_bridge_code(linkages, &name_to_idx);

    format!("{}\n{}", rust_code, bridge_code)
}

// ============================================================================
// Rust Code Generator
// ============================================================================

fn generate_rust_code(order: &[String], resources: &[ProcessedResource], schemas: &[ParsedSchema], name_to_idx: &HashMap<&str, usize>) -> String {
    let mut code = String::new();

    // Header
    writeln!(code, "// Auto-generated by build.rs - DO NOT EDIT").unwrap();
    writeln!(code).unwrap();

    // ── Dependency graph statics (unchanged logic) ──

    // Type alias for resource tuple to reduce complexity
    writeln!(code, "/// Type alias for resource data tuple: (name, required_fields, optional_fields, dependency_indices)").unwrap();
    writeln!(code, "pub type ResourceTuple = (&'static str, &'static [&'static str], &'static [&'static str], &'static [usize]);").unwrap();
    writeln!(code).unwrap();

    // Type alias for edge tuple: 8-field taxonomy record
    writeln!(code, "/// Dependency edge: (field_name, target_index, required, field_required, ref_optional, union_secondary, require_ready, containment)").unwrap();
    writeln!(code, "pub type EdgeTuple = (&'static str, usize, bool, bool, bool, bool, bool, bool);").unwrap();
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
                writeln!(code, "    (\"{}\", {}, {}, {}, {}, {}, {}, {}),", edge.field_name, idx, edge.required, edge.field_required, edge.ref_optional, edge.union_secondary, edge.require_ready, edge.containment).unwrap();
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
    let schema_lookup: HashMap<&str, &ParsedSchema> = schemas.iter().map(|s| (s.schema.resource.name.as_str(), s)).collect();
    writeln!(code, "/// Resource catalog: (kind, service, description).").unwrap();
    writeln!(code, "pub static RESOURCE_CATALOG: &[(&str, &str, &str)] = &[").unwrap();
    for name in order {
        if let Some(parsed) = schema_lookup.get(name.as_str()) {
            let resource = &parsed.schema.resource;
            let desc = crate::validation::first_sentence(resource.description.as_deref().unwrap_or(""));
            writeln!(code, "    (\"{}\", \"{}\", \"{}\"),", resource.kind, resource.service, desc.replace('\\', "\\\\").replace('"', "\\\"")).unwrap();
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
        if let Some(parsed) = schema_lookup.get(name.as_str()) {
            let resource = &parsed.schema.resource;
            let notes: Vec<String> = resource.prompt.as_ref().and_then(|p| p.get("notes")).and_then(|n| n.as_sequence()).map(|seq| seq.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect()).unwrap_or_default();
            let mut entry = format!("    ({:?}, &[", resource.kind);
            for note in &notes {
                entry.push_str(&format!("{:?}, ", note));
            }
            entry.push_str("]),");
            writeln!(code, "{}", entry).unwrap();
        }
    }
    writeln!(code, "];").unwrap();
    writeln!(code).unwrap();

    // ── RESOURCE_ADVISORIES: (kind, &[(severity, tier, date, text)]) parallel to RESOURCE_CATALOG ──
    // Iterated over `order` with the same `schema_lookup` so the two tables stay
    // index-aligned; advisories come from each `ParsedSchema`'s own top-level
    // `advisories:` block (one schema file = one kind, so this is equivalent to the
    // old build.rs's `advisories_by_kind.get(&schema.kind)` lookup).
    // Named via a type alias (like EdgeTuple/ConstraintTuple) — the raw 4-tuple form
    // trips clippy::type_complexity.
    writeln!(code, "/// One advisory: (severity, tier, date, text).").unwrap();
    writeln!(code, "pub type AdvisoryTuple = (&'static str, &'static str, &'static str, &'static str);").unwrap();
    writeln!(code, "/// Published advisories per kind: (kind, &[(severity, tier, date, text)]). Parallel to RESOURCE_CATALOG.").unwrap();
    writeln!(code, "pub static RESOURCE_ADVISORIES: &[(&str, &[AdvisoryTuple])] = &[").unwrap();
    for name in order {
        if let Some(parsed) = schema_lookup.get(name.as_str()) {
            let resource = &parsed.schema.resource;
            let mut entry = format!("    ({:?}, &[", resource.kind);
            for a in &parsed.advisories {
                assert!(a.severity == "info" || a.severity == "warn", "advisory severity on {} must be info|warn, got {:?}", resource.kind, a.severity);
                entry.push_str(&format!("({:?}, {:?}, {:?}, {:?}), ", a.severity, a.tier, a.date, a.text));
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
        let raw = if let Some(parsed) = schema_lookup.get(name.as_str()) { parsed.unsupported_on_raw.join(", ") } else { String::new() };
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
    //
    // NOTE: iterates `schemas` (the caller's original scan order), not the
    // topological `order` — matches `wxctl-schema/build.rs:737`.
    writeln!(code).unwrap();
    writeln!(code, "/// Schema-declared local path fields: (kind, field_name, parent_array_field).").unwrap();
    writeln!(code, "pub static PATH_FIELDS: &[(&str, &str, Option<&str>)] = &[").unwrap();
    for parsed in schemas {
        let resource = &parsed.schema.resource;
        emit_path_fields(&mut code, &resource.kind, &resource.schema.fields);
        if let Some(variants) = &resource.schema.variants {
            // Variant fields sit at the resource's top level in config data, so
            // the same (kind, field, parent) entries work unchanged.
            for variant in sorted_variants(variants) {
                emit_path_fields(&mut code, &resource.kind, &variant.fields);
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
    for parsed in schemas {
        let resource = &parsed.schema.resource;
        emit_synth_fields(&mut code, &resource.kind, &resource.schema.fields);
        if let Some(variants) = &resource.schema.variants {
            for variant in sorted_variants(variants) {
                emit_synth_fields(&mut code, &resource.kind, &variant.fields);
            }
        }
    }
    writeln!(code, "];").unwrap();

    code
}

/// Panic if `is_path: true` appears anywhere in this subtree — used for depth ≥ 2,
/// which `PATH_FIELDS` cannot express (the runtime resolver would silently fall
/// back to resolving against the CWD).
fn assert_no_deep_is_path(kind: &str, path: &str, field: &FieldDefinition) {
    assert!(!field.is_path, "schema '{}': field '{}.{}' has `is_path: true` at a depth PATH_FIELDS cannot express (max one level, inside an array field) — the path would silently resolve against the CWD at runtime; restructure the schema or extend resolve_file_paths first", kind, path, field.name);
    for child in child_field_defs(field) {
        assert_no_deep_is_path(kind, &format!("{}.{}", path, field.name), child);
    }
}

/// Emit PATH_FIELDS entries for one top-level field list (common or variant).
fn emit_path_fields(code: &mut String, kind: &str, fields: &[FieldDefinition]) {
    for field in fields {
        if field.is_path {
            assert_eq!(field.location, FieldLocation::LocalOnly, "schema '{}': field '{}' has `is_path: true` but location is {:?} (must be LocalOnly — a path must never be sent to the API)", kind, field.name, field.location);
            writeln!(code, "    (\"{}\", \"{}\", None),", kind, field.name).unwrap();
        }
        for sub in child_field_defs(field) {
            if sub.is_path {
                assert_eq!(field.location, FieldLocation::LocalOnly, "schema '{}': sub-field '{}.{}' has `is_path: true` but parent '{}' location is {:?} (must be LocalOnly)", kind, field.name, sub.name, field.name, field.location);
                assert!(
                    matches!(field.field_type, FieldType::Array),
                    "schema '{}': sub-field '{}.{}' has `is_path: true` but parent '{}' is type '{:?}' — the runtime resolver (resolve_file_paths) only rewrites paths one level deep inside an ARRAY field, so this path would silently resolve against the CWD",
                    kind,
                    field.name,
                    sub.name,
                    field.name,
                    field.field_type
                );
                writeln!(code, "    (\"{}\", \"{}\", Some(\"{}\")),", kind, sub.name, field.name).unwrap();
            }
            for grand in child_field_defs(sub) {
                assert_no_deep_is_path(kind, &format!("{}.{}", field.name, sub.name), grand);
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
fn emit_synth_fields(code: &mut String, kind: &str, fields: &[FieldDefinition]) {
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
