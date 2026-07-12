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

/// Data-generation prompt. Parameterized by the use case, the **whole config**
/// (shared context so the model keeps one seeded population consistent across
/// resources — the coherence deliverable), and the detected `DataNeed` list;
/// scopes the schema reference to the needs' kinds. `config_yaml` is the raw
/// multi-doc config string (same shape `assemble_test_prompt` takes) — wasm-safe,
/// no `Config` dependency.
pub fn assemble_data_prompt(use_case: &str, config_yaml: &str, needs: &[crate::data::DataNeed]) -> Result<String> {
    let template = crate::templates::DATA_GENERATION;
    let body = crate::extract_prompt_body(template);
    let kind_refs: HashSet<&str> = needs.iter().map(|n| n.kind.as_str()).collect();
    let schema_ref = wxctl_schema::render_kinds_markdown(Some(&kind_refs))?;
    Ok(body.replace("<USE_CASE>", use_case).replace("<CONFIG_CONTEXT>", config_yaml).replace("<DATA_NEEDS>", &render_data_needs(needs)).replace("<SCHEMA_REFERENCE>", &schema_ref))
}

/// One bullet per need: `- <ref_name> (<kind>.<field>) [<mode>]: <shape>`, where
/// `<mode>` is `fixture` or `embedded` so the prompt's delivery branch keys off it.
fn render_data_needs(needs: &[crate::data::DataNeed]) -> String {
    use crate::data::Delivery;
    if needs.is_empty() {
        return "(none)".to_string();
    }
    needs
        .iter()
        .map(|n| {
            let mode = match n.delivery {
                Delivery::Fixture => "fixture",
                Delivery::Embedded => "embedded",
            };
            format!("- {} ({}.{}) [{}]: {}", n.ref_name, n.kind, n.field, mode, n.shape.format.as_deref().unwrap_or("unknown"))
        })
        .collect::<Vec<_>>()
        .join("\n")
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

    #[test]
    fn data_prompt_substitutes_and_lists_needs() {
        use crate::data::{DataNeed, DataShape, DataSource, Delivery};
        let config_yaml = "kind: data_asset\nref_name: customers\nname: customers\nsource_path: customers.csv\n---\nkind: wml_function\nref_name: scorer\nname: scorer\nsource_path: score.py\n";
        let needs = vec![
            DataNeed { ref_name: "customers".into(), kind: "data_asset".into(), field: "source_path".into(), parent: None, shape: DataShape { format: Some("csv".into()) }, delivery: Delivery::Fixture, source: DataSource::Inferred },
            DataNeed { ref_name: "scorer".into(), kind: "wml_function".into(), field: "source_path".into(), parent: None, shape: DataShape { format: Some("py".into()) }, delivery: Delivery::Embedded, source: DataSource::Inferred },
        ];
        let p = assemble_data_prompt("a customer dataset", config_yaml, &needs).unwrap();
        assert!(p.contains("a customer dataset"));
        assert!(p.contains("- customers (data_asset.source_path) [fixture]: csv"));
        assert!(p.contains("- scorer (wml_function.source_path) [embedded]: py"));
        for ph in ["<USE_CASE>", "<CONFIG_CONTEXT>", "<DATA_NEEDS>", "<SCHEMA_REFERENCE>"] {
            assert!(!p.contains(ph), "placeholder {ph} must be substituted");
        }
    }

    #[test]
    fn data_prompt_carries_whole_config_context() {
        // AC6: the assembled data prompt embeds the whole config as shared context and
        // leaves no placeholder unresolved (incl. the new <CONFIG_CONTEXT> slot).
        let config_yaml = "kind: data_asset\nref_name: customers\nname: customers\nsource_path: customers.csv\n---\nkind: wml_function\nref_name: scorer\nname: scorer\nsource_path: score.py\n";
        let resources = crate::data::parse_resources(config_yaml).unwrap();
        let needs = crate::data::detect_data_needs(&resources);
        let p = assemble_data_prompt("a customer dataset", config_yaml, &needs).unwrap();
        // Whole-config context present: both ref_names from the config appear verbatim.
        assert!(p.contains("ref_name: customers"), "config context (data_asset) present");
        assert!(p.contains("ref_name: scorer"), "config context (wml_function) present");
        for ph in ["<USE_CASE>", "<CONFIG_CONTEXT>", "<DATA_NEEDS>", "<SCHEMA_REFERENCE>"] {
            assert!(!p.contains(ph), "placeholder {ph} must be substituted");
        }
    }

    #[test]
    fn data_template_carries_no_domain_literals() {
        // I2/AC6: the template must be product/scenario/domain-agnostic.
        let t = crate::templates::DATA_GENERATION.to_lowercase();
        for banned in ["churn", "credit", "watsonx", "insurance", "telco", "iris", "titanic"] {
            assert!(!t.contains(banned), "data-generation template must not name a domain: {banned}");
        }
    }
}
