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
use wxctl_core::{Config, RawResource};

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
    // Canonicalize cwd once for the defense-in-depth containment check below.
    let cwd_canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    for resource in &mut rewritten.resources {
        let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();
        // Path-traversal guard: `ref_name` is LLM-generated and gets joined into the scaffold
        // base. Reject anything that isn't a single safe path component before it can escape cwd
        // (`../x` climbs out; an absolute value REPLACES the base entirely under PathBuf::join).
        // A rejected resource becomes a manifest `failed` entry and is not materialized.
        if let Some(reason) = unsafe_ref_name_reason(&ref_name) {
            manifest.failed(format!("{}/{ref_name}", resource.kind), format!("unsafe ref_name — refusing to scaffold: {reason}"));
            continue;
        }
        // Canonical per-resource base, cwd-relative (e.g. ".wxctl-scaffold/weather").
        let rel_base = PathBuf::from(SCAFFOLD_BASE).join(&ref_name);
        // Defense-in-depth (mirrors the wxctl-mcp compose_tools guard): independently confirm the
        // resolved base stays inside cwd, resolving platform symlinks on the existing prefix — so a
        // pre-existing symlinked `.wxctl-scaffold` can't redirect writes out of cwd either.
        let resolved_base = canonicalize_prefix(&cwd.join(&rel_base));
        if !resolved_base.starts_with(&cwd_canon) {
            manifest.failed(format!("{}/{ref_name}", resource.kind), format!("scaffold base {} resolves outside cwd {}", resolved_base.display(), cwd_canon.display()));
            continue;
        }
        rewrite_resource_paths(resource.kind.as_str(), &mut resource.data, &rel_base);
        // Scaffold using the rewritten (cwd-relative) paths, resolved against cwd.
        scaffold_resource(resource.kind.as_str(), &resource.data, None, cwd, &mut manifest);
    }
    (ScaffoldOutput { manifest }, rewritten)
}

/// Pure path-rewrite half of `scaffold_config_in_cwd` — rewrite every resource's source-path
/// field(s) to their canonical in-cwd scaffold locations (`<SCAFFOLD_BASE>/<ref_name>/…`) and
/// return the clone, WITHOUT touching the filesystem. Resources whose `ref_name` is unsafe
/// (would escape cwd) are left unchanged — `scaffold_config_in_cwd` records those as manifest
/// failures and never materializes them. The MCP scaffold tool feeds this a *non-interpolated*
/// config so the config it returns to the client preserves `${env:…}` literals instead of
/// echoing resolved secrets, while still carrying the same canonical source paths.
pub fn rewrite_config_paths_in_cwd(config: &Config) -> Config {
    let mut rewritten = config.clone();
    for resource in &mut rewritten.resources {
        let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();
        if unsafe_ref_name_reason(&ref_name).is_some() {
            continue;
        }
        let rel_base = PathBuf::from(SCAFFOLD_BASE).join(&ref_name);
        rewrite_resource_paths(resource.kind.as_str(), &mut resource.data, &rel_base);
    }
    rewritten
}

/// Parse multi-document config YAML WITHOUT `${env:…}` interpolation, preserving the raw
/// literals. Mirrors `Config::from_yaml`'s document loop minus the env-expansion step (which
/// resolves — and would thereby leak — secrets). The MCP scaffold tool uses this to build the
/// config it returns to the client so `${env:…}` references round-trip untouched.
pub fn config_from_yaml_raw(content: &str) -> Result<Config> {
    use serde::Deserialize as _;
    let mut resources = Vec::new();
    for document in serde_norway::Deserializer::from_str(content) {
        let value = serde_norway::Value::deserialize(document).context("parse config document")?;
        if value.is_null() {
            continue;
        }
        let resource: RawResource = serde_norway::from_value(value).context("deserialize config resource")?;
        resources.push(resource);
    }
    Ok(Config { resources })
}

/// Reason string when `ref_name` is NOT a single safe path component and so must not be joined
/// into the in-cwd scaffold base. Rejects path separators, `..`, a leading `.`, empties, and
/// absolute paths — the shapes that let `<SCAFFOLD_BASE>/<ref_name>` escape cwd. `None` = safe.
fn unsafe_ref_name_reason(ref_name: &str) -> Option<String> {
    if ref_name.is_empty() {
        return Some("empty ref_name".to_string());
    }
    if ref_name.contains('/') || ref_name.contains('\\') {
        return Some(format!("'{ref_name}' contains a path separator"));
    }
    if ref_name.contains("..") {
        return Some(format!("'{ref_name}' contains '..'"));
    }
    if ref_name.starts_with('.') {
        return Some(format!("'{ref_name}' starts with '.'"));
    }
    if Path::new(ref_name).is_absolute() {
        return Some(format!("'{ref_name}' is an absolute path"));
    }
    // Final barrier: the only accepted shape is a single Normal path component.
    let mut comps = Path::new(ref_name).components();
    match (comps.next(), comps.next()) {
        (Some(std::path::Component::Normal(_)), None) => None,
        _ => Some(format!("'{ref_name}' is not a single path component")),
    }
}

/// Canonicalize as much of `path` as exists (resolving platform symlinks), then re-append any
/// non-existent tail lexically (collapsing `.`/`..`). Lets a security prefix-check run on a
/// not-yet-created path without forfeiting symlink resolution on the parts that do exist.
/// Mirrors the helper in the wxctl-mcp compose_tools guard (the crates can't share it — the
/// dependency edge points the other way).
fn canonicalize_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail = vec![];
    loop {
        if existing.exists() {
            break;
        }
        match existing.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                existing = existing.parent().map(PathBuf::from).unwrap_or_default();
            }
            None => break,
        }
    }
    let mut result = existing.canonicalize().unwrap_or(existing);
    for component in tail.into_iter().rev() {
        if component == "." {
            continue;
        } else if component == ".." {
            result.pop();
        } else {
            result.push(component);
        }
    }
    result
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
        "data_asset" => rebase_single_path(obj, "source_path", rel_base, "data.csv"),
        "sal_glossary" => rebase_single_path(obj, "glossary_csv", rel_base, "glossary.csv"),
        "s3_object" => rebase_single_path(obj, "path", rel_base, "object.csv"),
        _ => {}
    }
}

/// File-name tail of a path string, falling back to the whole string when there is none.
fn file_name_or(raw: &str) -> &str {
    Path::new(raw).file_name().and_then(|f| f.to_str()).unwrap_or(raw)
}

/// Rebase a single string path field under `rel_base`, preserving the incoming
/// file name (with extension) or falling back to `default_name`. No-op if absent.
fn rebase_single_path(obj: &mut serde_json::Map<String, Value>, field: &str, rel_base: &Path, default_name: &str) {
    if let Some(cur) = obj.get(field).and_then(|v| v.as_str()) {
        let name = Path::new(cur).file_name().and_then(|f| f.to_str()).filter(|n| Path::new(n).extension().is_some()).unwrap_or(default_name).to_string();
        obj.insert(field.into(), Value::String(rel_base.join(&name).to_string_lossy().replace('\\', "/")));
    }
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

/// Apply a synthesized-data file (`ref_name: { content }` YAML) back onto the config's
/// detected data needs — writing each returned `content` to the field's path
/// (OVERWRITING any placeholder, mirroring `apply_impl_file`). A `Fixture` need gets
/// file bytes; an `Embedded` need (`wml_function`/`ai_service`) gets data-synthesizing
/// source code written to `source_path` — the same generic field-path write, no
/// special-casing. Refs with no matching data need, or needs with no supplied content,
/// are skipped with a warning.
pub fn apply_data_file(config: &Config, data_file: &str) -> Result<()> {
    let content = std::fs::read_to_string(data_file).with_context(|| format!("Failed to read data file '{}'", data_file))?;
    let fixtures: HashMap<String, DataFixture> = serde_norway::from_str(&content).with_context(|| "Failed to parse data YAML")?;
    let needs = wxctl_compose_core::detect_data_needs(&config.resources);

    let mut applied = 0;
    for resource in &config.resources {
        let Some(ref_name) = resource.data.get("ref_name").and_then(|v| v.as_str()) else { continue };
        let Some(fixture) = fixtures.get(ref_name) else { continue };
        let Some(need) = needs.iter().find(|n| n.ref_name == ref_name) else {
            eprintln!("Warning: '{ref_name}' in data file has no detected data need — skipping");
            continue;
        };
        let path = match need.parent.as_deref() {
            None => resource.data.get(&need.field).and_then(|v| v.as_str()),
            Some(p) => resource.data.get(p).and_then(|o| o.get(&need.field)).and_then(|v| v.as_str()),
        };
        let Some(path) = path else { continue };
        if let Some(parent) = Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, &fixture.content).with_context(|| format!("Failed to write '{path}'"))?;
        applied += 1;
    }
    eprintln!("Applied {applied} data fixtures");
    Ok(())
}

#[derive(serde::Deserialize)]
struct DataFixture {
    content: String,
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
        "data_asset" => scaffold_data_file(data, "source_path", "csv", output_dir, config_dir, manifest),
        "sal_glossary" => scaffold_data_file(data, "glossary_csv", "csv", output_dir, config_dir, manifest),
        "s3_object" => scaffold_data_file(data, "path", "csv", output_dir, config_dir, manifest),
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

        let input_schema = resolve_stub_input_schema(&tool_dir, data);
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

/// Resolve the `input_schema` used to type the Python stub, in priority order:
/// an authored `<tool_dir>/schema.yaml` (its `input_schema`) -> the config inline
/// `input_schema` (back-compat) -> a bare `{type: object}`. Lets a config with no
/// inline schema still produce a correctly typed stub. Reading the authored file is
/// best-effort: an unreadable / malformed / `input_schema`-less `schema.yaml` falls
/// through to the config value, then bare — it never errors (mirrors the
/// never-panic scaffold contract; see Error Handling in the spec).
fn resolve_stub_input_schema(tool_dir: &Path, data: &Value) -> Value {
    if let Some(authored) = read_authored_input_schema(&tool_dir.join("schema.yaml")) {
        return authored;
    }
    data.get("input_schema").cloned().unwrap_or_else(|| serde_json::json!({"type": "object"}))
}

/// `input_schema` from an authored `schema.yaml`, or `None` when the file is absent,
/// unreadable, unparseable, or carries no object `input_schema`. Pure best-effort read
/// (no `load_schemas` dep — `wxctl-compose` does not depend on `wxctl-providers`).
fn read_authored_input_schema(schema_path: &Path) -> Option<Value> {
    if !schema_path.exists() {
        return None;
    }
    let content = std::fs::read_to_string(schema_path).ok()?;
    let doc: Value = serde_norway::from_str(&content).ok()?;
    let input = doc.get("input_schema")?;
    if input.is_object() { Some(input.clone()) } else { None }
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

/// Materialize a shape-correct placeholder fixture at a single path field (skip-if-exists).
fn scaffold_data_file(data: &Value, field: &str, shape: &str, output_dir: Option<&str>, config_dir: &Path, manifest: &mut Manifest) {
    let Some(raw) = data.get(field).and_then(|v| v.as_str()) else {
        return;
    };
    let p = target_path(raw, output_dir, config_dir);
    let shape = Path::new(raw).extension().and_then(|e| e.to_str()).unwrap_or(shape);
    write_stub(&p, stubs::data_fixture_stub(Some(shape)).as_bytes(), manifest.dry_run, manifest);
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
    fn in_cwd_rejects_unsafe_ref_names_and_writes_nothing_out_of_cwd() {
        // `../evil` — traversal via ref_name → recorded failed, nothing escapes cwd, and the
        // rejected resource's paths are left untouched (not rewritten under the scaffold base).
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: wml_function\nref_name: ../evil\nsource_path: score.py\n";
        let (out, rw) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert!(out.manifest.any_failed(), "traversal ref_name must be a scaffold failure");
        assert!(!tmp.path().parent().unwrap().join("evil").exists(), "nothing written above cwd");
        assert_eq!(rw.resources[0].data.get("source_path").and_then(|v| v.as_str()).unwrap(), "score.py", "rejected resource keeps its original path");

        // Absolute ref_name — `PathBuf::join` would REPLACE the base with it → rejected too.
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: wml_function\nref_name: /tmp/wxctl-evil\nsource_path: score.py\n";
        let (out, _rw) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert!(out.manifest.any_failed(), "absolute ref_name must be a scaffold failure");
        assert!(!std::path::Path::new("/tmp/wxctl-evil/score.py").exists(), "nothing written at the absolute path");

        // Normal name — scaffolds under the canonical base and rewrites the path.
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: wml_function\nref_name: scorer\nsource_path: score.py\n";
        let (out, rw) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert!(!out.manifest.any_failed(), "{}", out.manifest.render());
        assert_eq!(rw.resources[0].data.get("source_path").and_then(|v| v.as_str()).unwrap(), ".wxctl-scaffold/scorer/score.py");
        assert!(tmp.path().join(".wxctl-scaffold/scorer/score.py").exists());
    }

    #[test]
    fn config_from_yaml_raw_preserves_env_literals() {
        // A non-interpolating parse keeps `${env:...}` verbatim (no process-env read, no leak).
        let yaml = "kind: agent\nref_name: a\ndescription: uses ${env:WXCTL_SOME_SECRET}\n";
        let cfg = config_from_yaml_raw(yaml).unwrap();
        assert_eq!(cfg.resources[0].data.get("description").and_then(|v| v.as_str()).unwrap(), "uses ${env:WXCTL_SOME_SECRET}");
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

    use crate::test_support::lock_cwd;

    #[test]
    fn data_asset_fixture_materializes_rewrites_and_skips_second_run() {
        // First scaffold: rewrite source_path under the canonical base + write a valid CSV.
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: data_asset\nref_name: customers\nname: customers\nsource_path: raw/customers.csv\n";
        let (out, rw) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert!(!out.manifest.any_failed(), "{}", out.manifest.render());
        let field = rw.resources[0].data.get("source_path").and_then(|v| v.as_str()).unwrap();
        assert_eq!(field, ".wxctl-scaffold/customers/customers.csv");
        let file = tmp.path().join(".wxctl-scaffold/customers/customers.csv");
        let body = std::fs::read_to_string(&file).unwrap();
        assert!(body.lines().next() == Some("id,name,value") && body.lines().count() >= 2, "valid non-empty CSV with header");

        // Second scaffold run over the rewritten config: the fixture exists → skipped, not overwritten.
        std::fs::write(&file, "id,name,value\n9,custom,1\n").unwrap();
        let (out2, _rw2) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert_eq!(out2.manifest.counts(), (0, 1, 0), "second run skips the existing fixture");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "id,name,value\n9,custom,1\n", "no overwrite");
    }

    #[test]
    fn apply_data_file_writes_synthesized_bytes_over_placeholder() {
        // apply_data_file writes to the literal (cwd-relative) path field from the config, so
        // the process CWD must actually be the scaffold base dir while it runs — guard the
        // global CWD mutation with the crate's CWD_LOCK (mirrors wxctl-mcp/wxctl-providers'
        // established pattern for this exact hazard) so this doesn't race other tests.
        let _g = lock_cwd();
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: data_asset\nref_name: customers\nname: customers\nsource_path: raw/customers.csv\n";
        let (_out, rw) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        // The agent's output, keyed by ref_name; paths in `rw` are cwd-relative.
        let data_yaml = "customers:\n  content: |\n    id,name,value\n    1,synth,42\n";
        let data_path = tmp.path().join("data.yaml");
        std::fs::write(&data_path, data_yaml).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = apply_data_file(&rw, data_path.to_str().unwrap());
        std::env::set_current_dir(&prev).unwrap();
        result.unwrap();
        let written = std::fs::read_to_string(tmp.path().join(".wxctl-scaffold/customers/customers.csv")).unwrap();
        assert!(written.contains("synth,42"), "agent content applied over placeholder: {written}");
    }

    #[test]
    fn apply_data_file_writes_embedded_wml_function_source() {
        // A wml_function with no supplied dataset: scaffold writes a placeholder score.py,
        // then the data round-trip overwrites it with data-synthesizing source that carries
        // a fixed seed. Guards the global CWD mutation (see the sibling test's note).
        let _g = lock_cwd();
        let tmp = tempfile::tempdir().unwrap();
        let yaml = "kind: wml_function\nref_name: scorer\nname: scorer\nsource_path: score.py\n";
        let (out, rw) = scaffold_config_in_cwd(&cfg(yaml), tmp.path(), false);
        assert!(!out.manifest.any_failed(), "{}", out.manifest.render());
        // Detection flags the embedded need so apply_data_file targets source_path.
        let needs = wxctl_compose_core::detect_data_needs(&rw.resources);
        assert!(needs.iter().any(|n| n.kind == "wml_function" && n.delivery == wxctl_compose_core::Delivery::Embedded), "wml_function must be an Embedded need");
        // Agent embedded output: source that synthesizes records in-code with a fixed seed.
        let data_yaml = "scorer:\n  content: |\n    import random\n    random.seed(42)\n    def score(payload):\n        rows = [{\"id\": i, \"value\": random.random()} for i in range(100)]\n        return {\"predictions\": [{\"values\": [[r[\"value\"]] for r in rows]}]}\n";
        let data_path = tmp.path().join("data.yaml");
        std::fs::write(&data_path, data_yaml).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = apply_data_file(&rw, data_path.to_str().unwrap());
        std::env::set_current_dir(&prev).unwrap();
        result.unwrap();
        let written = std::fs::read_to_string(tmp.path().join(".wxctl-scaffold/scorer/score.py")).unwrap();
        assert!(written.contains("random.seed(42)"), "embedded source carries a fixed seed: {written}");
        assert!(written.contains("def score(payload):"), "embedded source preserves the entry point: {written}");
    }
}
