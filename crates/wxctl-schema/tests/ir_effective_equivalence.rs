//! AC2 (per-deployment clause): every baked per-deployment effective schema in
//! `wxctl_schema::ir::RESOURCE_IR_EFFECTIVE` serializes to the same canonical
//! value as the production overlay merge — `wxctl_schema_compiler::overlay::
//! effective_definition(base, deployment_for_key(key))` — re-run here from the
//! source YAML. This is the overlay counterpart of `ir_parse_equivalence.rs`
//! (which covers base schemas): it proves the codegen emitter round-trips the
//! effective variants, so the redesigned per-deployment schemas match the
//! pre-redesign runtime overlay merge (same `effective_definition`, now build-time).

use semver::Version;
use wxctl_schema_compiler::deployment::Deployment;
use wxctl_schema_compiler::{SchemaParser, definition::ResourceSchema, overlay::effective_definition};

/// Recursively sort object keys so `HashMap`-backed maps compare by contents, not
/// insertion order (same helper as `ir_parse_equivalence.rs`).
fn canonical(v: serde_json::Value) -> serde_json::Value {
    match v {
        serde_json::Value::Object(map) => {
            let mut entries: Vec<(String, serde_json::Value)> = map.into_iter().map(|(k, v)| (k, canonical(v))).collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            let mut sorted = serde_json::Map::with_capacity(entries.len());
            for (k, v) in entries {
                sorted.insert(k, v);
            }
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(items) => serde_json::Value::Array(items.into_iter().map(canonical).collect()),
        other => other,
    }
}

/// Verbatim port of `codegen/ir.rs::deployment_for_key` — the mapping the build
/// used to select each overlay key. Every current key is `"saas"`, `"software"`,
/// or `"software-<version-prefix>"`; pad numeric suffixes to a full triple.
fn deployment_for_key(key: &str) -> Deployment {
    if key == "saas" {
        return Deployment::Saas;
    }
    if key == "software" {
        return Deployment::Software { version: Version::new(0, 0, 0) };
    }
    let rest = key.strip_prefix("software-").unwrap_or_else(|| panic!("unrecognized deployment overlay key {key:?}"));
    if let Ok(v) = Version::parse(rest) {
        return Deployment::Software { version: v };
    }
    let parts: Vec<u64> = rest.split('.').map(|p| p.trim_end_matches(['x', 'X', '*']).parse::<u64>().unwrap_or_else(|_| panic!("non-numeric version component in overlay key {key:?}"))).collect();
    Deployment::Software { version: Version::new(parts.first().copied().unwrap_or(0), parts.get(1).copied().unwrap_or(0), parts.get(2).copied().unwrap_or(0)) }
}

/// Parse every schema YAML once; index owned `ResourceSchema` by `resource.kind`.
fn parse_by_kind() -> std::collections::HashMap<String, ResourceSchema> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/schemas");
    let mut files = Vec::new();
    collect_yaml(&root, &mut files);
    files
        .iter()
        .map(|p| {
            let s = SchemaParser::parse_str(&std::fs::read_to_string(p).unwrap()).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()));
            (s.resource.kind.clone(), s)
        })
        .collect()
}

fn collect_yaml(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            collect_yaml(&p, out);
        } else if p.extension().and_then(|e| e.to_str()) == Some("yaml") {
            out.push(p);
        }
    }
}

#[test]
fn effective_ir_matches_overlay_merge_for_every_variant() {
    let by_kind = parse_by_kind();
    let mut checked = 0usize;
    // `&(...)` binds the Copy references directly: kind: &str, key: &str, ir: &SchemaIr.
    for &(kind, key, ir) in wxctl_schema::ir::RESOURCE_IR_EFFECTIVE {
        let base = by_kind.get(kind).unwrap_or_else(|| panic!("kind {kind} in RESOURCE_IR_EFFECTIVE but not in schema YAML"));
        let merged = effective_definition(&base.resource, &deployment_for_key(key)).unwrap_or_else(|e| panic!("effective_definition({kind}, {key}): {e}"));
        let want = canonical(serde_json::to_value(ResourceSchema { resource: merged }).unwrap());
        let got = canonical(serde_json::to_value(ir).unwrap());
        assert_eq!(got, want, "effective IR mismatch for kind {kind}, deployment key {key}");
        checked += 1;
    }
    assert_eq!(checked, wxctl_schema::ir::RESOURCE_IR_EFFECTIVE.len(), "checked every effective variant");
    assert!(checked > 0, "at least one kind declares a non-empty overlay");
}
