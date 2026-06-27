//! Prompt templates embedded at compile time via `include_str!`. The template bytes ship
//! inside this crate (`templates/`), so runtime template-not-found failures are impossible and
//! the public `wxctl/` workspace stays self-contained (no climb to repo-root assets). `fix.md`
//! is embedded here too and shared with `validate --fix-prompt`.

/// Orchestrate SDK version pinned into the implementation prompt's requirements rule.
/// Single source of truth — the `<ORCHESTRATE_VERSION>` placeholder in `implementation.md` is
/// substituted from this constant at assembly time.
pub const ORCHESTRATE_VERSION: &str = "1.6.1";

pub const IDENTIFICATION: &str = include_str!("../templates/prompts/identification.md");
pub const CONFIG_GENERATION: &str = include_str!("../templates/prompts/config-generation.md");
/// Worked examples injected into the config-generation prompt's `<WORKED_EXAMPLES>` slot,
/// selected by the recommended path's family (see `prompt::assemble_config_prompt`). Split out
/// of `config-generation.md` so a non-agent path no longer ships the agent few-shots and an
/// agent path no longer ships the WML scoring rules.
pub const EXAMPLES_AGENT: &str = include_str!("../templates/prompts/examples-agent.md");
pub const EXAMPLES_WML: &str = include_str!("../templates/prompts/examples-wml.md");
pub const IMPLEMENTATION: &str = include_str!("../templates/prompts/implementation.md");
pub const TEST_GENERATION: &str = include_str!("../templates/prompts/test-generation.md");
pub const FIX: &str = include_str!("../templates/prompts/fix.md");
pub const REFERENCE_PREAMBLE: &str = include_str!("../templates/schema/reference-preamble.md");

#[cfg(test)]
mod golden {
    use super::*;
    use serde::Deserialize;
    use wxctl_schema::validate_config;

    /// Extract every ```yaml fenced block from a markdown template.
    fn yaml_blocks(md: &str) -> Vec<String> {
        let mut blocks = Vec::new();
        let mut lines = md.lines();
        while let Some(line) = lines.next() {
            if line.trim_start().starts_with("```yaml") {
                let mut buf = String::new();
                for l in lines.by_ref() {
                    if l.trim_start().starts_with("```") {
                        break;
                    }
                    buf.push_str(l);
                    buf.push('\n');
                }
                blocks.push(buf);
            }
        }
        blocks
    }

    #[test]
    fn config_generation_examples_validate_offline() {
        // The worked configs now live in the family-scoped example files that the config prompt
        // injects via <WORKED_EXAMPLES>; scan them together with config-generation.md (which still
        // carries the clarification-request illustration) so the drift guard keeps covering them.
        let combined = format!("{CONFIG_GENERATION}\n{EXAMPLES_AGENT}\n{EXAMPLES_WML}");
        let blocks = yaml_blocks(&combined);
        assert!(!blocks.is_empty(), "no yaml examples found in config-generation example templates");
        let mut validated = 0;
        for (i, yaml) in blocks.iter().enumerate() {
            // Skip ONLY the clarification-request illustration — a contract template with
            // `<angle-bracket>` placeholders, not a real config. Real config examples that
            // legitimately contain angle brackets (e.g. an `<svg>` agent icon) MUST be validated;
            // a broad `contains('<')` skip would silently drop them and gut the drift guard.
            if yaml.contains("kind: clarification_request") {
                continue;
            }
            let report = validate_config(yaml).unwrap_or_else(|e| panic!("config example #{} failed to parse: {}", i + 1, e));
            assert!(report.valid, "config-generation.md example #{} is invalid offline: {:?}", i + 1, report.errors);
            validated += 1;
        }
        // Count guard: if the skip ever over-matches and drops real examples, this fails
        // loudly instead of passing vacuously. config-generation.md ships 4 real configs.
        assert!(validated >= 4, "expected >= 4 real config examples validated, got {validated} — skip filter too broad?");
    }

    // `test-generation.md` examples are `kind: test` documents (conversation turns / payload
    // assertions). `kind: test` is an engine-level concept filtered out *before* schema validation
    // (`commands/common.rs` retains `r.kind != "test"`) and has NO registered schema, so the offline
    // `validate_config` path cannot schema-check them (it would return `UnknownResourceType`). The
    // meaningful golden guard here is therefore YAML well-formedness: every non-illustration block
    // must parse cleanly, so a rotted/malformed example fails CI. The schema-drift guard lives on the
    // config-generation examples (see `config_generation_examples_validate_offline`).
    #[test]
    fn test_generation_examples_parse_as_yaml() {
        let blocks = yaml_blocks(TEST_GENERATION);
        assert!(!blocks.is_empty(), "no yaml examples found in test-generation.md");
        for (i, yaml) in blocks.iter().enumerate() {
            // Skip format-illustration snippets that use `<placeholder>` angle brackets.
            if yaml.contains('<') && yaml.contains('>') {
                continue;
            }
            for doc in serde_norway::Deserializer::from_str(yaml) {
                serde_norway::Value::deserialize(doc).unwrap_or_else(|e| panic!("test-generation.md example #{} is not well-formed YAML: {}", i + 1, e));
            }
        }
    }
}
