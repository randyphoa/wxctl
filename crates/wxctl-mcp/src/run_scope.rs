//! Per-tool-call run-record scope for the MCP server. The `wxctl` binary's global
//! tracing subscriber (installed in `main.rs`) already carries `RunRecordLayer`;
//! `wxctl mcp serve` runs inside that binary, so all we do here is install a fresh
//! sink (global slot) + open a root `run` span for the duration of one mutating
//! tool call. Dropping the scope clears the slot; `finish` finalizes the manifest.
//!
//! # Span approach: `tracing::Span` + `Instrument` (not `EnteredSpan`)
//!
//! `EnteredSpan` is `!Send`. rmcp tool futures must be `Send` (tokio multi-thread
//! runtime). Storing an `EnteredSpan` across `.await` points would break Send bounds
//! and mis-scope the span. Instead, `McpRunScope` stores an un-entered `tracing::Span`
//! (`Clone + Send`) and exposes it via `span()`. Callers wrap the awaited client call
//! with `.instrument(scope.span())` from `tracing::Instrument`.

use std::sync::Arc;
use wxctl_core::logging::run_record::{RunCounts, RunManifest, RunSink, RunSinkGuard, generate_run_id, install_run_sink, set_full_trace, utc_now_string};

/// Holds the live sink + its install guard + an un-entered `run` span for one tool
/// call. `run_id` is surfaced into the tool's output so the agent can `run_diagnose`.
pub struct McpRunScope {
    pub run_id: String,
    sink: Arc<RunSink>,
    _guard: RunSinkGuard,
    span: tracing::Span,
}

impl McpRunScope {
    /// Open a run scope for an MCP tool call. `command` is the tool name without the
    /// `wxctl_` prefix (e.g. `apply`). Best-effort: a sink that fails to create
    /// falls back to the null sink (observability never breaks the call).
    pub fn begin(command: &str, profile: &str, config_paths: Vec<String>, full_trace: bool) -> Self {
        set_full_trace(full_trace);
        let run_id = generate_run_id(command);
        let manifest = RunManifest {
            run_id: run_id.clone(),
            command: command.to_string(),
            args: vec![format!("mcp:{command}")],
            profile: Some(profile.to_string()),
            deployment: None,
            config_paths,
            started: utc_now_string(),
            finished: None,
            outcome: None,
            counts: RunCounts::default(),
            errors: Vec::new(),
            full_trace,
            record_incomplete: false,
        };
        let sink = Arc::new(RunSink::new(manifest).unwrap_or_else(RunSink::null));
        let guard = install_run_sink(sink.clone());
        // Store an un-entered span; callers use `.instrument(scope.span())` to scope
        // the async future correctly without crossing Send boundaries.
        let span = tracing::info_span!(target: "wxctl::stage::run", "run", run_id = %run_id, command = %command, profile = %profile, full_trace = full_trace);
        Self { run_id, sink, _guard: guard, span }
    }

    /// Clone the span for use with `tracing::Instrument`. The span is un-entered here;
    /// the instrumented future enters it for the duration of its execution.
    pub fn span(&self) -> tracing::Span {
        self.span.clone()
    }

    /// Record the run's deployment (`saas` / `software`) once the profile is resolved —
    /// `begin` predates the live client, so this lands late but before `finish`.
    pub fn set_deployment(&self, deployment: Option<String>) {
        self.sink.set_deployment(deployment);
    }

    /// Finalize the manifest with a definite outcome (`success` | `failed`).
    pub fn finish(&self, outcome: &str) {
        self.sink.finalize(outcome);
    }
}
