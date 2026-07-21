use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::Path;

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
    ("src/schemas/pa_workspace", "schemas/pa_workspace"),
    ("src/schemas/vault", "schemas/vault"),
];

// ============================================================================
// Main Build Script
// ============================================================================
//
// Parsing, validation, topological ordering, and table/IR codegen are all
// owned by `wxctl-schema-compiler` (a build-dependency-only crate) — this
// script's job is just file collection (this crate is the only one that knows
// where its schema YAML lives) plus wiring the compiler's output into OUT_DIR.

fn main() {
    let out_dir = env::var("OUT_DIR").expect("OUT_DIR not set");
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set");
    let src_dir = Path::new(&manifest_dir).join("src");

    // ── Collect + parse schemas from all directories ──
    let mut parsed_schemas: Vec<wxctl_schema_compiler::build_meta::ParsedSchema> = Vec::new();
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

                // Build absolute include_str! path (include_str! in generated files
                // resolves relative to OUT_DIR, so we must use absolute paths).
                // Normalize to forward slashes: the path is embedded verbatim into an
                // include_str!("...") literal, and on Windows the backslash separators
                // would be parsed as invalid string escapes (\a, \w, ...). rustc accepts
                // forward slashes in include paths on all platforms.
                let file_name = path.file_name().unwrap().to_str().unwrap();
                let abs_path = src_dir.join(include_prefix).join(file_name).to_str().unwrap().replace('\\', "/");

                let parsed = wxctl_schema_compiler::build_meta::parse_schema_file(&content, abs_path).unwrap_or_else(|e| panic!("Failed to parse schema {}: {}", path.display(), e));

                // Check for duplicate resource names across directories.
                let name = parsed.schema.resource.name.clone();
                if !seen_names.insert(name.clone()) {
                    panic!("Duplicate schema name '{}' found in {}", name, path.display());
                }

                parsed_schemas.push(parsed);
            }
        }
    }

    // ── Build-time validation ──
    wxctl_schema_compiler::validation::validate_schemas(&parsed_schemas);

    // ── Load and validate linkages ──
    // rerun-if-changed is emitted unconditionally: if the file is absent now but
    // appears later, the build must still rerun to pick it up.
    println!("cargo:rerun-if-changed=src/linkages.yaml");
    let linkages_path = Path::new("src/linkages.yaml");
    let linkages: wxctl_schema_compiler::build_meta::LinkagesFile = if linkages_path.exists() {
        let content = fs::read_to_string(linkages_path).expect("Failed to read linkages.yaml");
        let parsed: wxctl_schema_compiler::build_meta::LinkagesFile = serde_norway::from_str(&content).expect("Failed to parse linkages.yaml");
        wxctl_schema_compiler::validation::validate_linkages(&parsed, &seen_names, &parsed_schemas);
        parsed
    } else {
        wxctl_schema_compiler::build_meta::LinkagesFile { bridges: Vec::new() }
    };

    // ── Topological sort (owned by the compiler) ──
    let order = wxctl_schema_compiler::codegen::tables::topo_order(&parsed_schemas, &linkages);

    // ── Emit the dependency-graph tables (byte-identical to today's output) ──
    let tables = wxctl_schema_compiler::codegen::tables::generate_tables(&order, &parsed_schemas, &linkages);
    let dest_path = Path::new(&out_dir).join("dependency_graph_generated.rs");
    fs::write(&dest_path, tables).expect("Failed to write dependency_graph_generated.rs");

    // ── Emit the static IR (schemas, per-deployment variants, descriptors) ──
    let ir = wxctl_schema_compiler::codegen::ir::generate_ir(&order, &parsed_schemas);
    let ir_dest_path = Path::new(&out_dir).join("schema_ir_generated.rs");
    fs::write(&ir_dest_path, ir).expect("Failed to write schema_ir_generated.rs");

    // ── Emit rerun-if-changed for all schema directories and build.rs ──
    println!("cargo:rerun-if-changed=build.rs");
    for &(fs_dir, _) in SCHEMA_DIRS {
        println!("cargo:rerun-if-changed={}", fs_dir);
    }
}
