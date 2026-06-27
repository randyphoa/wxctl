//! Live-tool DTOs + backing logic for `wxctl_validate` and `wxctl_plan`. These call
//! `WxctlClient::validate` / `WxctlClient::plan` (a profile + possible network), unlike
//! the discovery tools in `crate::tools`. Output is trimmed to summaries; raw plan
//! payloads are gated behind `verbose: true`. The server module owns the shared
//! `Arc<WxctlClient>`; this module is pure shaping over the SDK's return types.

use schemars::JsonSchema;
use serde::Serialize;
use wxctl_engine::{AnnotatedValidationError, CompiledPlan, OperationType, ValidationResult};

/// Output for `wxctl_validate`. `valid: false` with a non-empty `errors` list is a
/// *successful* validation that found problems (the tool result is `isError: false`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct ValidateOutput {
    pub valid: bool,
    /// Each error: `{ resource, field, code, message, suggestion }` — the exact shape
    /// `AnnotatedValidationError` serializes to (verified `wxctl-schema/.../validation/types.rs:18`).
    pub errors: Vec<serde_json::Value>,
    /// A ready-to-run LLM correction prompt (`fix.md` template scoped to the failing
    /// kinds). Populated only when `valid == false`; `None` on a valid config.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_prompt: Option<String>,
}

/// One planned operation, trimmed to the fields a host needs to render a diff line.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlanOperation {
    /// `"<kind>/<ref_name>"` — the resource key.
    pub key: String,
    /// `create` | `update` | `delete` | `recreate` | `no-op` | `retain` | `skip (...)`.
    pub op_type: String,
    pub kind: String,
    pub ref_name: String,
}

/// Counts by category. `no_change` aggregates the structural no-ops (`NoOp`, `Retain`,
/// `Skip`), matching the CLI's "unchanged" bucket (`collector.rs:55`).
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlanSummary {
    pub create: usize,
    pub update: usize,
    pub delete: usize,
    pub no_change: usize,
}

/// Output for `wxctl_plan`. `operations` is always the trimmed list; `raw` carries the
/// full operation payloads only when the caller passed `verbose: true`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct PlanOutput {
    pub summary: PlanSummary,
    pub operations: Vec<PlanOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Vec<serde_json::Value>>,
}

/// Map a `ValidationResult` into the trimmed validate output. On failure, attach an
/// inline `fix_prompt` (the `fix.md` template scoped to the failing kinds) so the agent
/// can self-correct in one round-trip. `config_yaml` is the serialized config under test.
pub fn shape_validation(result: &ValidationResult, config_yaml: &str) -> ValidateOutput {
    let errors = result.errors().iter().map(serialize_error).collect();
    let valid = result.is_valid();
    let fix_prompt = if valid { None } else { Some(assemble_fix_prompt(config_yaml, result.errors())) };
    ValidateOutput { valid, errors, fix_prompt }
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

fn serialize_error(err: &AnnotatedValidationError) -> serde_json::Value {
    serde_json::to_value(err).unwrap_or_else(|e| serde_json::json!({ "message": format!("error serialization failed: {e}") }))
}

/// Map a `CompiledPlan` into the trimmed plan output. `verbose` adds a `raw` array of
/// `{ key, op_type, kind, ref_name }` plus best-effort local/remote payloads.
pub fn shape_plan(plan: &CompiledPlan, verbose: bool) -> PlanOutput {
    let mut summary = PlanSummary { create: 0, update: 0, delete: 0, no_change: 0 };
    let mut operations = Vec::with_capacity(plan.operations.len());
    for planned in &plan.operations {
        let op = planned;
        match &op.op_type {
            OperationType::Create => summary.create += 1,
            OperationType::Update { .. } => summary.update += 1,
            OperationType::Recreate => summary.create += 1,
            OperationType::Delete => summary.delete += 1,
            OperationType::NoOp | OperationType::Retain | OperationType::Skip { .. } => summary.no_change += 1,
        }
        operations.push(PlanOperation { key: op.key.to_string(), op_type: op.op_type.to_string(), kind: op.key.kind.to_string(), ref_name: op.key.name.to_string() });
    }
    let raw = verbose.then(|| plan.operations.iter().map(shape_raw_operation).collect());
    PlanOutput { summary, operations, raw }
}

fn shape_raw_operation(planned: &wxctl_engine::Operation) -> serde_json::Value {
    let op = planned;
    serde_json::json!({
        "key": op.key.to_string(),
        "op_type": op.op_type.to_string(),
        "local": op.local.as_ref().map(|r| serde_json::to_value(&r.data).unwrap_or(serde_json::Value::Null)),
        "remote": op.remote.as_ref().map(|r| r.data.clone()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_core::ResourceKey;
    use wxctl_core::ResourceSet;
    use wxctl_engine::Operation;

    fn op(kind: &str, name: &str, op_type: OperationType) -> Operation {
        Operation { key: ResourceKey::new(kind, name), op_type, local: None, remote: None }
    }

    #[test]
    fn shape_plan_buckets_categories_and_gates_raw_on_verbose() {
        let plan = CompiledPlan {
            operations: vec![
                op("s3_bucket", "a", OperationType::Create),
                op("s3_bucket", "b", OperationType::Update { fields: vec!["x".to_string()] }),
                op("s3_bucket", "c", OperationType::Delete),
                op("s3_bucket", "d", OperationType::NoOp),
                op("s3_bucket", "e", OperationType::Retain),
                op("s3_bucket", "f", OperationType::Recreate),
            ],
        };
        // Non-verbose: summary buckets the six op types; raw is omitted.
        let out = shape_plan(&plan, false);
        assert_eq!(out.summary.create, 2, "create + recreate");
        assert_eq!(out.summary.update, 1);
        assert_eq!(out.summary.delete, 1);
        assert_eq!(out.summary.no_change, 2, "noop + retain");
        assert!(out.raw.is_none(), "raw omitted without verbose");
        assert_eq!(out.operations[0].key, "s3_bucket/a");
        assert_eq!(out.operations[0].kind, "s3_bucket");
        assert_eq!(out.operations[0].ref_name, "a");
        assert_eq!(out.operations[1].op_type, "update");

        // Verbose: raw payload is included, carrying the per-op key.
        let out = shape_plan(&plan, true);
        let raw = out.raw.expect("raw present with verbose");
        assert_eq!(raw.len(), 6);
        assert_eq!(raw[0].get("key").and_then(|v| v.as_str()), Some("s3_bucket/a"));
    }

    #[test]
    fn fix_prompt_present_only_on_failure() {
        // A valid result → no fix_prompt.
        let ok = ValidationResult::success(ResourceSet::<wxctl_core::ValidatedResource>::from_sorted(vec![]));
        let out = shape_validation(&ok, "kind: agent\nref_name: a\n");
        assert!(out.valid && out.fix_prompt.is_none());
    }
}
