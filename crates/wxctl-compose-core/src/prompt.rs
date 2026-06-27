//! Pure prompt-assembly cores — config + test prompts, and the existing-resources block
//! formatter. Wasm-safe: no FS, no env. The FS-bound implementation prompt and
//! `discover_*` helpers stay native in `wxctl-compose`.

use anyhow::Result;
use std::collections::HashSet;

/// Config-generation prompt (Pass 3). `paths_yaml` scopes the schema reference to the
/// recommended path's kinds; `existing_resources` is the pre-rendered "files already
/// exist" block (empty string when none).
pub fn assemble_config_prompt(user_input: &str, paths_yaml: &str, existing_resources: &str) -> Result<String> {
    let template = crate::templates::CONFIG_GENERATION;
    let preamble = crate::templates::REFERENCE_PREAMBLE;
    let kinds = scoped_kinds(paths_yaml);
    let kind_refs: HashSet<&str> = kinds.iter().map(|s| s.as_str()).collect();
    let rendered = wxctl_schema::render_kinds_markdown(Some(&kind_refs))?;
    let schema_ref = format!("{}\n\n{}", preamble, rendered);
    let worked_examples = select_worked_examples(&kind_refs);
    let body = crate::extract_prompt_body(template);
    Ok(body.replace("<PATHS>", paths_yaml).replace("<EXISTING_RESOURCES>", existing_resources).replace("<SCHEMA_REFERENCE>", &schema_ref).replace("<WORKED_EXAMPLES>", &worked_examples).replace("<USER_INPUT>", user_input))
}

/// Pick the worked examples for the generation prompt by the recommended path's family.
/// Agent-family kinds get the agent few-shots; WML-family kinds get the WML scoring example +
/// rules. A path with neither (or no path at all → empty `kinds`) falls back to the agent
/// examples so the prompt always demonstrates the multi-document config shape. This keeps the
/// WML scoring rules out of agent prompts (and vice versa) instead of shipping all four examples
/// every time.
fn select_worked_examples(kinds: &HashSet<&str>) -> String {
    const AGENT_FAMILY: [&str; 5] = ["agent", "tool", "knowledge_base", "model", "orchestrate_connection"];
    const WML_FAMILY: [&str; 5] = ["space", "software_specification", "wml_function", "wml_deployment", "ai_service"];
    let agent = AGENT_FAMILY.iter().any(|k| kinds.contains(k));
    let wml = WML_FAMILY.iter().any(|k| kinds.contains(k));
    let mut out = String::new();
    if agent || !wml {
        out.push_str(crate::templates::EXAMPLES_AGENT.trim());
    }
    if wml {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(crate::templates::EXAMPLES_WML.trim());
    }
    out
}

/// Test-generation prompt. `config_yaml` + use-case text → prompt.
pub fn assemble_test_prompt(config_yaml: &str, user_input: &str) -> Result<String> {
    let template = crate::templates::TEST_GENERATION;
    let body = crate::extract_prompt_body(template);
    Ok(body.replace("<include config.yaml>", config_yaml).replace("<include use case description>", user_input))
}

/// Render the pre-formatted "files already exist" block from a file list (the entries
/// `discover_existing_resources` produces). Empty list → empty string. Pure: no FS, no
/// env. The single source of the existing-resources block format across surfaces.
pub fn render_existing_resources(files: &[String]) -> String {
    if files.is_empty() {
        return String::new();
    }
    format!(
        "The following files already exist in the project. Reference these exact paths\n\
         in the generated config — do not invent alternative filenames.\n\
         \n\
         ### Knowledge Base Documents\n\
         {}",
        files.join("\n")
    )
}

fn scoped_kinds(paths_yaml: &str) -> Vec<String> {
    #[derive(serde::Deserialize)]
    struct Paths {
        #[serde(default)]
        paths: Vec<PathEntry>,
    }
    #[derive(serde::Deserialize)]
    struct PathEntry {
        #[serde(default)]
        recommended: bool,
        #[serde(default)]
        resources: Vec<Res>,
    }
    #[derive(serde::Deserialize)]
    struct Res {
        kind: String,
    }
    let Ok(parsed) = serde_norway::from_str::<Paths>(paths_yaml) else { return Vec::new() };
    let mut kinds: Vec<String> = Vec::new();
    let recommended: Vec<&PathEntry> = parsed.paths.iter().filter(|p| p.recommended).collect();
    let chosen: Vec<&PathEntry> = if recommended.is_empty() { parsed.paths.iter().collect() } else { recommended };
    for p in chosen {
        for r in &p.resources {
            if !kinds.contains(&r.kind) {
                kinds.push(r.kind.clone());
            }
        }
    }
    kinds
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pass3_substitutes_placeholders_and_scopes_schema_to_recommended_kinds() {
        // Single recommended-agent path: input + path name are injected, every placeholder is substituted.
        let paths_yaml = "format: compose/v1\ndeployment: saas\npaths:\n  - name: minimal\n    recommended: true\n    resources:\n      - kind: agent\n    edges: []\n";
        let prompt = assemble_config_prompt("HR chatbot", paths_yaml, "").unwrap();
        assert!(prompt.contains("HR chatbot"));
        assert!(prompt.contains("minimal"));
        for placeholder in ["<PATHS>", "<USER_INPUT>", "<SCHEMA_REFERENCE>", "<EXISTING_RESOURCES>", "<WORKED_EXAMPLES>"] {
            assert!(!prompt.contains(placeholder), "placeholder {placeholder} must be substituted");
        }

        // The scoped schema reference only renders the recommended kinds (agent), not unrelated kinds.
        let paths_yaml = "format: compose/v1\ndeployment: saas\npaths:\n  - name: only_agent\n    recommended: true\n    resources:\n      - kind: agent\n    edges: []\n";
        let prompt = assemble_config_prompt("an agent", paths_yaml, "").unwrap();
        assert!(prompt.contains("agent"));
        // The reference preamble is static and may name wml_deployment in examples; check the
        // scoped schema rendering (markdown section headers) instead.
        assert!(!prompt.contains("\n# wml_deployment\n"));
    }

    #[test]
    fn test_pass3_worked_examples_scoped_by_family() {
        // Agent path → agent few-shots, no WML noise.
        let agent_paths = "format: compose/v1\npaths:\n  - name: a\n    recommended: true\n    resources:\n      - kind: agent\n      - kind: tool\n";
        let p = assemble_config_prompt("an agent with a tool", agent_paths, "").unwrap();
        assert!(p.contains("Example 1: Minimal (agent only)"), "agent path should carry the agent examples");
        assert!(!p.contains("WML Resource Rules"), "agent path must NOT carry the WML scoring rules");
        assert!(!p.contains("<WORKED_EXAMPLES>"), "placeholder must be substituted");

        // WML path → WML example + rules, no agent few-shots.
        let wml_paths = "format: compose/v1\npaths:\n  - name: w\n    recommended: true\n    resources:\n      - kind: space\n      - kind: wml_function\n      - kind: wml_deployment\n";
        let w = assemble_config_prompt("a scoring service", wml_paths, "").unwrap();
        assert!(w.contains("WML Resource Rules"), "wml path should carry the WML rules");
        assert!(!w.contains("Example 1: Minimal (agent only)"), "wml path must NOT carry the agent examples");

        // No path → fall back to the agent examples (structural demo), still no WML noise.
        let none = assemble_config_prompt("something", "", "").unwrap();
        assert!(none.contains("Example 1: Minimal (agent only)"));
        assert!(!none.contains("WML Resource Rules"));
    }

    #[test]
    fn test_scoped_kinds_recommended_only_and_unparseable_empty() {
        // Only the recommended path's kinds are scoped in (path `b` is not recommended → its `tool` is dropped).
        let paths_yaml = "paths:\n  - name: a\n    recommended: true\n    resources:\n      - kind: agent\n  - name: b\n    resources:\n      - kind: tool\n";
        assert_eq!(scoped_kinds(paths_yaml), vec!["agent".to_string()]);

        // Unparseable YAML → empty (no panic, graceful degradation).
        assert!(scoped_kinds("not: [valid").is_empty());
    }

    #[test]
    fn test_render_existing_resources_empty_and_format() {
        assert_eq!(render_existing_resources(&[]), "");
        let block = render_existing_resources(&["- ./knowledge_base/a.txt".to_string()]);
        let expected = "The following files already exist in the project. Reference these exact paths\nin the generated config — do not invent alternative filenames.\n\n### Knowledge Base Documents\n- ./knowledge_base/a.txt";
        assert_eq!(block, expected);
    }

    #[test]
    fn test_test_prompt_contains_config_and_input() {
        let config = "kind: agent\nref_name: my_agent\nname: my_agent\n";
        let prompt = assemble_test_prompt(config, "A helpful assistant").unwrap();
        assert!(prompt.contains("my_agent"));
        assert!(prompt.contains("A helpful assistant"));
        assert!(!prompt.contains("<include config.yaml>"));
        assert!(!prompt.contains("<include use case description>"));
    }
}
