//! Backing logic for `wxctl_validate` / `wxctl_plan`. The output DTOs now live in
//! `wxctl_sdk::json`; this module keeps the MCP-only `fix_prompt` assembly (which pulls in
//! `wxctl-compose-core` + `wxctl-schema`) and a thin `shape_validation` wrapper that
//! computes the prompt on failure before delegating to the shared shape. `wxctl_plan`
//! calls `wxctl_sdk::json::plan_output` directly (see `crate::server`).

use wxctl_engine::{AnnotatedValidationError, ValidationResult};
use wxctl_sdk::json::{ValidateOutput, validate_output};

/// Map a `ValidationResult` into the shared validate output. On failure, attach an inline
/// `fix_prompt` (the `fix.md` template scoped to the failing kinds) so the agent can
/// self-correct in one round-trip. `config_yaml` is the serialized config under test.
pub fn shape_validation(result: &ValidationResult, config_yaml: &str) -> ValidateOutput {
    let fix_prompt = if result.is_valid() { None } else { Some(assemble_fix_prompt(config_yaml, result.errors())) };
    validate_output(result, fix_prompt)
}

/// In-memory mirror of the CLI's no-original-prompt `assemble_fix_prompt`
/// (`wxctl/crates/wxctl/src/commands/validate.rs`): `fix.md` body with `<CONFIG>`/`<ERRORS>`/
/// `<SCHEMA_REFERENCE>` substituted, schema docs scoped to the kinds that have errors.
fn assemble_fix_prompt(config_yaml: &str, errors: &[AnnotatedValidationError]) -> String {
    let errors_text = errors.iter().enumerate().map(|(i, e)| format!("{}. [{}] {}: {}. {}", i + 1, e.resource, e.error.field(), e.error, e.error.suggestion())).collect::<Vec<_>>().join("\n");
    let failing_kinds: std::collections::HashSet<&str> = errors.iter().filter_map(|e| if e.resource.is_empty() { None } else { e.resource.split('/').next() }).collect();
    let schema_ref = wxctl_schema::render_kinds_markdown(Some(&failing_kinds)).unwrap_or_default();
    let template = wxctl_compose_core::templates::FIX;
    let body = wxctl_compose_core::extract_prompt_body(template);
    body.replace("<CONFIG>", config_yaml).replace("<ERRORS>", &errors_text).replace("<SCHEMA_REFERENCE>", &schema_ref)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_core::ResourceSet;

    #[test]
    fn fix_prompt_present_only_on_failure() {
        // A valid result → no fix_prompt.
        let ok = ValidationResult::success(ResourceSet::<wxctl_core::ValidatedResource>::from_sorted(vec![]));
        let out = shape_validation(&ok, "kind: agent\nref_name: a\n");
        assert!(out.valid && out.fix_prompt.is_none());
    }
}
