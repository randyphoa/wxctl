//! Backing logic for `wxctl_validate` / `wxctl_plan`. The output DTOs now live in
//! `wxctl_sdk::json`; this module keeps the MCP-only `fix_prompt` assembly (which pulls in
//! `wxctl-compose-core` + `wxctl-schema`) and a thin `shape_validation` wrapper that
//! computes the prompt on failure before delegating to the shared shape. `wxctl_plan`
//! calls `wxctl_sdk::json::plan_output` directly (see `crate::server`).

use wxctl_engine::{Advisory, AnnotatedValidationError, ValidationError, ValidationResult};
use wxctl_sdk::json::{ValidateOutput, validate_output};

/// Map a `ValidationResult` into the shared validate output. On failure, attach an inline
/// `fix_prompt` (the `fix.md` template scoped to the failing + suggested kinds, with an
/// advisories appendix when present) so the agent can self-correct in one round-trip.
/// `config_yaml` is the serialized config under test.
pub fn shape_validation(result: &ValidationResult, config_yaml: &str) -> ValidateOutput {
    let fix_prompt = if result.is_valid() { None } else { Some(assemble_fix_prompt(config_yaml, result.errors(), result.advisories())) };
    validate_output(result, fix_prompt)
}

/// In-memory mirror of the CLI's no-original-prompt `assemble_fix_prompt`
/// (`wxctl/crates/wxctl/src/commands/validate.rs`): `fix.md` body with `<CONFIG>`/`<ERRORS>`/
/// `<SCHEMA_REFERENCE>` substituted; schema docs scoped to the kinds that have errors plus
/// every kind an add-resource suggestion names; a non-blocking advisories appendix when present.
fn assemble_fix_prompt(config_yaml: &str, errors: &[AnnotatedValidationError], advisories: &[Advisory]) -> String {
    let errors_text = errors.iter().enumerate().map(|(i, e)| format!("{}. [{}] {}: {}. {}", i + 1, e.resource, e.error.field(), e.error, e.error.suggestion())).collect::<Vec<_>>().join("\n");
    let mut ref_kinds: std::collections::HashSet<&str> = errors.iter().filter_map(|e| if e.resource.is_empty() { None } else { e.resource.split('/').next() }).collect();
    for e in errors {
        if let ValidationError::UnresolvedReference { ref_kind, required_chain, .. } = &e.error {
            ref_kinds.insert(ref_kind.as_str());
            for (kind, _, _) in required_chain {
                ref_kinds.insert(kind.as_str());
            }
        }
    }
    let schema_ref = wxctl_schema::render_kinds_markdown(Some(&ref_kinds)).unwrap_or_default();
    let template = wxctl_compose_core::templates::FIX;
    let body = wxctl_compose_core::extract_prompt_body(template);
    let mut prompt = body.replace("<CONFIG>", config_yaml).replace("<ERRORS>", &errors_text).replace("<SCHEMA_REFERENCE>", &schema_ref);
    if !advisories.is_empty() {
        prompt.push_str("\n\n## Advisories (non-blocking)\n\n");
        for a in advisories {
            prompt.push_str(&format!("- [{}] {}: {}. {}\n", a.code, a.resource, a.message, a.suggestion));
        }
    }
    prompt
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

    #[test]
    fn shape_validation_passes_warnings_and_builds_fix_prompt() {
        // A failing result: one dangling-ref error (with a chain) + one advisory.
        let err = AnnotatedValidationError {
            resource: "agent/a".to_string(),
            error: ValidationError::UnresolvedReference { field_path: "tools[0]".to_string(), ref_kind: "wml_model".to_string(), ref_name: "absent".to_string(), required_chain: vec![("autoai_experiment".to_string(), "wml_model".to_string(), "experiment".to_string())] },
        };
        let adv = Advisory { code: "WXCTL-V505".into(), resource: "common_core_connection/db".into(), message: "orphan resource".into(), suggestion: "add an orchestrate_connection".into() };
        let result = ValidationResult::failure(vec![err]).with_advisories(vec![adv]);

        let out = shape_validation(&result, "kind: agent\nref_name: a\n");
        // AC9 (MCP): same DTO shape — warnings passed through, valid false.
        assert!(!out.valid);
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].code, "WXCTL-V505");
        // AC10: fix prompt carries the loosened add-rule, schema docs for suggested kinds
        // (the unresolved kind + its chain), and a non-blocking advisories appendix.
        let fp = out.fix_prompt.expect("failing result carries a fix_prompt");
        assert!(fp.contains("You MAY add a new resource document ONLY when an error's suggestion explicitly names a"), "loosened rule missing");
        assert!(fp.contains("# wml_model"), "schema ref for the suggested kind missing");
        assert!(fp.contains("# autoai_experiment"), "schema ref for the chain kind missing");
        assert!(fp.contains("## Advisories (non-blocking)"), "advisories appendix missing");
        assert!(fp.contains("WXCTL-V505"), "advisory line missing from appendix");
    }
}
