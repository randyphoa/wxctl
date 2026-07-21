//! Asserts, for every kind, that the generated static IR (`wxctl_schema::ir::RESOURCE_IR`)
//! serializes to the same canonical value as a fresh compiler-driven parse of the
//! same `src/schemas/**/*.yaml` files (`wxctl_schema_compiler::SchemaParser::parse_str`)
//! — the render/explain serialization surface both depend on. Pins the IR/parse
//! serialize-parity contract from earlier tasks in this phase; Phase 2 deletes the
//! owned parse path this test used to compare against.

/// Recursively sort object keys in a `serde_json::Value`.
///
/// Neutralizes the nondeterministic `HashMap` serialization of owned
/// `variants`/`deployments` (D3) on both sides: `serde_json`'s default `Map`
/// preserves insertion order, so we rebuild maps with keys sorted before comparing.
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

fn parse_all_schemas() -> Vec<wxctl_schema_compiler::definition::ResourceSchema> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/schemas");
    let mut files = Vec::new();
    collect_yaml(&root, &mut files);
    files.sort();
    files.iter().map(|p| wxctl_schema_compiler::SchemaParser::parse_str(&std::fs::read_to_string(p).unwrap()).unwrap_or_else(|e| panic!("parse {}: {e}", p.display()))).collect()
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
fn ir_matches_parse_for_all_kinds() {
    let owned = parse_all_schemas();
    for schema in &owned {
        let kind = schema.resource.kind.as_str();
        let ir = wxctl_schema::ir::RESOURCE_IR.get(kind).expect("kind present in RESOURCE_IR");
        let want = canonical(serde_json::to_value(schema).unwrap());
        let got = canonical(serde_json::to_value(ir).unwrap());
        assert_eq!(got, want, "IR/parse serialization mismatch for kind {kind}");
    }
    assert_eq!(owned.len(), wxctl_schema::ir::RESOURCE_IR.len(), "kind count parity");
}
