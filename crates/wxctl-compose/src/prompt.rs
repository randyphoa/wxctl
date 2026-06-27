//! `compose prompt` native cores — implementation-prompt assembly + the FS scans that
//! feed the prompt layer (existing-resources discovery, tool-stub discovery, config-derived
//! tool descriptions). The pure config/test prompt assembly lives in `wxctl-compose-core`.

use anyhow::{Context, Result, bail};
use std::collections::{HashMap, HashSet};
use std::path::Path;

/// Implementation prompt (Pass 4). `original_input` is the use-case context (may be
/// empty — the wrapper emits the missing-context warning). `descriptions` joins tool
/// `ref_name` → config `description` (from `-f config.yaml`); empty map = none.
pub fn assemble_implementation_prompt(scaffold_dir: &str, original_input: &str, descriptions: &HashMap<String, String>) -> Result<String> {
    let template = wxctl_compose_core::templates::IMPLEMENTATION;
    let tool_stubs = discover_tool_stubs(scaffold_dir, descriptions)?;
    let body = wxctl_compose_core::extract_prompt_body(template);
    Ok(body.replace("<TOOL_STUBS>", &tool_stubs).replace("<ORIGINAL_INPUT>", original_input).replace("<ORCHESTRATE_VERSION>", wxctl_compose_core::templates::ORCHESTRATE_VERSION))
}

/// FS scan for existing knowledge-base files under `resources_dir`. Returns the
/// formatted file-list entries (`- ./<relpath>`), sorted, or an empty `Vec` when none
/// exist. The CLI-only FS half of the existing-resources contract — the pure
/// `render_existing_resources` turns this list into the prompt block (single source of
/// the block format, wasm-safe).
pub fn discover_existing_resources(resources_dir: Option<&str>) -> Result<Vec<String>> {
    let Some(dir) = resources_dir else {
        return Ok(Vec::new());
    };
    let base = Path::new(dir);
    let kb_dirs = [base.join("knowledge_base"), base.join("common").join("knowledge_base")];

    let mut files: Vec<String> = Vec::new();
    for kb_dir in &kb_dirs {
        if !kb_dir.is_dir() {
            continue;
        }
        for entry in std::fs::read_dir(kb_dir).with_context(|| format!("Failed to read '{}'", kb_dir.display()))? {
            let entry = entry?;
            if entry.file_type()?.is_file() {
                let rel = kb_dir.strip_prefix(base).unwrap_or(kb_dir);
                let rel_path = rel.join(entry.file_name());
                files.push(format!("- ./{}", rel_path.display()));
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Extract tool `ref_name` → `description` mappings from a multi-doc config YAML.
/// Only `kind: tool` documents with both `ref_name` and `description` are included.
pub fn tool_descriptions_from_config(config_path: &str) -> Result<HashMap<String, String>> {
    use serde::Deserialize;
    let yaml = std::fs::read_to_string(config_path).with_context(|| format!("Failed to read config '{}'", config_path))?;
    let mut map = HashMap::new();
    for doc in serde_norway::Deserializer::from_str(&yaml) {
        let Ok(value) = serde_norway::Value::deserialize(doc) else { continue };
        if value.get("kind").and_then(|v| v.as_str()) != Some("tool") {
            continue;
        }
        let (Some(ref_name), Some(desc)) = (value.get("ref_name").and_then(|v| v.as_str()), value.get("description").and_then(|v| v.as_str())) else { continue };
        map.insert(ref_name.to_string(), desc.trim().to_string());
    }
    Ok(map)
}

fn discover_tool_stubs(scaffold_dir: &str, descriptions: &HashMap<String, String>) -> Result<String> {
    let dir = Path::new(scaffold_dir);
    if !dir.is_dir() {
        bail!("Scaffold directory '{}' does not exist", scaffold_dir);
    }

    let mut entries = read_subdirs(dir)?;

    // scaffold_dir is typically resources/tool, so common is resources/common/tool
    if let Some(parent) = dir.parent() {
        let common_tool_dir = parent.join("common").join("tool");
        if common_tool_dir.is_dir() {
            let project_names: HashSet<_> = entries.iter().map(|e| e.file_name()).collect();
            let common_entries = read_subdirs(&common_tool_dir)?.into_iter().filter(|e| !project_names.contains(&e.file_name())).collect::<Vec<_>>();
            entries.extend(common_entries);
        }
    }

    entries.sort_by_key(|e| e.file_name());

    let mut sections = Vec::new();

    for entry in entries {
        let tool_name = entry.file_name().to_string_lossy().to_string();
        let tool_path = entry.path();

        let schema_path = tool_path.join("schema.yaml");
        let schema_content = if schema_path.exists() { std::fs::read_to_string(&schema_path).with_context(|| format!("Failed to read '{}'", schema_path.display()))? } else { "(no schema)".to_string() };

        let py_files: Vec<_> = std::fs::read_dir(&tool_path)?.filter_map(|e| e.ok()).filter(|e| e.path().extension().map(|ext| ext == "py").unwrap_or(false)).collect();

        let stub_content = if let Some(py_file) = py_files.first() { std::fs::read_to_string(py_file.path()).with_context(|| format!("Failed to read '{}'", py_file.path().display()))? } else { "(no stub)".to_string() };

        let py_filename = py_files.first().map(|f| f.file_name().to_string_lossy().to_string()).unwrap_or_else(|| format!("{}.py", tool_name));

        // Use description from -f config.yaml if provided; fall back to schema.yaml field
        let description = descriptions.get(&tool_name).cloned().or_else(|| if let Ok(parsed) = serde_norway::from_str::<serde_json::Value>(&schema_content) { parsed.get("description").and_then(|v| v.as_str()).map(|s| s.to_string()) } else { None });
        let desc_line = description.map(|d| format!("Description: {}\n\n", d.trim())).unwrap_or_default();

        sections.push(format!("## Tool: {}\n\n{}schema.yaml:\n{}\n\nCurrent stub ({}):\n{}", tool_name, desc_line, indent_block(&schema_content, "  "), py_filename, indent_block(&stub_content, "  "),));
    }

    if sections.is_empty() {
        bail!("No tool directories found in '{}'", scaffold_dir);
    }

    Ok(sections.join("\n\n"))
}

fn read_subdirs(dir: &Path) -> Result<Vec<std::fs::DirEntry>> {
    Ok(std::fs::read_dir(dir)?.filter_map(|e| e.ok()).filter(|e| e.path().is_dir()).collect())
}

fn indent_block(text: &str, prefix: &str) -> String {
    text.lines().map(|line| if line.is_empty() { String::new() } else { format!("{}{}", prefix, line) }).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_compose_core::render_existing_resources;

    #[test]
    fn test_pass4_prompt_substitutes_version_and_descriptions() {
        let tmp = tempfile::tempdir().unwrap();
        let tool_dir = tmp.path().join("get_weather");
        std::fs::create_dir_all(&tool_dir).unwrap();
        std::fs::write(tool_dir.join("schema.yaml"), "input_schema:\n  type: object\n").unwrap();
        std::fs::write(tool_dir.join("get_weather.py"), "def main(city: str) -> dict:\n    return {}\n").unwrap();
        let mut descriptions = HashMap::new();
        descriptions.insert("get_weather".to_string(), "Fetch the weather for a city".to_string());
        let prompt = assemble_implementation_prompt(tmp.path().to_str().unwrap(), "look up weather", &descriptions).unwrap();
        assert!(prompt.contains("## Tool: get_weather"));
        assert!(prompt.contains("Fetch the weather for a city"));
        assert!(prompt.contains(wxctl_compose_core::templates::ORCHESTRATE_VERSION));
        assert!(!prompt.contains("<TOOL_STUBS>"));
        assert!(!prompt.contains("<ORCHESTRATE_VERSION>"));
    }

    #[test]
    fn test_tool_descriptions_from_config() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = tmp.path().join("config.yaml");
        std::fs::write(&cfg, "kind: tool\nref_name: get_weather\ndescription: Fetch weather\n---\nkind: agent\nref_name: a\n").unwrap();
        let map = tool_descriptions_from_config(cfg.to_str().unwrap()).unwrap();
        assert_eq!(map.get("get_weather").map(|s| s.as_str()), Some("Fetch weather"));
        assert!(!map.contains_key("a"));
    }

    #[test]
    fn test_discover_existing_resources() {
        // None → empty (no resources dir to scan).
        assert!(discover_existing_resources(None).unwrap().is_empty());

        // A project-level knowledge_base/ dir → one sorted entry per doc, rendered into a block.
        let tmp = tempfile::tempdir().unwrap();
        let kb_dir = tmp.path().join("knowledge_base");
        std::fs::create_dir_all(&kb_dir).unwrap();
        std::fs::write(kb_dir.join("ibm_history.txt"), "test").unwrap();
        std::fs::write(kb_dir.join("guide.pdf"), "test").unwrap();
        let files = discover_existing_resources(Some(tmp.path().to_str().unwrap())).unwrap();
        assert_eq!(files, vec!["- ./knowledge_base/guide.pdf".to_string(), "- ./knowledge_base/ibm_history.txt".to_string()]);
        let block = render_existing_resources(&files);
        assert!(block.contains("knowledge_base/guide.pdf"));
        assert!(block.contains("knowledge_base/ibm_history.txt"));
        assert!(block.contains("Knowledge Base Documents"));

        // A common/knowledge_base/ dir is merged alongside the project-level one.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("knowledge_base")).unwrap();
        std::fs::write(tmp.path().join("knowledge_base/local.txt"), "test").unwrap();
        std::fs::create_dir_all(tmp.path().join("common/knowledge_base")).unwrap();
        std::fs::write(tmp.path().join("common/knowledge_base/shared.txt"), "test").unwrap();
        let block = render_existing_resources(&discover_existing_resources(Some(tmp.path().to_str().unwrap())).unwrap());
        assert!(block.contains("knowledge_base/local.txt"));
        assert!(block.contains("common/knowledge_base/shared.txt"));
    }

    /// Materialize a `<base>/<sub>/tool/<name>/` stub (schema.yaml + <name>.py).
    fn write_tool_stub(base: &std::path::Path, sub: &str, name: &str) {
        let dir = base.join(sub).join("tool").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("schema.yaml"), "input_schema:\n  type: object\n").unwrap();
        std::fs::write(dir.join(format!("{name}.py")), "def main(params):\n    return {}\n").unwrap();
    }

    #[test]
    fn test_discover_tool_stubs() {
        // Empty dir → error (nothing to assemble).
        let tmp = tempfile::tempdir().unwrap();
        assert!(discover_tool_stubs(tmp.path().to_str().unwrap(), &HashMap::new()).is_err());

        // Project + common tools are both included.
        let tmp = tempfile::tempdir().unwrap();
        write_tool_stub(tmp.path(), "", "add");
        write_tool_stub(tmp.path(), "common", "shared_util");
        let result = discover_tool_stubs(tmp.path().join("tool").to_str().unwrap(), &HashMap::new()).unwrap();
        assert!(result.contains("## Tool: add"));
        assert!(result.contains("## Tool: shared_util"));

        // A common tool duplicating a project tool name is deduplicated (project wins).
        let tmp = tempfile::tempdir().unwrap();
        write_tool_stub(tmp.path(), "", "add");
        write_tool_stub(tmp.path(), "common", "add");
        let result = discover_tool_stubs(tmp.path().join("tool").to_str().unwrap(), &HashMap::new()).unwrap();
        assert_eq!(result.matches("## Tool: add").count(), 1, "Duplicate tool 'add' should be deduplicated");
    }
}
