//! Shared machine-readable output DTOs for the `wxctl` CLI and the `wxctl-mcp` server.
//!
//! Single definition of every result shape (`PlanOutput`, `ExecuteOutput`, `TestOutput`,
//! `ValidateOutput`) plus conversions off the engine/sdk result types. `wxctl-sdk` is the
//! only crate that sees both the engine result types *and* its own `TestResults` while
//! being a dependency of both consumers, so CLI/MCP parity is structural.
//!
//! `Serialize` is always derived; `JsonSchema` is derived only under the `schema` feature
//! (enabled by `wxctl-mcp` for rmcp tool schemas). The CLI depends on this crate without
//! the feature and stays lean.

use crate::testing::{TestResults, TurnOutcome};
use serde::Serialize;
use wxctl_engine::{AnnotatedValidationError, CompiledPlan, ExecutionResult, ExecutionResults, Operation, OperationType, ValidationResult};

// ── validate ──

/// Output for `wxctl_validate` / `validate --output json`. `valid: false` with a non-empty
/// `errors` list is a *successful* validation that found problems.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ValidateOutput {
    pub valid: bool,
    /// Each error: `{ resource, field, code, message, suggestion }` — the exact shape
    /// `AnnotatedValidationError` serializes to.
    pub errors: Vec<serde_json::Value>,
    /// A ready-to-run LLM correction prompt. Populated only when `valid == false`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_prompt: Option<String>,
}

// ── plan ──

/// One planned operation, trimmed to the fields a host needs to render a diff line.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanOperation {
    /// `"<kind>/<ref_name>"` — the resource key.
    pub key: String,
    /// `create` | `update` | `delete` | `recreate` | `no-op` | `retain` | `skip (...)`.
    pub op_type: String,
    pub kind: String,
    pub ref_name: String,
}

/// Counts by category. `no_change` aggregates the structural no-ops (`NoOp`, `Retain`, `Skip`).
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanSummary {
    pub create: usize,
    pub update: usize,
    pub delete: usize,
    pub no_change: usize,
}

/// Output for `wxctl_plan` / `plan --output json`. `raw` carries full operation payloads
/// only when the caller passed `verbose: true`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct PlanOutput {
    pub summary: PlanSummary,
    pub operations: Vec<PlanOperation>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Vec<serde_json::Value>>,
}

// ── execute (apply / destroy) ──

/// One failed resource, trimmed to key + error.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct FailedResource {
    pub key: String,
    pub error: String,
}

/// Counts for an apply/destroy run.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecuteSummary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: bool,
}

/// Output for `wxctl_apply` / `wxctl_destroy` / `apply|destroy --output json`. `raw` carries
/// the per-resource API `response` only when the caller passed `verbose: true`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ExecuteOutput {
    /// Run-record id for this run. Pass to `run_diagnose` / `wxctl debug` on failure.
    pub run_id: String,
    pub summary: ExecuteSummary,
    pub succeeded: Vec<String>,
    pub failed: Vec<FailedResource>,
    pub skipped: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Vec<serde_json::Value>>,
}

// ── test ──

/// One conversation turn's outcome, trimmed to a label.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TurnSummary {
    pub turn_num: usize,
    /// `success` | `tool_mismatch` | `error`.
    pub outcome: String,
}

/// One test case's result.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TestCaseSummary {
    pub ref_name: String,
    pub passed: bool,
    pub turns: Vec<TurnSummary>,
}

/// Output for `wxctl_test` / `test --output json`.
#[derive(Debug, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct TestOutput {
    /// Run-record id for this run. Pass to `run_diagnose` / `wxctl debug` on failure.
    pub run_id: String,
    pub passed: usize,
    pub failed: usize,
    pub tests: Vec<TestCaseSummary>,
}

// ── conversions ──

/// Non-verbose plan shape (`raw: None`); the entry point for the CLI.
impl From<&CompiledPlan> for PlanOutput {
    fn from(plan: &CompiledPlan) -> Self {
        plan_output(plan, false)
    }
}

/// Map a `CompiledPlan` into `PlanOutput`. `verbose` adds a `raw` array of
/// `{ key, op_type, local, remote }` payloads.
pub fn plan_output(plan: &CompiledPlan, verbose: bool) -> PlanOutput {
    let mut summary = PlanSummary { create: 0, update: 0, delete: 0, no_change: 0 };
    let mut operations = Vec::with_capacity(plan.operations.len());
    for op in &plan.operations {
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

fn shape_raw_operation(op: &Operation) -> serde_json::Value {
    serde_json::json!({
        "key": op.key.to_string(),
        "op_type": op.op_type.to_string(),
        "local": op.local.as_ref().map(|r| serde_json::to_value(&r.data).unwrap_or(serde_json::Value::Null)),
        "remote": op.remote.as_ref().map(|r| r.data.clone()),
    })
}

fn key_string(r: &ExecutionResult) -> String {
    r.key.to_string()
}

/// Map an `ExecutionResults` into `ExecuteOutput`. `verbose` adds a `raw` array of
/// `{ key, success, response }` for every succeeded + failed result.
pub fn execute_output(run_id: String, results: &ExecutionResults, verbose: bool) -> ExecuteOutput {
    let summary = ExecuteSummary { succeeded: results.succeeded.len(), failed: results.failed.len(), skipped: results.skipped.len(), cancelled: results.cancelled };
    let succeeded = results.succeeded.iter().map(key_string).collect();
    let failed = results.failed.iter().map(|r| FailedResource { key: key_string(r), error: r.error.clone().unwrap_or_default() }).collect();
    let skipped = results.skipped.iter().map(ToString::to_string).collect();
    let raw = verbose.then(|| results.succeeded.iter().chain(results.failed.iter()).map(|r| serde_json::json!({ "key": key_string(r), "success": r.success, "response": r.response.clone() })).collect());
    ExecuteOutput { run_id, summary, succeeded, failed, skipped, raw }
}

fn turn_outcome_label(outcome: &TurnOutcome) -> &'static str {
    match outcome {
        TurnOutcome::Success { .. } => "success",
        TurnOutcome::ToolMismatch { .. } => "tool_mismatch",
        TurnOutcome::Error(_) => "error",
    }
}

/// Map a `TestResults` into `TestOutput`.
pub fn test_output(run_id: String, results: &TestResults) -> TestOutput {
    let tests = results.tests.iter().map(|t| TestCaseSummary { ref_name: t.ref_name.clone(), passed: t.passed, turns: t.turns.iter().map(|tr| TurnSummary { turn_num: tr.turn_num, outcome: turn_outcome_label(&tr.outcome).to_string() }).collect() }).collect();
    TestOutput { run_id, passed: results.passed, failed: results.failed, tests }
}

fn serialize_error(err: &AnnotatedValidationError) -> serde_json::Value {
    serde_json::to_value(err).unwrap_or_else(|e| serde_json::json!({ "message": format!("error serialization failed: {e}") }))
}

/// Map a `ValidationResult` into `ValidateOutput`. `fix_prompt` is supplied by the caller
/// (the CLI and MCP assemble it from `wxctl-compose-core`, which this crate must not depend
/// on); pass `None` on a valid config so a valid result never carries a prompt.
pub fn validate_output(result: &ValidationResult, fix_prompt: Option<String>) -> ValidateOutput {
    let errors = result.errors().iter().map(serialize_error).collect();
    ValidateOutput { valid: result.is_valid(), errors, fix_prompt }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_core::ResourceKey;

    fn op(kind: &str, name: &str, op_type: OperationType) -> Operation {
        Operation { key: ResourceKey::new(kind, name), op_type, local: None, remote: None }
    }

    #[test]
    fn plan_output_buckets_categories_and_gates_raw_on_verbose() {
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
        let out = plan_output(&plan, false);
        assert_eq!(out.summary.create, 2, "create + recreate");
        assert_eq!(out.summary.update, 1);
        assert_eq!(out.summary.delete, 1);
        assert_eq!(out.summary.no_change, 2, "noop + retain");
        assert!(out.raw.is_none(), "raw omitted without verbose");
        assert_eq!(out.operations[0].key, "s3_bucket/a");
        assert_eq!(out.operations[0].kind, "s3_bucket");
        assert_eq!(out.operations[0].ref_name, "a");
        assert_eq!(out.operations[1].op_type, "update");
        // `From` matches the non-verbose shape.
        assert!(PlanOutput::from(&plan).raw.is_none());

        // Verbose: raw payload is included, carrying the per-op key.
        let out = plan_output(&plan, true);
        let raw = out.raw.expect("raw present with verbose");
        assert_eq!(raw.len(), 6);
        assert_eq!(raw[0].get("key").and_then(|v| v.as_str()), Some("s3_bucket/a"));
    }

    fn exec_ok(kind: &str, name: &str, response: Option<serde_json::Value>) -> ExecutionResult {
        ExecutionResult { key: ResourceKey::new(kind, name), operation: OperationType::Create, success: true, error: None, response, attempts: 1 }
    }

    fn exec_err(kind: &str, name: &str, msg: &str) -> ExecutionResult {
        ExecutionResult { key: ResourceKey::new(kind, name), operation: OperationType::Create, success: false, error: Some(msg.to_string()), response: None, attempts: 1 }
    }

    #[test]
    fn execute_output_trims_buckets_and_gates_raw_on_verbose() {
        // Non-verbose: succeeded/failed/skipped buckets + trimmed keys; raw omitted.
        let results = ExecutionResults { succeeded: vec![exec_ok("space", "a", Some(serde_json::json!({ "id": "x" })))], failed: vec![exec_err("space", "b", "boom")], skipped: vec![ResourceKey::new("space", "c")], cancelled: false };
        let out = execute_output("test-run-id-001".to_string(), &results, false);
        assert_eq!(out.run_id, "test-run-id-001");
        assert_eq!(out.summary.succeeded, 1);
        assert_eq!(out.summary.failed, 1);
        assert_eq!(out.summary.skipped, 1);
        assert!(!out.summary.cancelled);
        assert_eq!(out.succeeded, vec!["space/a".to_string()]);
        assert_eq!(out.failed[0].key, "space/b");
        assert_eq!(out.failed[0].error, "boom");
        assert_eq!(out.skipped, vec!["space/c".to_string()]);
        assert!(out.raw.is_none(), "raw omitted without verbose");

        // Verbose: raw carries the per-op key + full response body.
        let results = ExecutionResults { succeeded: vec![exec_ok("space", "a", Some(serde_json::json!({ "id": "x" })))], failed: Vec::new(), skipped: Vec::new(), cancelled: false };
        let out = execute_output("test-run-id-002".to_string(), &results, true);
        assert_eq!(out.run_id, "test-run-id-002");
        let raw = out.raw.expect("raw present with verbose");
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].get("key").and_then(|v| v.as_str()), Some("space/a"));
        assert_eq!(raw[0].get("response").and_then(|r| r.get("id")).and_then(|v| v.as_str()), Some("x"));
    }

    /// Parity guard (spec AC2): pins each DTO's JSON. Renaming any field flips the snapshot.
    /// Serialize to pretty JSON and snapshot the string (base `insta`, no `json` feature),
    /// matching the repo's existing `assert_snapshot!` usage.
    #[test]
    fn dto_json_shapes_are_pinned() {
        let plan = PlanOutput { summary: PlanSummary { create: 1, update: 2, delete: 3, no_change: 4 }, operations: vec![PlanOperation { key: "space/a".into(), op_type: "create".into(), kind: "space".into(), ref_name: "a".into() }], raw: None };
        insta::assert_snapshot!("plan_output", serde_json::to_string_pretty(&plan).unwrap());

        let execute =
            ExecuteOutput { run_id: "run-001".into(), summary: ExecuteSummary { succeeded: 1, failed: 1, skipped: 1, cancelled: false }, succeeded: vec!["space/a".into()], failed: vec![FailedResource { key: "space/b".into(), error: "boom".into() }], skipped: vec!["space/c".into()], raw: None };
        insta::assert_snapshot!("execute_output", serde_json::to_string_pretty(&execute).unwrap());

        let test = TestOutput {
            run_id: "run-002".into(),
            passed: 1,
            failed: 1,
            tests: vec![TestCaseSummary { ref_name: "t1".into(), passed: true, turns: vec![TurnSummary { turn_num: 1, outcome: "success".into() }] }, TestCaseSummary { ref_name: "t2".into(), passed: false, turns: vec![TurnSummary { turn_num: 1, outcome: "tool_mismatch".into() }] }],
        };
        insta::assert_snapshot!("test_output", serde_json::to_string_pretty(&test).unwrap());

        let validate = ValidateOutput { valid: false, errors: vec![serde_json::json!({ "resource": "agent/a", "field": "name", "code": "V001", "message": "missing", "suggestion": "add name" })], fix_prompt: Some("fix this".into()) };
        insta::assert_snapshot!("validate_output", serde_json::to_string_pretty(&validate).unwrap());
    }
}
