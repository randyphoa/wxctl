//! `compose scaffold` core — materialize every source file a config references and
//! return the run `Manifest`. CLI/MCP wrappers own config reading, manifest
//! rendering/printing, and the non-zero-exit-on-failure decision.

pub mod manifest;
mod stubs;
mod typemap;

use anyhow::{Context, Result};
use manifest::Manifest;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use wxctl_core::Config;

/// Result of a scaffold run: the manifest (every created/skipped/failed entry).
pub struct ScaffoldOutput {
    pub manifest: Manifest,
}

/// Scaffold every referenced file for a parsed config. `config_dir` is the directory
/// config-relative paths resolve against (mirror of `resolve_file_paths`). `dry_run`
/// records intent without touching the FS. Never bails on per-resource failure — the
/// caller inspects `manifest.any_failed()`.
pub fn scaffold_config(config: &Config, output_dir: Option<&str>, config_dir: &Path, dry_run: bool) -> ScaffoldOutput {
    let mut manifest = Manifest::new(dry_run);
    for resource in &config.resources {
        scaffold_resource(resource.kind.as_str(), &resource.data, output_dir, config_dir, &mut manifest);
    }
    ScaffoldOutput { manifest }
}

/// Canonical in-cwd scaffold base directory name. Hidden, namespaced to wxctl,
/// gitignored, and unambiguously inside cwd so the engine's path-traversal guard
/// (`wxctl-providers::util::validate_path`) passes naturally at plan/apply.
pub const SCAFFOLD_BASE: &str = ".wxctl-scaffold";

/// Scaffold every source-bearing resource into a canonical per-resource dir under
/// `<cwd>/.wxctl-scaffold/<ref_name>/`, AND return a clone of `config` whose path
/// fields (`source_path`/`spec_path`/`flow_path`/kb document paths) are rewritten to
/// those canonical cwd-relative locations. Always rewrites unconditionally — the
/// incoming path value is ignored (the tool guarantees scaffold-write/path consistency).
/// `cwd` is the directory the canonical base hangs off and config-relative paths resolve
/// against. The existing `scaffold_config` (CLI / explicit-output_dir) path is unchanged.
pub fn scaffold_config_in_cwd(config: &Config, cwd: &Path, dry_run: bool) -> (ScaffoldOutput, Config) {
    let mut rewritten = config.clone();
    let mut manifest = Manifest::new(dry_run);
    for resource in &mut rewritten.resources {
        let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();
        // Canonical per-resource base, cwd-relative (e.g. ".wxctl-scaffold/weather").
        let rel_base = PathBuf::from(SCAFFOLD_BASE).join(&ref_name);
        rewrite_resource_paths(resource.kind.as_str(), &mut resource.data, &rel_base);
        // Scaffold using the rewritten (cwd-relative) paths, resolved against cwd.
        scaffold_resource(resource.kind.as_str(), &resource.data, None, cwd, &mut manifest);
    }
    (ScaffoldOutput { manifest }, rewritten)
}

/// Rewrite a resource's source-path field(s) to point under `rel_base`
/// (cwd-relative). Mirrors the per-kind path fields `scaffold_resource` consumes.
/// No-op for kinds with no source-path field, or resources missing the field
/// (those become manifest `failed` entries downstream, as today).
///
/// Emitted values are normalized to forward slashes (`to_string_lossy().replace('\\', "/")`):
/// these land in portable config YAML, so they must not carry Windows `\` separators.
fn rewrite_resource_paths(kind: &str, data: &mut Value, rel_base: &Path) {
    let Some(obj) = data.as_object_mut() else { return };
    match kind {
        "tool" => {
            let binding = obj.get("binding").cloned();
            if binding.as_ref().and_then(|b| b.get("python")).is_some() {
                // Python: source_path is the tool dir.
                obj.insert("source_path".into(), Value::String(rel_base.to_string_lossy().replace('\\', "/")));
            } else if binding.as_ref().and_then(|b| b.get("openapi")).is_some() {
                // OpenAPI: spec_path is a single file under the dir.
                obj.insert("spec_path".into(), Value::String(rel_base.join("openapi.yaml").to_string_lossy().replace('\\', "/")));
            } else if binding.as_ref().and_then(|b| b.get("flow")).is_some() {
                // Flow: the flow file is named by source_path (validate-gated) or flow_path.
                // Only rewrite a field that is already present, preserving the file name +
                // extension (json vs yaml drives the stub format in scaffold_tool).
                if let Some(name) = obj.get("source_path").and_then(|v| v.as_str()).map(file_name_or).map(str::to_owned) {
                    obj.insert("source_path".into(), Value::String(rel_base.join(&name).to_string_lossy().replace('\\', "/")));
                } else if let Some(name) = obj.get("flow_path").and_then(|v| v.as_str()).map(file_name_or).map(str::to_owned) {
                    obj.insert("flow_path".into(), Value::String(rel_base.join(&name).to_string_lossy().replace('\\', "/")));
                }
            }
        }
        "knowledge_base" => {
            // Each document's basename is rebased under rel_base; object docs keep other keys.
            if let Some(Value::Array(docs)) = obj.get_mut("documents") {
                for doc in docs.iter_mut() {
                    if let Some(s) = doc.as_str() {
                        let name = file_name_or(s).to_owned();
                        *doc = Value::String(rel_base.join(&name).to_string_lossy().replace('\\', "/"));
                    } else if let Some(o) = doc.as_object_mut()
                        && let Some(name) = o.get("path").and_then(|v| v.as_str()).map(file_name_or).map(str::to_owned)
                    {
                        o.insert("path".into(), Value::String(rel_base.join(&name).to_string_lossy().replace('\\', "/")));
                    }
                }
            }
        }
        "wml_function" | "ai_service" => {
            // source_path is read as a file by the handler; rewrite to <dir>/score.py
            // when the incoming value has no extension, else preserve its file name.
            if let Some(cur) = obj.get("source_path").and_then(|v| v.as_str()) {
                let name = if Path::new(cur).extension().is_none() { "score.py".to_string() } else { file_name_or(cur).to_owned() };
                obj.insert("source_path".into(), Value::String(rel_base.join(&name).to_string_lossy().replace('\\', "/")));
            }
        }
        "toolkit" if obj.contains_key("server_path") => {
            // server_path is the server dir.
            obj.insert("server_path".into(), Value::String(rel_base.to_string_lossy().replace('\\', "/")));
        }
        _ => {}
    }
}

/// File-name tail of a path string, falling back to the whole string when there is none.
fn file_name_or(raw: &str) -> &str {
    Path::new(raw).file_name().and_then(|f| f.to_str()).unwrap_or(raw)
}

/// Serialize a `Config` back to multi-document YAML (one `---`-separated doc per
/// resource), the shape `Config::from_yaml` and every downstream `-f` consumer expect.
/// Serializing `Config` whole would emit a `resources:` wrapper instead.
pub fn config_to_multidoc_yaml(config: &Config) -> Result<String> {
    let mut out = String::new();
    for (i, resource) in config.resources.iter().enumerate() {
        if i > 0 {
            out.push_str("---\n");
        }
        out.push_str(&serde_norway::to_string(resource).with_context(|| format!("serialize resource #{}", i + 1))?);
    }
    Ok(out)
}

/// Apply an implementations.yaml back onto the scaffolded tool sources. Moved
/// verbatim from the bin crate (returns Result, writes files, logs to stderr — the
/// CLI keeps the eprintln progress; the MCP layer does not expose this round-trip).
pub fn apply_impl_file(config: &Config, impl_file: &str) -> Result<()> {
    let content = std::fs::read_to_string(impl_file).with_context(|| format!("Failed to read implementations file '{}'", impl_file))?;
    let implementations: HashMap<String, ToolImplementation> = serde_norway::from_str(&content).with_context(|| "Failed to parse implementations YAML")?;

    let mut tool_map: HashMap<String, (String, String)> = HashMap::new();
    for resource in &config.resources {
        if resource.kind != "tool" {
            continue;
        }
        if let (Some(ref_name), Some(source_path)) = (resource.data.get("ref_name").and_then(|v| v.as_str()), resource.data.get("source_path").and_then(|v| v.as_str())) {
            let function_spec = resource.data.get("binding").and_then(|b| b.get("python")).and_then(|p| p.get("function")).and_then(|v| v.as_str()).unwrap_or("main:main");
            tool_map.insert(ref_name.to_string(), (source_path.to_string(), function_spec.to_string()));
        }
    }

    let mut applied = 0;
    for (name, implementation) in &implementations {
        let Some((source_path, function_spec)) = tool_map.get(name) else {
            eprintln!("Warning: '{}' in implementations has no matching tool in config — skipping", name);
            continue;
        };

        let tool_dir = Path::new(source_path);
        let module = function_spec.split(':').next().unwrap_or("main");
        let py_path = tool_dir.join(format!("{}.py", module));

        std::fs::write(&py_path, &implementation.code).with_context(|| format!("Failed to write '{}'", py_path.display()))?;

        let req_path = tool_dir.join("requirements.txt");
        let req_content = if implementation.requirements.is_empty() { stubs::tool_requirements().to_string() } else { implementation.requirements.join("\n") + "\n" };
        std::fs::write(&req_path, req_content)?;

        eprintln!("Applied implementation for '{}'", name);
        applied += 1;
    }

    for ref_name in tool_map.keys() {
        if !implementations.contains_key(ref_name) {
            eprintln!("Warning: tool '{}' has no implementation in '{}' — stub retained", ref_name, impl_file);
        }
    }

    eprintln!("\nApplied {} implementations", applied);
    Ok(())
}

#[derive(serde::Deserialize)]
struct ToolImplementation {
    code: String,
    #[serde(default)]
    requirements: Vec<String>,
}

/// Resolve a config-relative path against the config dir (mirror of
/// resolve_file_paths' single-value branch), unless an output_dir override is
/// in effect (then the path's file-name tail is rebased under output_dir).
fn target_path(raw: &str, output_dir: Option<&str>, config_dir: &Path) -> PathBuf {
    if let Some(base) = output_dir {
        let tail = Path::new(raw).file_name().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(raw));
        return Path::new(base).join(tail);
    }
    let p = Path::new(raw);
    if p.is_absolute() { p.to_path_buf() } else { config_dir.join(p) }
}

/// Dispatch one resource to the matching stub writer(s). Each writer records
/// its own manifest entries and never panics; errors become Failed entries.
fn scaffold_resource(kind: &str, data: &Value, output_dir: Option<&str>, config_dir: &Path, manifest: &mut Manifest) {
    match kind {
        "tool" => scaffold_tool(data, output_dir, config_dir, manifest),
        "knowledge_base" => scaffold_knowledge_base(data, output_dir, config_dir, manifest),
        "wml_function" | "ai_service" => scaffold_wml_source(data, output_dir, config_dir, manifest),
        "toolkit" => scaffold_toolkit(data, output_dir, config_dir, manifest),
        _ => {}
    }
}

/// Write content to `path` unless it already exists; record the outcome.
/// Under dry-run, records intent without touching the FS. Parent dirs created.
fn write_stub(path: &Path, content: &[u8], dry_run: bool, manifest: &mut Manifest) {
    let display = path.display().to_string();
    if path.exists() {
        manifest.skipped(display);
        return;
    }
    if dry_run {
        manifest.created(display);
        return;
    }
    if let Some(parent) = path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        manifest.failed(display, format!("mkdir {}: {e}", parent.display()));
        return;
    }
    match std::fs::write(path, content) {
        Ok(()) => manifest.created(display),
        Err(e) => manifest.failed(display, e.to_string()),
    }
}

fn scaffold_tool(data: &Value, output_dir: Option<&str>, config_dir: &Path, manifest: &mut Manifest) {
    let binding = data.get("binding");
    let ref_name = data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("<unnamed>");

    // Python binding: source_path is a directory holding schema.yaml + <module>.py + requirements.txt.
    if binding.and_then(|b| b.get("python")).is_some() {
        let Some(source_path) = data.get("source_path").and_then(|v| v.as_str()) else {
            manifest.failed(format!("tool/{ref_name}"), "python binding without source_path");
            return;
        };
        let tool_dir = target_path(source_path, output_dir, config_dir);

        let func_spec = binding.and_then(|b| b.get("python")).and_then(|p| p.get("function")).and_then(|v| v.as_str()).unwrap_or("main:main");
        let module = func_spec.split(':').next().unwrap_or("main");
        let func = func_spec.split(':').nth(1).unwrap_or("main");

        let input_schema = data.get("input_schema").cloned().unwrap_or_else(|| serde_json::json!({"type": "object"}));
        let output_schema = data.get("output_schema");

        match stubs::tool_schema_yaml(&input_schema, output_schema) {
            Ok(yaml) => write_stub(&tool_dir.join("schema.yaml"), yaml.as_bytes(), manifest.dry_run, manifest),
            Err(e) => manifest.failed(tool_dir.join("schema.yaml").display().to_string(), e.to_string()),
        }
        write_stub(&tool_dir.join(format!("{module}.py")), stubs::python_tool_stub(func, &input_schema).as_bytes(), manifest.dry_run, manifest);
        write_stub(&tool_dir.join("requirements.txt"), stubs::tool_requirements().as_bytes(), manifest.dry_run, manifest);
        return;
    }

    // OpenAPI binding: spec_path is a single OAS3 file.
    if binding.and_then(|b| b.get("openapi")).is_some() {
        if let Some(spec_path) = data.get("spec_path").and_then(|v| v.as_str()) {
            let p = target_path(spec_path, output_dir, config_dir);
            write_stub(&p, stubs::openapi_spec_stub().as_bytes(), manifest.dry_run, manifest);
        } else {
            manifest.failed(format!("tool/{ref_name}"), "openapi binding without spec_path");
        }
        return;
    }

    // Flow binding: the flow file is named by source_path (validate-gated) or
    // flow_path (additive alias). Write whichever is set; prefer source_path.
    if binding.and_then(|b| b.get("flow")).is_some() {
        let flow_field = data.get("source_path").and_then(|v| v.as_str()).or_else(|| data.get("flow_path").and_then(|v| v.as_str()));
        let Some(flow_ref) = flow_field else {
            // Inlined flow model under binding.flow.model — nothing to scaffold.
            return;
        };
        let p = target_path(flow_ref, output_dir, config_dir);
        let flow_name = Path::new(flow_ref).file_stem().and_then(|s| s.to_str()).unwrap_or("scaffolded_flow");
        let is_json = p.extension().and_then(|e| e.to_str()).map(|e| e.eq_ignore_ascii_case("json")).unwrap_or(false);
        if is_json {
            write_stub(&p, stubs::flow_doc_stub(flow_name).as_bytes(), manifest.dry_run, manifest);
        } else {
            match stubs::flow_doc_stub_yaml(flow_name) {
                Ok(y) => write_stub(&p, y.as_bytes(), manifest.dry_run, manifest),
                Err(e) => manifest.failed(p.display().to_string(), e.to_string()),
            }
        }
    }

    // No recognized binding → nothing to scaffold for this tool.
}

fn scaffold_knowledge_base(data: &Value, output_dir: Option<&str>, config_dir: &Path, manifest: &mut Manifest) {
    let Some(docs) = data.get("documents").and_then(|v| v.as_array()) else {
        return;
    };
    for doc in docs {
        let raw = if let Some(obj) = doc.as_object() { obj.get("path").and_then(|v| v.as_str()) } else { doc.as_str() };
        let Some(raw) = raw else { continue };
        let p = target_path(raw, output_dir, config_dir);
        let filename = Path::new(raw).file_name().and_then(|f| f.to_str()).unwrap_or("document.txt");
        write_stub(&p, stubs::kb_document_stub(filename).as_bytes(), manifest.dry_run, manifest);
    }
}

fn scaffold_wml_source(data: &Value, output_dir: Option<&str>, config_dir: &Path, manifest: &mut Manifest) {
    let Some(source_path) = data.get("source_path").and_then(|v| v.as_str()) else {
        return;
    };
    // hash_and_tag_source reads source_path as a file; write the score-stub there.
    // If source_path looks like a directory (no extension), write the stub as
    // <dir>/score.py and also leave the dir — but the handler reads the path
    // itself, so a config that names a dir would fail validate regardless. We
    // honor the literal path: a file path → write the file.
    let p = target_path(source_path, output_dir, config_dir);
    let target = if p.extension().is_none() { p.join("score.py") } else { p };
    write_stub(&target, stubs::wml_score_stub().as_bytes(), manifest.dry_run, manifest);
}

fn scaffold_toolkit(data: &Value, output_dir: Option<&str>, config_dir: &Path, manifest: &mut Manifest) {
    let Some(server_path) = data.get("server_path").and_then(|v| v.as_str()) else {
        return;
    };
    let server_dir = target_path(server_path, output_dir, config_dir);
    write_stub(&server_dir.join("server.py"), stubs::fastmcp_server_stub().as_bytes(), manifest.dry_run, manifest);
    write_stub(&server_dir.join("requirements.txt"), stubs::fastmcp_requirements().as_bytes(), manifest.dry_run, manifest);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cfg(yaml: &str) -> Config {
        Config::from_yaml(yaml).unwrap()
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn scaffold_dispatch_writes_expected_files_per_kind() {
        // Each case drives one scaffold_* dispatcher with a representative config and
        // verifies the materialized files + manifest (created, skipped, failed) counts.
        // `dispatch` runs the right scaffolder; `verify` asserts on the temp dir.
        type Dispatch = fn(&serde_json::Value, &std::path::Path, &mut Manifest);
        type Verify = fn(&std::path::Path);
        let cases: &[(&str, serde_json::Value, (usize, usize, usize), Dispatch, Verify)] = &[
            (
                // python tool → dir with a TYPED stub derived from input_schema (not a generic params stub)
                "python_tool_typed_stub",
                json!({ "kind": "tool", "ref_name": "weather", "source_path": "weather", "input_schema": {"type": "object", "properties": {"city": {"type": "string"}, "days": {"type": "integer"}}, "required": ["city", "days"]}, "binding": {"python": {"function": "weather:main"}} }),
                (3, 0, 0),
                |d, p, m| scaffold_tool(d, None, p, m),
                |p| {
                    let dir = p.join("weather");
                    assert!(dir.join("schema.yaml").exists());
                    assert!(dir.join("requirements.txt").exists());
                    let py = std::fs::read_to_string(dir.join("weather.py")).unwrap();
                    assert!(py.contains("def main(city: str, days: int) -> dict:"), "got: {py}");
                    assert!(!py.contains("def main(params)"));
                },
            ),
            (
                // knowledge_base → one file per document, with seeded heading content
                "knowledge_base_per_document",
                json!({"kind": "knowledge_base", "ref_name": "kb", "documents": [{"path": "docs/policy.md"}, {"path": "docs/faq.txt"}]}),
                (2, 0, 0),
                |d, p, m| scaffold_knowledge_base(d, None, p, m),
                |p| {
                    assert!(p.join("docs/policy.md").exists());
                    assert!(p.join("docs/faq.txt").exists());
                    assert!(std::fs::read_to_string(p.join("docs/policy.md")).unwrap().starts_with("# Policy"));
                },
            ),
            (
                // wml_function → score.py with a score(payload) entry point
                "wml_function_score_file",
                json!({"kind": "wml_function", "ref_name": "f", "source_path": "score.py"}),
                (1, 0, 0),
                |d, p, m| scaffold_wml_source(d, None, p, m),
                |p| assert!(std::fs::read_to_string(p.join("score.py")).unwrap().contains("def score(payload):")),
            ),
            (
                // openapi-bound tool → a valid OpenAPI 3.0.3 spec file
                "openapi_tool_spec_file",
                json!({"kind": "tool", "ref_name": "api", "spec_path": "openapi.yaml", "binding": {"openapi": {"tools": ["*"]}}}),
                (1, 0, 0),
                |d, p, m| scaffold_tool(d, None, p, m),
                |p| {
                    let v: serde_json::Value = serde_norway::from_str(&std::fs::read_to_string(p.join("openapi.yaml")).unwrap()).unwrap();
                    assert_eq!(v.get("openapi").and_then(|x| x.as_str()), Some("3.0.3"));
                },
            ),
            (
                // flow-bound tool → a loadable flow doc named after the ref
                "flow_tool_loadable_doc",
                json!({"kind": "tool", "ref_name": "fl", "source_path": "myflow.yaml", "binding": {"flow": {}}}),
                (1, 0, 0),
                |d, p, m| scaffold_tool(d, None, p, m),
                |p| {
                    let v: serde_json::Value = serde_norway::from_str(&std::fs::read_to_string(p.join("myflow.yaml")).unwrap()).unwrap();
                    assert_eq!(v.pointer("/spec/name").and_then(|x| x.as_str()), Some("myflow"));
                    assert!(v.pointer("/spec/description").is_some());
                },
            ),
            (
                // toolkit → a fastmcp server.py + requirements.txt
                "toolkit_fastmcp_server",
                json!({"kind": "toolkit", "ref_name": "tk", "server_path": "servers/echo"}),
                (2, 0, 0),
                |d, p, m| scaffold_toolkit(d, None, p, m),
                |p| {
                    assert!(std::fs::read_to_string(p.join("servers/echo/server.py")).unwrap().contains("def echo(text: str) -> str:"));
                    assert!(p.join("servers/echo/requirements.txt").exists());
                },
            ),
        ];
        for (label, data, counts, dispatch, verify) in cases {
            let tmp = tempfile::tempdir().unwrap();
            let mut m = Manifest::new(false);
            dispatch(data, tmp.path(), &mut m);
            verify(tmp.path());
            assert_eq!(m.counts(), *counts, "manifest counts for {label}");
        }
    }

    #[test]
    fn manifest_dry_run_skip_and_failed_entry() {
        let wml = json!({"kind": "wml_function", "ref_name": "f", "source_path": "score.py"});

        // Dry-run: reports created but writes nothing to disk.
        let tmp = tempfile::tempdir().unwrap();
        let mut m = Manifest::new(true);
        scaffold_wml_source(&wml, None, tmp.path(), &mut m);
        assert!(!tmp.path().join("score.py").exists(), "dry-run must not write");
        assert_eq!(m.counts(), (1, 0, 0));

        // Skip-if-exists: a pre-existing target is left untouched and counted as skipped.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("score.py"), "existing\n").unwrap();
        let mut m = Manifest::new(false);
        scaffold_wml_source(&wml, None, tmp.path(), &mut m);
        assert_eq!(std::fs::read_to_string(tmp.path().join("score.py")).unwrap(), "existing\n");
        assert_eq!(m.counts(), (0, 1, 0));

        // Python binding with no source_path → a failed manifest entry (cannot materialize).
        let tmp = tempfile::tempdir().unwrap();
        let mut m = Manifest::new(false);
        scaffold_tool(&json!({"kind": "tool", "ref_name": "bad", "binding": {"python": {"function": "x:main"}}}), None, tmp.path(), &mut m);
        assert!(m.any_failed());
    }

    #[test]
    fn apply_implementations_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let tool_dir = tmp.path().join("resources").join("tool").join("add");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(tool_dir.join("add.py"), "def main():\n    return {}\n").unwrap();
        std::fs::write(tool_dir.join("requirements.txt"), "").unwrap();
        let config_yaml = format!("kind: tool\nref_name: add\nsource_path: {}\nbinding:\n  python:\n    function: add:main\n", tool_dir.display());
        let config = cfg(&config_yaml);
        let impl_yaml = "add:\n  code: |\n    def main(a: int, b: int) -> dict:\n        return {\"result\": a + b}\n  requirements:\n    - numpy\n";
        let impl_path = tmp.path().join("implementations.yaml");
        std::fs::write(&impl_path, impl_yaml).unwrap();
        apply_impl_file(&config, impl_path.to_str().unwrap()).unwrap();
        assert!(std::fs::read_to_string(tool_dir.join("add.py")).unwrap().contains("result"));
        assert!(std::fs::read_to_string(tool_dir.join("requirements.txt")).unwrap().contains("numpy"));
    }

    #[test]
    fn in_cwd_rewrites_python_source_path_unconditionally_and_writes_stubs() {
        // Relative incoming path → rewritten to the canonical cwd-relative dir, with stubs written.
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: tool\nref_name: weather\nsource_path: anywhere/old\ninput_schema:\n  type: object\nbinding:\n  python:\n    function: weather:main\n";
        let (out, rewritten) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert!(!out.manifest.any_failed(), "{}", out.manifest.render());
        assert_eq!(rewritten.resources[0].data.get("source_path").and_then(|v| v.as_str()).unwrap(), ".wxctl-scaffold/weather", "source_path rewritten to canonical cwd-relative dir");
        let dir = tmp.path().join(".wxctl-scaffold/weather");
        assert!(dir.join("schema.yaml").exists());
        assert!(dir.join("weather.py").exists());
        assert!(dir.join("requirements.txt").exists());

        // Unconditional: an absolute out-of-cwd incoming value is replaced too (not preserved).
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: tool\nref_name: t\nsource_path: /etc/evil\ninput_schema:\n  type: object\nbinding:\n  python:\n    function: t:main\n";
        let (_out, rewritten) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert_eq!(rewritten.resources[0].data.get("source_path").and_then(|v| v.as_str()).unwrap(), ".wxctl-scaffold/t");
    }

    #[test]
    fn in_cwd_no_source_resources_returns_input_paths_untouched() {
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: agent\nref_name: a\ndescription: hi\n";
        let (out, rewritten) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert_eq!(out.manifest.counts(), (0, 0, 0));
        // Agent has no source-path field; data is structurally unchanged.
        assert!(rewritten.resources[0].data.get("source_path").is_none());
    }

    #[test]
    fn in_cwd_rewrites_kb_documents_and_openapi_and_toolkit() {
        let tmp = tempfile::tempdir().unwrap();
        let kb = "kind: knowledge_base\nref_name: kb\ndocuments:\n  - path: old/policy.md\n";
        let (_o, rw) = scaffold_config_in_cwd(&cfg(kb), tmp.path(), false);
        let p = rw.resources[0].data.pointer("/documents/0/path").and_then(|v| v.as_str()).unwrap();
        assert_eq!(p, ".wxctl-scaffold/kb/policy.md");
        assert!(tmp.path().join(".wxctl-scaffold/kb/policy.md").exists());

        let api = "kind: tool\nref_name: api\nspec_path: old.yaml\nbinding:\n  openapi:\n    tools: ['*']\n";
        let (_o, rw) = scaffold_config_in_cwd(&cfg(api), tmp.path(), false);
        assert_eq!(rw.resources[0].data.get("spec_path").and_then(|v| v.as_str()).unwrap(), ".wxctl-scaffold/api/openapi.yaml");
        assert!(tmp.path().join(".wxctl-scaffold/api/openapi.yaml").exists());

        let tk = "kind: toolkit\nref_name: tk\nserver_path: old/echo\n";
        let (_o, rw) = scaffold_config_in_cwd(&cfg(tk), tmp.path(), false);
        assert_eq!(rw.resources[0].data.get("server_path").and_then(|v| v.as_str()).unwrap(), ".wxctl-scaffold/tk");
        assert!(tmp.path().join(".wxctl-scaffold/tk/server.py").exists());
    }

    #[test]
    fn config_to_multidoc_yaml_round_trips() {
        let yaml = "kind: agent\nref_name: a\ndescription: hi\n---\nkind: tool\nref_name: t\nsource_path: x\nbinding:\n  python:\n    function: t:main\n";
        let config = cfg(yaml);
        let serialized = config_to_multidoc_yaml(&config).unwrap();
        let reparsed = Config::from_yaml(&serialized).unwrap();
        assert_eq!(reparsed.resources.len(), 2);
        assert_eq!(reparsed.resources[1].kind, "tool");
        assert_eq!(reparsed.resources[1].data.get("source_path").and_then(|v| v.as_str()), Some("x"));
    }
}
