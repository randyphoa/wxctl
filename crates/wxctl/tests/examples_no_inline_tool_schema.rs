//! Guard (spec invariant I3 / AC4): no shipped `wxctl/examples/**/config.yaml`
//! Python-binding `tool` block carries an inline `input_schema`/`output_schema`.
//! For a Python tool, `schema.yaml` in `source_path` is authoritative
//! (`ToolHandler` loads it and overwrites any inline block), so an inline schema
//! is dead weight that silently drifts. Scoped to the PUBLIC examples tree only.

use serde::de::Deserialize;
use serde_norway::Value;
use std::path::{Path, PathBuf};

/// Recursively collect every file named `config.yaml` under `dir`.
fn collect_config_yamls(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read_dir {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            collect_config_yamls(&path, out);
        } else if path.file_name().and_then(|n| n.to_str()) == Some("config.yaml") {
            out.push(path);
        }
    }
}

#[test]
fn examples_python_tools_carry_no_inline_schema() {
    // CARGO_MANIFEST_DIR = <repo>/wxctl/crates/wxctl -> ../../examples = <repo>/wxctl/examples (public tree only).
    let examples_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples");
    let examples_dir = examples_dir.canonicalize().unwrap_or_else(|e| panic!("canonicalize {}: {e}", examples_dir.display()));
    assert!(examples_dir.is_dir(), "public examples dir missing: {}", examples_dir.display());

    let mut configs = Vec::new();
    collect_config_yamls(&examples_dir, &mut configs);
    assert!(!configs.is_empty(), "no config.yaml found under {}", examples_dir.display());

    let mut violations = Vec::new();
    for config in &configs {
        let content = std::fs::read_to_string(config).unwrap_or_else(|e| panic!("read {}: {e}", config.display()));
        for document in serde_norway::Deserializer::from_str(&content) {
            let Ok(value) = Value::deserialize(document) else { continue };
            let Some(map) = value.as_mapping() else { continue };
            if map.get("kind").and_then(|v| v.as_str()) != Some("tool") {
                continue;
            }
            let is_python = map.get("binding").and_then(|b| b.as_mapping()).map(|b| b.contains_key("python")).unwrap_or(false);
            if !is_python {
                continue;
            }
            let ref_name = map.get("ref_name").and_then(|v| v.as_str()).unwrap_or("<unnamed>");
            for field in ["input_schema", "output_schema"] {
                if map.get(field).is_some() {
                    violations.push(format!("{} (tool '{}') carries inline '{}'", config.display(), ref_name, field));
                }
            }
        }
    }

    assert!(violations.is_empty(), "Python tool blocks must keep schemas in <source_path>/schema.yaml, not inline in config.yaml:\n  {}", violations.join("\n  "));
}
