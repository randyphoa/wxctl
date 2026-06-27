//! DTOs + shaping + progress-bridge observers for the Phase 3 mutating tools
//! (`wxctl_apply`, `wxctl_destroy`, `wxctl_test`).
//!
//! Output is trimmed: apply/destroy return succeeded/failed/skipped keys + counts,
//! with the raw per-resource API `response` gated behind `verbose: true`; test returns
//! per-case pass/fail + per-turn outcome labels.
//!
//! The engine `ExecutionObserver` and the SDK `TestObserver` are **synchronous**
//! (`fn on_*(&self, ..)`), but MCP progress is sent via `Peer::notify_progress`, which
//! is **async**. The two observer types here bridge that gap: each callback pushes a
//! `ProgressEvent` onto a `tokio::sync::mpsc::unbounded_channel`; the server spawns a
//! task (see `crate::server`) that drains the receiver and awaits `notify_progress`.

use schemars::JsonSchema;
use serde::Serialize;
use tokio::sync::mpsc::UnboundedSender;
use wxctl_engine::{ExecutionObserver, ExecutionResult, ExecutionResults};
use wxctl_sdk::{TestObserver, TestResults, TurnOutcome};

/// A progress step to forward to the MCP client. `message` is a short human label;
/// `done` is a monotonically increasing completed-count for the `progress` field.
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub done: f64,
    pub message: String,
}

/// One failed resource, trimmed to key + error.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FailedResource {
    pub key: String,
    pub error: String,
}

/// Counts for an apply/destroy run.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ExecuteSummary {
    pub succeeded: usize,
    pub failed: usize,
    pub skipped: usize,
    pub cancelled: bool,
}

/// Output for `wxctl_apply` / `wxctl_destroy`. `raw` carries the per-resource API
/// `response` only when the caller passed `verbose: true`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ExecuteOutput {
    /// Run-record id for this run. Pass to `run_diagnose` on failure.
    pub run_id: String,
    pub summary: ExecuteSummary,
    pub succeeded: Vec<String>,
    pub failed: Vec<FailedResource>,
    pub skipped: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<Vec<serde_json::Value>>,
}

/// One conversation turn's outcome, trimmed to a label.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TurnSummary {
    pub turn_num: usize,
    /// `success` | `tool_mismatch` | `error`.
    pub outcome: String,
}

/// One test case's result.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TestCaseSummary {
    pub ref_name: String,
    pub passed: bool,
    pub turns: Vec<TurnSummary>,
}

/// Output for `wxctl_test`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct TestOutput {
    /// Run-record id for this run. Pass to `run_diagnose` on failure.
    pub run_id: String,
    pub passed: usize,
    pub failed: usize,
    pub tests: Vec<TestCaseSummary>,
}

fn key_string(r: &ExecutionResult) -> String {
    r.key.to_string()
}

/// Trim an `ExecutionResults` into `ExecuteOutput`. `verbose` adds a `raw` array of
/// `{ key, success, response }` (the per-resource API payload) for every succeeded +
/// failed result.
pub fn shape_execution(results: &ExecutionResults, verbose: bool, run_id: String) -> ExecuteOutput {
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

/// Trim a `TestResults` into `TestOutput`.
pub fn shape_tests(results: &TestResults, run_id: String) -> TestOutput {
    let tests = results.tests.iter().map(|t| TestCaseSummary { ref_name: t.ref_name.clone(), passed: t.passed, turns: t.turns.iter().map(|tr| TurnSummary { turn_num: tr.turn_num, outcome: turn_outcome_label(&tr.outcome).to_string() }).collect() }).collect();
    TestOutput { run_id, passed: results.passed, failed: results.failed, tests }
}

/// Bridges the engine's synchronous `ExecutionObserver` callbacks to async MCP progress
/// by pushing `ProgressEvent`s onto an unbounded channel. A monotonic counter drives the
/// `done` field. A closed channel (drain task gone) is benign — sends are best-effort.
pub struct ProgressExecutionObserver {
    tx: UnboundedSender<ProgressEvent>,
    done: std::sync::atomic::AtomicU64,
}

impl ProgressExecutionObserver {
    pub fn new(tx: UnboundedSender<ProgressEvent>) -> Self {
        Self { tx, done: std::sync::atomic::AtomicU64::new(0) }
    }

    fn emit(&self, message: String) {
        let done = self.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let _ = self.tx.send(ProgressEvent { done: done as f64, message });
    }
}

impl ExecutionObserver for ProgressExecutionObserver {
    fn on_task_start(&self, key: &wxctl_core::ResourceKey) {
        let _ = self.tx.send(ProgressEvent { done: self.done.load(std::sync::atomic::Ordering::Relaxed) as f64, message: format!("start {}/{}", key.kind, key.name) });
    }

    fn on_task_complete(&self, key: &wxctl_core::ResourceKey, success: bool, _duration: std::time::Duration, _response: Option<&serde_json::Value>) {
        self.emit(format!("{} {}/{}", if success { "ok" } else { "failed" }, key.kind, key.name));
    }

    fn on_task_skipped(&self, key: &wxctl_core::ResourceKey, _reason: &str) {
        self.emit(format!("skipped {}/{}", key.kind, key.name));
    }

    fn on_task_error(&self, key: &wxctl_core::ResourceKey, error: &str) {
        let _ = self.tx.send(ProgressEvent { done: self.done.load(std::sync::atomic::Ordering::Relaxed) as f64, message: format!("error {}/{}: {error}", key.kind, key.name) });
    }
}

/// Bridges the SDK's synchronous `TestObserver` callbacks to async MCP progress.
pub struct ProgressTestObserver {
    tx: UnboundedSender<ProgressEvent>,
}

impl ProgressTestObserver {
    pub fn new(tx: UnboundedSender<ProgressEvent>) -> Self {
        Self { tx }
    }
}

impl TestObserver for ProgressTestObserver {
    fn on_test_start(&self, test_name: &str) {
        let _ = self.tx.send(ProgressEvent { done: 0.0, message: format!("start test {test_name}") });
    }

    fn on_test_complete(&self, test_name: &str, passed: bool, completed: usize, total: usize) {
        let _ = self.tx.send(ProgressEvent { done: completed as f64, message: format!("{}/{} {} {}", completed, total, if passed { "passed" } else { "failed" }, test_name) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_core::ResourceKey;
    use wxctl_engine::{ExecutionResult, ExecutionResults, OperationType};

    fn exec_ok(kind: &str, name: &str, response: Option<serde_json::Value>) -> ExecutionResult {
        ExecutionResult { key: ResourceKey::new(kind, name), operation: OperationType::Create, success: true, error: None, response, attempts: 1 }
    }

    fn exec_err(kind: &str, name: &str, msg: &str) -> ExecutionResult {
        ExecutionResult { key: ResourceKey::new(kind, name), operation: OperationType::Create, success: false, error: Some(msg.to_string()), response: None, attempts: 1 }
    }

    #[test]
    fn shape_execution_trims_buckets_and_gates_raw_on_verbose() {
        // Non-verbose: succeeded/failed/skipped buckets + trimmed keys; raw omitted.
        let results = ExecutionResults { succeeded: vec![exec_ok("space", "a", Some(serde_json::json!({ "id": "x" })))], failed: vec![exec_err("space", "b", "boom")], skipped: vec![ResourceKey::new("space", "c")], cancelled: false };
        let out = shape_execution(&results, false, "test-run-id-001".to_string());
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
        let out = shape_execution(&results, true, "test-run-id-002".to_string());
        assert_eq!(out.run_id, "test-run-id-002");
        let raw = out.raw.expect("raw present with verbose");
        assert_eq!(raw.len(), 1);
        assert_eq!(raw[0].get("key").and_then(|v| v.as_str()), Some("space/a"));
        assert_eq!(raw[0].get("response").and_then(|r| r.get("id")).and_then(|v| v.as_str()), Some("x"));
    }

    #[test]
    fn progress_observer_emits_on_complete() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let obs = ProgressExecutionObserver::new(tx);
        obs.on_task_complete(&ResourceKey::new("space", "a"), true, std::time::Duration::from_secs(1), None);
        let ev = rx.try_recv().expect("event sent");
        assert_eq!(ev.done, 1.0);
        assert!(ev.message.contains("ok space/a"));
    }
}
