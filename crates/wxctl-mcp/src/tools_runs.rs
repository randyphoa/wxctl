//! DTOs + backing logic for the four read-only diagnose tools (`runs_list`,
//! `run_get`, `run_events_query`, `run_diagnose`). Each wraps the
//! `wxctl_core::diagnose` API (Phase 2) — no profile, no network, no artifact
//! mutation. Every output type advertises a root `type: object` JSON Schema
//! (rmcp requirement): the two that surface `serde_json::Value` use a transparent
//! object-schema newtype, the plain structs derive `JsonSchema`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use wxctl_core::diagnose::{EventLine, RunArtifact, build_bundle, find_latest_failed, list_runs, load_artifact};

/// Input for `runs_list`: an optional `outcome` filter + an optional `limit`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunsListInput {
    /// Show only runs with this outcome: `success` | `failed` | `aborted` | `unknown`.
    #[serde(default)]
    pub outcome: Option<String>,
    /// Cap the number of runs returned (newest first). Default: all.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One row in `runs_list`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RunRow {
    pub run_id: String,
    pub command: String,
    pub started: String,
    pub outcome: String,
    pub error_count: usize,
}

/// Output for `runs_list`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RunsListOutput {
    pub runs: Vec<RunRow>,
}

/// Input for `run_get`.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunGetInput {
    /// Run id (see `runs_list`).
    pub run_id: String,
}

/// Transparent wrapper around the manifest JSON. Serializes to exactly the inner
/// value but advertises a root `type: object` schema (rmcp requirement).
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct RunGetOutput(pub serde_json::Value);

impl JsonSchema for RunGetOutput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RunGetOutput".into()
    }
    fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "Run-record manifest: command, args, profile, deployment, config_paths, started/finished, outcome, counts, and the error index [{code, resource, message, fix}]."
        })
    }
}

/// Input for `run_events_query`: a run id + optional filters.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunEventsQueryInput {
    /// Run id (see `runs_list`).
    pub run_id: String,
    /// Filter to events at this level (case-insensitive): `ERROR` | `WARN` | `INFO` | `DEBUG` | `TRACE`.
    #[serde(default)]
    pub level: Option<String>,
    /// Filter to events whose `target` starts with this prefix (e.g. `wxctl::error`, `wxctl::substage::http`).
    #[serde(default)]
    pub target: Option<String>,
    /// Filter to events touching this resource (`<type>.<name>` or a substring of either field).
    #[serde(default)]
    pub resource: Option<String>,
    /// Filter to events whose `span` path contains this substring (e.g. `execution`).
    #[serde(default)]
    pub span: Option<String>,
    /// Cap the number of events returned (in file order). Default: 200.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One event in `run_events_query` output — the `EventLine` envelope + its fields.
#[derive(Debug, Serialize, JsonSchema)]
pub struct EventRow {
    pub ts: String,
    pub level: String,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    /// All remaining recorded fields (error_code, message, fix, decision, …).
    pub fields: std::collections::BTreeMap<String, String>,
}

/// Output for `run_events_query`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RunEventsQueryOutput {
    pub run_id: String,
    /// Number of events that matched the filters (before `limit` truncation).
    pub matched: usize,
    pub events: Vec<EventRow>,
}

/// Input for `run_diagnose`: an optional run id (defaults to the latest failed run).
#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunDiagnoseInput {
    /// Run id to diagnose. Omit to diagnose the most recent failed/aborted run.
    #[serde(default)]
    pub run_id: Option<String>,
}

/// Transparent wrapper around the diagnosis-bundle JSON (`DiagnosisBundle::render_json`).
/// Object-root schema for rmcp.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub struct RunDiagnoseOutput(pub serde_json::Value);

impl JsonSchema for RunDiagnoseOutput {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "RunDiagnoseOutput".into()
    }
    fn json_schema(_g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "type": "object",
            "description": "Agent-ready diagnosis bundle: run_id, command, outcome, full_trace, config_paths, errors[{error_code, resource, message, cause, fix, triage, triage_guidance, exchange, error_chain, preceding, span, src}], troubleshoot[], fix_instructions. Same JSON as `wxctl debug -o json`."
        })
    }
}

/// `runs_list` — list run records newest-first, optionally filtered + capped.
pub fn runs_list(input: &RunsListInput) -> Result<RunsListOutput, String> {
    let mut rows: Vec<RunRow> = list_runs().into_iter().filter(|r| input.outcome.as_deref().is_none_or(|o| r.outcome == o)).map(|r| RunRow { run_id: r.run_id, command: r.command, started: r.started, outcome: r.outcome, error_count: r.error_count }).collect();
    if let Some(n) = input.limit {
        rows.truncate(n);
    }
    Ok(RunsListOutput { runs: rows })
}

/// `run_get` — the run's manifest as JSON. Unknown/corrupt run → actionable `Err`.
pub fn run_get(input: &RunGetInput) -> Result<RunGetOutput, String> {
    let art = load_artifact(&input.run_id).map_err(|e| format!("{e:#}"))?;
    let value = serde_json::to_value(&art.manifest).map_err(|e| format!("manifest serialization error: {e}"))?;
    Ok(RunGetOutput(value))
}

/// Whether an event matches all provided filters.
fn matches(ev: &EventLine, input: &RunEventsQueryInput) -> bool {
    if let Some(level) = &input.level
        && !ev.level.eq_ignore_ascii_case(level)
    {
        return false;
    }
    if let Some(target) = &input.target
        && !ev.target.starts_with(target.as_str())
    {
        return false;
    }
    if let Some(span) = &input.span
        && !ev.span.as_deref().is_some_and(|s| s.contains(span.as_str()))
    {
        return false;
    }
    if let Some(resource) = &input.resource {
        let combined = match (ev.get("resource_type"), ev.get("resource_name")) {
            (Some(t), Some(n)) => format!("{t}.{n}"),
            _ => String::new(),
        };
        let hit = combined.contains(resource.as_str()) || ev.get("resource_type").is_some_and(|t| t.contains(resource.as_str())) || ev.get("resource_name").is_some_and(|n| n.contains(resource.as_str()));
        if !hit {
            return false;
        }
    }
    true
}

/// `run_events_query` — filtered slice of a run's events.jsonl.
pub fn run_events_query(input: &RunEventsQueryInput) -> Result<RunEventsQueryOutput, String> {
    let art: RunArtifact = load_artifact(&input.run_id).map_err(|e| format!("{e:#}"))?;
    let matched_events: Vec<&EventLine> = art.events.iter().filter(|ev| matches(ev, input)).collect();
    let matched = matched_events.len();
    let limit = input.limit.unwrap_or(200);
    let events = matched_events.into_iter().take(limit).map(|ev| EventRow { ts: ev.ts.clone(), level: ev.level.clone(), target: ev.target.clone(), span: ev.span.clone(), src: ev.src.clone(), fields: ev.fields.clone() }).collect();
    Ok(RunEventsQueryOutput { run_id: input.run_id.clone(), matched, events })
}

/// `run_diagnose` — bundle for one run (or the latest failed run when `run_id` is omitted).
pub fn run_diagnose(input: &RunDiagnoseInput) -> Result<RunDiagnoseOutput, String> {
    let run_id = match &input.run_id {
        Some(id) => id.clone(),
        None => find_latest_failed().ok_or_else(|| {
            let available = list_runs();
            if available.is_empty() {
                "no run records found. Run wxctl_apply/destroy/test first; failed runs are diagnosable here.".to_string()
            } else {
                format!("no failed or aborted run found. Pass an explicit run_id (see runs_list); most recent: {}", available.first().map(|r| r.run_id.as_str()).unwrap_or("-"))
            }
        })?,
    };
    let art = load_artifact(&run_id).map_err(|e| format!("{e:#}"))?;
    let bundle = build_bundle(&art);
    Ok(RunDiagnoseOutput(bundle.render_json()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_core::logging::run_record::{ManifestError, RunCounts, RunManifest, RunSink, generate_run_id, utc_now_string};

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    #[test]
    fn diagnose_tools_over_seeded_failed_run() {
        let _env = env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-mcp-runs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("WXCTL_RUNS_DIR", &tmp) };

        let run_id = generate_run_id("apply");
        let manifest = RunManifest {
            run_id: run_id.clone(),
            command: "apply".into(),
            args: vec!["mcp:apply".into()],
            profile: Some("itz".into()),
            deployment: None,
            config_paths: vec!["inline".into()],
            started: utc_now_string(),
            finished: None,
            outcome: None,
            counts: RunCounts::default(),
            errors: vec![],
            full_trace: false,
            record_incomplete: false,
        };
        let sink = RunSink::new(manifest).unwrap();
        sink.write_event(r#"{"ts":"t","level":"INFO","target":"wxctl::decision","span":"run>reconciliation","resource_type":"space","resource_name":"dev","decision":"create","reason":"absent"}"#);
        sink.write_event(r#"{"ts":"t","level":"ERROR","target":"wxctl::error","span":"run>execution","src":"crates/x.rs:1","error_code":"WXCTL-H001","resource_type":"space","resource_name":"dev","message":"HTTP 404 not found","fix":"check the instance guid","context":"{\"request_body\":{\"name\":\"dev\"},\"response_body\":{\"error\":\"not found\"}}"}"#);
        sink.add_error(ManifestError { code: "WXCTL-H001".into(), resource: Some("space.dev".into()), message: "HTTP 404 not found".into(), fix: Some("check the instance guid".into()) });
        sink.finalize("failed");
        drop(sink);

        let listed = runs_list(&RunsListInput { outcome: None, limit: None }).unwrap();
        assert!(listed.runs.iter().any(|r| r.run_id == run_id && r.outcome == "failed" && r.error_count == 1));
        let failed_only = runs_list(&RunsListInput { outcome: Some("failed".into()), limit: Some(10) }).unwrap();
        assert!(failed_only.runs.iter().all(|r| r.outcome == "failed"));

        let got = run_get(&RunGetInput { run_id: run_id.clone() }).unwrap();
        assert_eq!(got.0.get("command").and_then(|v| v.as_str()), Some("apply"));
        assert_eq!(got.0.get("errors").and_then(|v| v.as_array()).map(|a| a.len()), Some(1));

        let errs = run_events_query(&RunEventsQueryInput { run_id: run_id.clone(), level: Some("error".into()), target: None, resource: None, span: None, limit: None }).unwrap();
        assert_eq!(errs.matched, 1);
        assert_eq!(errs.events[0].fields.get("error_code").map(String::as_str), Some("WXCTL-H001"));
        let by_resource = run_events_query(&RunEventsQueryInput { run_id: run_id.clone(), level: None, target: None, resource: Some("space.dev".into()), span: None, limit: None }).unwrap();
        assert_eq!(by_resource.matched, 2, "both events touch space.dev");
        let by_target = run_events_query(&RunEventsQueryInput { run_id: run_id.clone(), level: None, target: Some("wxctl::decision".into()), resource: None, span: None, limit: None }).unwrap();
        assert_eq!(by_target.matched, 1);

        let diag = run_diagnose(&RunDiagnoseInput { run_id: Some(run_id.clone()) }).unwrap();
        assert_eq!(diag.0.get("run_id").and_then(|v| v.as_str()), Some(run_id.as_str()));
        let code = diag.0.get("errors").and_then(|v| v.as_array()).and_then(|a| a.first()).and_then(|e| e.get("error_code")).and_then(|v| v.as_str());
        assert_eq!(code, Some("WXCTL-H001"));
        let diag_latest = run_diagnose(&RunDiagnoseInput { run_id: None }).unwrap();
        assert_eq!(diag_latest.0.get("run_id").and_then(|v| v.as_str()), Some(run_id.as_str()));

        // Error path: run_get on a non-existent run id is actionable, even with a populated dir.
        let err = run_get(&RunGetInput { run_id: "nope".into() }).unwrap_err();
        assert!(err.contains("no run record found"), "actionable: {err}");

        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var("WXCTL_RUNS_DIR") };
    }
}
