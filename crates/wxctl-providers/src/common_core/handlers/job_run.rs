use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec, error_matches};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::job::{env_pairs_to_strings, job_env_sensitive_paths};
use crate::util::{IDENTITY_ENV_KEY, REF_PREFIX, extract_identity_env_marker, job_run_state_rank};

pub struct JobRunHandler;

const DONE_STATES: &[&str] = &["completed"];
const FAILED_STATES: &[&str] = &["failed", "canceled", "completedwitherrors"];

fn matches_status(status: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|s| s.eq_ignore_ascii_case(status))
}

/// Poll bound in seconds: WXCTL_JOB_RUN_TIMEOUT overrides the 40-minute default.
fn run_timeout_secs() -> u64 {
    std::env::var("WXCTL_JOB_RUN_TIMEOUT").ok().and_then(|v| v.parse::<u64>().ok()).filter(|s| *s > 0).unwrap_or(2400)
}

/// Append the optional `project_id` query param from the resource.
fn with_project(mut spec: RequestSpec, resource: &Value) -> RequestSpec {
    if let Some(p) = resource.get("project_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("project_id", p);
    }
    spec
}

/// Run status lives at entity.job_run.state on the run GET (verified live in Phase 5);
/// fall back to metadata.state / top-level state.
fn extract_state(run: &Value) -> Option<String> {
    run.pointer("/entity/job_run/state").or_else(|| run.pointer("/metadata/state")).or_else(|| run.get("state")).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Extract the run id from a submit / get response (CAMS nests it under metadata).
fn extract_run_id(run: &Value) -> Option<String> {
    run.pointer("/metadata/asset_id").or_else(|| run.pointer("/metadata/id")).or_else(|| run.get("id")).and_then(|v| v.as_str()).map(|s| s.to_string())
}

/// Job-level env_variables as "KEY=value" wire strings, read from a job value in
/// either the CAMS envelope shape (`entity.job.configuration.env_variables` —
/// create responses and discovery entries) or hoisted (`configuration.env_variables`).
/// Entries may already be wire strings (server echo) or {name, value} objects
/// (declared spec) — both normalize to strings.
fn extract_job_env_strings(job_value: &Value) -> Vec<String> {
    let Some(arr) = job_value.pointer("/entity/job/configuration/env_variables").or_else(|| job_value.pointer("/configuration/env_variables")).and_then(|v| v.as_array()) else {
        return Vec::new();
    };
    arr.iter()
        .flat_map(|entry| match entry {
            Value::String(s) => vec![s.clone()],
            Value::Object(_) => env_pairs_to_strings(std::slice::from_ref(entry)),
            _ => Vec::new(),
        })
        .collect()
}

/// The effective env set for a run submit. CAMS applies run-level
/// `configuration.env_variables` as a FULL OVERRIDE of the job's — not a merge
/// (live-pinned 2026-07-06, both CPDaaS and CP4D: a marker-only run array erased
/// the job's TRAINING_* vars and train.py crashed with `KeyError`) — so the
/// submitted array must carry the union: job-level entries first, run-level
/// entries (including the WXCTL_IDENTITY marker) winning on key collision. A
/// stray job-level WXCTL_IDENTITY is dropped — the marker identifies the RUN.
fn merge_env_strings(job_env: Vec<String>, run_env: Vec<String>) -> Vec<String> {
    let run_keys: std::collections::HashSet<&str> = run_env.iter().filter_map(|s| s.split_once('=').map(|(k, _)| k)).collect();
    let mut merged: Vec<String> = job_env.into_iter().filter(|s| s.split_once('=').is_some_and(|(k, _)| k != IDENTITY_ENV_KEY && !run_keys.contains(k))).collect();
    merged.extend(run_env);
    merged
}

/// Pre-submit match outcome for runs already carrying this run's identity marker.
#[derive(Debug)]
pub(crate) enum RunMatch {
    /// A Completed run exists — adopt it, no new submit.
    Completed(Value),
    /// A run is still in flight — poll it to a terminal state instead of submitting.
    Active(Value),
    /// No run, or only Failed/Canceled/CompletedWithErrors runs — submit fresh.
    NoMatch,
}

/// Classify the list-response `results[]` entries whose round-tripped
/// `configuration.env_variables` carries `WXCTL_IDENTITY=<hash>` for exactly this
/// hash. Names are deliberately ignored: both CPDaaS and CP4D clobber the
/// submitted run name to "Notebook Job" (live-pinned 2026-07-05), so a
/// name-based match can never hit — while the configuration round-trips
/// verbatim. The list may hold prior generations (different markers — excluded
/// by the exact-hash filter) and duplicate runs with the SAME marker (residue
/// of the pre-fix create-loop bug). Preference order: any Completed run wins
/// (stable adopt), else any non-terminal run (tail it), else NoMatch (failed
/// runs are resubmitted — a changed input would have changed the hash anyway).
pub(crate) fn classify_runs_by_marker(list_response: &Value, hash: &str) -> RunMatch {
    let Some(results) = list_response.get("results").and_then(|v| v.as_array()) else {
        return RunMatch::NoMatch;
    };
    let best = results.iter().filter(|item| extract_identity_env_marker(item) == Some(hash)).min_by_key(|item| job_run_state_rank(item));
    match best {
        Some(run) if job_run_state_rank(run) == 0 => RunMatch::Completed(run.clone()),
        Some(run) if job_run_state_rank(run) == 1 => RunMatch::Active(run.clone()),
        _ => RunMatch::NoMatch,
    }
}

/// Hoist `id` (+ `state` when known) top-level on an adopted/terminal run value
/// so downstream `${job_run.x.id}` refs and state comparison resolve.
fn hoist_id_state(mut run: Value, run_id: &str) -> Value {
    let state = extract_state(&run);
    if let Some(obj) = run.as_object_mut() {
        obj.insert("id".to_string(), json!(run_id));
        if let Some(s) = state {
            obj.insert("state".to_string(), json!(s));
        }
    }
    run
}

impl ResourceHandler for JobRunHandler {
    /// Own the full create: POST the run, then poll to a terminal state. The materializer
    /// only serialises declared Body fields (job_run has none — `job` is Path, `env_variables`
    /// and `generation` are LocalOnly), so the submit body is built here and posted directly,
    /// returning Handled so the engine skips its default POST.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let job = resource.get("job").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] job_run requires job (parent job id)"))?.to_string();
            let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] job_run requires name"))?.to_string();
            // Stamped at validation (HashStorage::EnvMarker) alongside the injected
            // WXCTL_IDENTITY env entry; synthetic, never part of any API body.
            let identity_hash = resource.get("identity_hash").and_then(|v| v.as_str()).map(str::to_string);

            // Pre-submit run match (idempotency backstop for the CreateUnchecked
            // path, where reconciliation couldn't discover the run ahead of the
            // create): the server clobbers submitted run names to "Notebook Job"
            // (both CPDaaS and CP4D, live-pinned 2026-07-05), so an existing run
            // is recognized by the WXCTL_IDENTITY=<hash8> marker round-tripped in
            // its configuration.env_variables — never by name. Completed → adopt;
            // still in flight → tail-poll it to a terminal state instead of
            // submitting a duplicate; Failed (or list unavailable) → fall through
            // and submit fresh.
            if let Some(hash) = &identity_hash {
                let list_spec = with_project(RequestSpec::new(Method::GET, format!("/v2/jobs/{job}/runs")).body(BodyKind::None).sensitive_paths(job_env_sensitive_paths()), resource);
                if let Ok(list) = client.execute::<Value>(operation_id, list_spec).await {
                    match classify_runs_by_marker(&list, hash) {
                        RunMatch::Completed(run) => {
                            let run_id = extract_run_id(&run).ok_or_else(|| anyhow!("[{operation_id}] matched completed job_run (identity {hash}) but no run id in the list entry: {run}"))?;
                            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job_run", run_id = %run_id, identity_hash = %hash, "adopt: completed run with matching identity marker — skipping submit");
                            return Ok(HookOutcome::Handled(hoist_id_state(run, &run_id)));
                        }
                        RunMatch::Active(run) => {
                            let run_id = extract_run_id(&run).ok_or_else(|| anyhow!("[{operation_id}] matched in-flight job_run (identity {hash}) but no run id in the list entry: {run}"))?;
                            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job_run", run_id = %run_id, identity_hash = %hash, "in-flight run with matching identity marker — polling it instead of submitting");
                            let terminal = wait_for_run_terminal(client, &job, &run_id, resource, operation_id).await?;
                            return Ok(HookOutcome::Handled(hoist_id_state(terminal, &run_id)));
                        }
                        RunMatch::NoMatch => {}
                    }
                }
            }

            let mut configuration = json!({});
            // Convert {name, value} objects to "NAME=value" strings — the live API
            // rejects the object shape (400 "/job/configuration/env_variables/0 must
            // be string"), verified on both SaaS and CP4D 2026-07-05. See
            // job::env_pairs_to_strings for details. The array already carries the
            // WXCTL_IDENTITY marker injected at validation; re-add it if the local
            // data was rebuilt without it (belt and braces — the submitted run must
            // always carry its identity or every re-apply creates a duplicate).
            let mut env_strings = resource.get("env_variables").and_then(|v| v.as_array()).map(|envs| env_pairs_to_strings(envs)).unwrap_or_default();
            if let Some(hash) = &identity_hash
                && !env_strings.iter().any(|s| s.strip_prefix(IDENTITY_ENV_KEY).is_some_and(|rest| rest.starts_with('=')))
            {
                env_strings.push(format!("{IDENTITY_ENV_KEY}={hash}"));
            }
            // Merge in the parent job's env_variables (see merge_env_strings: the
            // server treats a non-empty run-level array as a full override, so a
            // marker-only submit would erase the job's env at runtime). Primary
            // source is the engine's `__ref__job` enrichment (the job's stored
            // create-response/discovery state, secrets resolved); a live GET is
            // the fallback when the enrichment key is absent. Enrichment present
            // but env-less = the job genuinely declares none — nothing to merge.
            let job_env = match resource.get(format!("{REF_PREFIX}job")) {
                Some(job_value) => extract_job_env_strings(job_value),
                None => {
                    let get_spec = with_project(RequestSpec::new(Method::GET, format!("/v2/jobs/{job}")).body(BodyKind::None).sensitive_paths(job_env_sensitive_paths()), resource);
                    match client.execute::<Value>(operation_id, get_spec).await {
                        Ok(job_value) => extract_job_env_strings(&job_value),
                        Err(e) => {
                            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job_run", job = %job, error = %e, "job GET for env merge failed — submitting run-level env only");
                            Vec::new()
                        }
                    }
                }
            };
            let env_strings = merge_env_strings(job_env, env_strings);
            if !env_strings.is_empty() {
                configuration["env_variables"] = json!(env_strings);
            }
            // The name is submitted for display parity but the server discards it
            // (see above) — identity rides the env marker.
            let body = json!({"job_run": {"name": name, "configuration": configuration}});
            let submit_spec = with_project(RequestSpec::new(Method::POST, format!("/v2/jobs/{job}/runs")).body(BodyKind::Json(body)).sensitive_paths(job_env_sensitive_paths()), resource);
            let resp: Value = client.execute(operation_id, submit_spec).await.map_err(|e| anyhow!("[{operation_id}] job_run submit failed (job={job}): {e}"))?;

            let run_id = extract_run_id(&resp).ok_or_else(|| anyhow!("[{operation_id}] no run id in submit response: {}", serde_json::to_string_pretty(&resp).unwrap_or_default()))?;
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job_run", job = %job, run_id = %run_id, "submitted job run; polling to terminal state");

            let mut terminal = wait_for_run_terminal(client, &job, &run_id, resource, operation_id).await?;
            let state = extract_state(&terminal);
            if let Some(obj) = terminal.as_object_mut() {
                obj.insert("id".to_string(), json!(run_id));
                if let Some(s) = state {
                    obj.insert("state".to_string(), json!(s));
                }
            }
            Ok(HookOutcome::Handled(terminal))
        })
    }

    /// Expose id/state top-level on discovery so downstream refs resolve, and on the apply
    /// path tail a still-running run to a terminal state so a re-apply doesn't return mid-run.
    /// Mirrors autoai_experiment.post_discover.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, client: &'a HttpClient, operation_id: &'a str, is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let state = extract_state(remote_data).unwrap_or_default();
            let run_id = extract_run_id(remote_data);
            if let (Some(id), Some(obj)) = (run_id.clone(), remote_data.as_object_mut()) {
                obj.insert("id".to_string(), json!(id));
                if !state.is_empty() {
                    obj.insert("state".to_string(), json!(state.clone()));
                }
            }
            if !is_apply || matches_status(&state, DONE_STATES) || matches_status(&state, FAILED_STATES) {
                return Ok(());
            }
            let (Some(job), Some(run_id)) = (remote_data.get("job").and_then(|v| v.as_str()).map(|s| s.to_string()), run_id) else {
                return Ok(());
            };
            let terminal = wait_for_run_terminal(client, &job, &run_id, remote_data, operation_id).await?;
            if let (Some(s), Some(obj)) = (extract_state(&terminal), remote_data.as_object_mut()) {
                obj.insert("state".to_string(), json!(s));
            }
            Ok(())
        })
    }

    /// Destroy deletes the run; 404 is tolerated (the run may have cascade-deleted with its
    /// parent job). pre_delete owns the DELETE so the tolerance is explicit.
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let (Some(job), Some(run_id)) = (resource.get("job").and_then(|v| v.as_str()), resource.get("id").and_then(|v| v.as_str())) else {
                return Ok(HookOutcome::Handled(json!({})));
            };
            let spec = with_project(RequestSpec::new(Method::DELETE, format!("/v2/jobs/{job}/runs/{run_id}")).body(BodyKind::None), resource);
            match client.execute::<Value>(operation_id, spec).await {
                Ok(_) => Ok(HookOutcome::Handled(json!({}))),
                Err(e) if error_matches(&e, 404, &[]) => {
                    tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job_run", run_id = %run_id, "run already absent on delete (404 tolerated)");
                    Ok(HookOutcome::Handled(json!({})))
                }
                Err(e) => Err(anyhow!("[{operation_id}] job_run delete failed (job={job}, run_id={run_id}): {e}")),
            }
        })
    }
}

/// Poll GET /v2/jobs/{job}/runs/{run_id} until a terminal state. On Failed/Canceled/
/// CompletedWithErrors, fold the run's log tail into the failure detail. Bound by
/// run_timeout_secs() (default 40 min, 15 s interval).
async fn wait_for_run_terminal(client: &HttpClient, job: &str, run_id: &str, resource: &Value, operation_id: &str) -> Result<Value> {
    let interval = Duration::from_secs(15);
    let max_attempts = (run_timeout_secs() / interval.as_secs()).max(1) as u32;
    let job = job.to_string();
    let run_id = run_id.to_string();
    let operation_id = operation_id.to_string();
    let project_id = resource.get("project_id").and_then(|v| v.as_str()).map(|s| s.to_string());

    crate::util::poll_until(max_attempts, interval, crate::util::PollTimeout::Bail(format!("[{operation_id}] timed out ({} min) waiting for job_run {run_id} to reach a terminal state", run_timeout_secs() / 60)), None::<String>, move |attempt, mut prev_state| {
        let job = job.clone();
        let run_id = run_id.clone();
        let operation_id = operation_id.clone();
        let project_id = project_id.clone();
        async move {
            let mut path = format!("/v2/jobs/{job}/runs/{run_id}");
            if let Some(p) = &project_id {
                path.push_str(&format!("?project_id={p}"));
            }
            // Poll GET responses echo configuration.env_variables too — same redaction
            // coverage as the submit/list specs.
            let resp: Value = client.execute(&operation_id, RequestSpec::new(Method::GET, &path).body(BodyKind::None).sensitive_paths(job_env_sensitive_paths())).await?;
            let state = extract_state(&resp).unwrap_or_else(|| "unknown".to_string());
            if prev_state.as_deref() != Some(state.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job_run", run_id = %run_id, status = %state, attempt = attempt, max_attempts = max_attempts, "job_run status observed");
                prev_state = Some(state.clone());
            }
            let outcome = if matches_status(&state, DONE_STATES) {
                crate::util::PollOutcome::Done(resp.clone())
            } else if matches_status(&state, FAILED_STATES) {
                let logs = fetch_log_tail(client, &job, &run_id, project_id.as_deref(), &operation_id).await;
                crate::util::PollOutcome::Failed(format!("[{operation_id}] job_run {run_id} {state}{logs}"))
            } else {
                crate::util::PollOutcome::Pending
            };
            Ok((outcome, prev_state))
        }
    })
    .await
}

/// Best-effort fetch of the run's last log lines to fold into a failure message. Returns a
/// "; last logs: …" suffix (empty on any error). Log shape verified live in Phase 5.
async fn fetch_log_tail(client: &HttpClient, job: &str, run_id: &str, project_id: Option<&str>, operation_id: &str) -> String {
    let mut path = format!("/v2/jobs/{job}/runs/{run_id}/logs");
    if let Some(p) = project_id {
        path.push_str(&format!("?project_id={p}"));
    }
    let Ok(resp) = client.execute::<Value>(operation_id, RequestSpec::new(Method::GET, &path).body(BodyKind::None)).await else {
        return String::new();
    };
    let lines: Vec<String> = resp.get("results").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|l| l.as_str().map(str::to_string)).collect()).unwrap_or_default();
    if lines.is_empty() {
        return String::new();
    }
    let tail: Vec<String> = lines.iter().rev().take(20).rev().cloned().collect();
    format!("; last logs: {}", tail.join(" | "))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A runs-LIST item in the live CAMS shape (evidence 2026-07-05, both
    /// deployments): the name is ALWAYS the server-clobbered "Notebook Job";
    /// identity rides the WXCTL_IDENTITY entry in the round-tripped
    /// configuration.env_variables.
    fn run_item(hash: &str, id: &str, state: &str) -> Value {
        json!({"metadata": {"name": "Notebook Job", "asset_id": id}, "entity": {"job_run": {"name": "Notebook Job", "state": state, "configuration": {"env_variables": ["TRAINING_MODEL_NAME=m", format!("{IDENTITY_ENV_KEY}={hash}")]}}}})
    }

    // classify_runs_by_marker: exact-hash filter on the env marker (names are all
    // "Notebook Job" and must be irrelevant), then Completed > Active > NoMatch.
    #[test]
    fn classify_runs_by_marker_prefers_completed_then_active() {
        let hash = "abcd1234";

        // A Completed run wins even when an in-flight one carries the same marker
        // (duplicate residue of the pre-fix create-loop); a completed run from a
        // DIFFERENT generation (other marker) never matches.
        let list = json!({"results": [run_item(hash, "r-1", "Running"), run_item(hash, "r-2", "Completed"), run_item("99999999", "r-3", "Completed")]});
        match classify_runs_by_marker(&list, hash) {
            RunMatch::Completed(run) => assert_eq!(run.pointer("/metadata/asset_id").and_then(|v| v.as_str()), Some("r-2")),
            other => panic!("expected Completed, got {other:?}"),
        }

        // No Completed → the non-terminal run is tailed.
        let list = json!({"results": [run_item(hash, "r-4", "Failed"), run_item(hash, "r-5", "Starting")]});
        match classify_runs_by_marker(&list, hash) {
            RunMatch::Active(run) => assert_eq!(run.pointer("/metadata/asset_id").and_then(|v| v.as_str()), Some("r-5")),
            other => panic!("expected Active, got {other:?}"),
        }

        // Only Failed/Canceled matches (or none at all) → submit fresh; a run with
        // NO marker (pre-fix legacy run) never matches either.
        assert!(matches!(classify_runs_by_marker(&json!({"results": [run_item(hash, "r-6", "Failed"), run_item(hash, "r-7", "Canceled")]}), hash), RunMatch::NoMatch));
        assert!(matches!(classify_runs_by_marker(&json!({"results": [run_item("99999999", "r-8", "Completed")]}), hash), RunMatch::NoMatch));
        assert!(matches!(classify_runs_by_marker(&json!({"results": [{"metadata": {"name": "Notebook Job", "asset_id": "r-9"}, "entity": {"job_run": {"state": "Completed", "configuration": {}}}}]}), hash), RunMatch::NoMatch));
        assert!(matches!(classify_runs_by_marker(&json!({"results": []}), hash), RunMatch::NoMatch));
        assert!(matches!(classify_runs_by_marker(&json!({}), hash), RunMatch::NoMatch));
    }

    // extract_job_env_strings reads both the CAMS envelope (server echo, wire
    // strings) and the hoisted/declared shapes ({name,value} objects); absent → empty.
    #[test]
    fn extract_job_env_strings_reads_envelope_and_hoisted_shapes() {
        let envelope = json!({"entity": {"job": {"configuration": {"env_variables": ["A=1", "B=2"]}}}});
        assert_eq!(extract_job_env_strings(&envelope), vec!["A=1", "B=2"]);

        let hoisted_objects = json!({"configuration": {"env_variables": [{"name": "A", "value": "1"}, {"name": "B", "value": "2"}]}});
        assert_eq!(extract_job_env_strings(&hoisted_objects), vec!["A=1", "B=2"]);

        assert!(extract_job_env_strings(&json!({"entity": {"job": {}}})).is_empty());
        assert!(extract_job_env_strings(&json!({})).is_empty());
    }

    // merge_env_strings: the run submit must carry the job's env too (server
    // treats run-level env_variables as a full override — live-pinned KeyError),
    // with run-level entries and the identity marker winning on collision and a
    // stray job-level marker dropped.
    #[test]
    fn merge_env_strings_unions_job_env_with_run_overrides() {
        let job_env = vec!["TRAINING_MODEL_NAME=m".to_string(), "TRAINING_APIKEY=k".to_string(), format!("{IDENTITY_ENV_KEY}=stale")];
        let run_env = vec!["TRAINING_APIKEY=override".to_string(), format!("{IDENTITY_ENV_KEY}=abcd1234")];
        assert_eq!(merge_env_strings(job_env, run_env), vec!["TRAINING_MODEL_NAME=m", "TRAINING_APIKEY=override", &format!("{IDENTITY_ENV_KEY}=abcd1234")]);

        // Marker-only run + job env: the exact live-failure shape — job vars must survive.
        let merged = merge_env_strings(vec!["TRAINING_MODEL_NAME=m".to_string()], vec![format!("{IDENTITY_ENV_KEY}=abcd1234")]);
        assert_eq!(merged, vec!["TRAINING_MODEL_NAME=m", &format!("{IDENTITY_ENV_KEY}=abcd1234")]);

        // No job env: run-level set passes through unchanged.
        assert_eq!(merge_env_strings(vec![], vec!["X=1".to_string()]), vec!["X=1"]);
    }

    // hoist_id_state exposes id + state top-level for downstream refs/compare.
    #[test]
    fn hoist_id_state_exposes_top_level_fields() {
        let hoisted = hoist_id_state(run_item("abcd1234", "r-1", "Completed"), "r-1");
        assert_eq!(hoisted.get("id").and_then(|v| v.as_str()), Some("r-1"));
        assert_eq!(hoisted.get("state").and_then(|v| v.as_str()), Some("Completed"));
        assert_eq!(hoisted.pointer("/entity/job_run/state").and_then(|v| v.as_str()), Some("Completed"));
    }

    // Redaction: a job_run LIST/GET/201 response body logged through the normal
    // emission path (http.rs applies redact_for_log(resp, spec.sensitive_paths);
    // every handler-owned job/job_run spec passes job_env_sensitive_paths()) must
    // mask the round-tripped env_variables values. Live-shaped bodies: a plaintext
    // TRAINING_APIKEY reached the WXCTL_LOG_PATH sink this way (2026-07-05, SaaS).
    #[test]
    fn job_run_response_env_variables_redacted_in_logged_bodies() {
        let paths = job_env_sensitive_paths();

        // runs-LIST shape: results[].entity.job_run.configuration.env_variables.
        let list = json!({"total_rows": 1, "results": [{"metadata": {"name": "Notebook Job", "asset_id": "r-1"}, "entity": {"job_run": {"state": "Completed", "configuration": {"env_variables": ["TRAINING_APIKEY=SEEDED-SECRET", "WXCTL_IDENTITY=abcd1234"]}}}}]});
        let logged = wxctl_core::logging::redact_for_log(&list, &paths);
        let s = serde_json::to_string(&logged).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "LIST response leaked env_variables: {s}");
        assert!(s.contains("Notebook Job"), "non-sensitive fields retained: {s}");

        // 201/GET shape: entity.job_run.configuration.env_variables (no results wrapper).
        let get = json!({"metadata": {"asset_id": "r-1"}, "entity": {"job_run": {"state": "Running", "configuration": {"env_variables": ["TRAINING_APIKEY=SEEDED-SECRET"]}}}});
        let s = serde_json::to_string(&wxctl_core::logging::redact_for_log(&get, &paths)).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "GET/201 response leaked env_variables: {s}");

        // Request envelope: job_run.configuration.env_variables (the submit body).
        let submit = json!({"job_run": {"name": "n", "configuration": {"env_variables": ["TRAINING_APIKEY=SEEDED-SECRET"]}}});
        let s = serde_json::to_string(&wxctl_core::logging::redact_for_log(&submit, &paths)).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "submit body leaked env_variables: {s}");

        // Jobs-LIST shape (job kind): results[].entity.job.configuration.env_variables.
        let jobs = json!({"results": [{"metadata": {"name": "train-job", "asset_id": "j-1"}, "entity": {"job": {"configuration": {"env_variables": ["TRAINING_APIKEY=SEEDED-SECRET"]}}}}]});
        let s = serde_json::to_string(&wxctl_core::logging::redact_for_log(&jobs, &paths)).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "jobs LIST response leaked env_variables: {s}");

        // Identity matching reads the RAW in-memory body — redaction only touches
        // the logged copy, so the marker stays extractable from the original.
        assert!(matches!(classify_runs_by_marker(&list, "abcd1234"), RunMatch::Completed(_)), "redaction must not affect in-memory marker matching");
    }

    // The engine's generic discovery LIST derives its redaction paths from the
    // schema (ResourceDefinition::sensitive_paths); the job_run schema must keep
    // env_variables sensitive with the configuration.env_variables api_field so
    // the derived superset reaches the CAMS response envelope.
    #[test]
    fn job_run_schema_sensitive_paths_cover_response_envelope() {
        let schema = wxctl_schema::load_all_schemas().unwrap().into_iter().find(|s| s.resource.kind == "job_run").expect("job_run schema present");
        let paths = schema.resource.sensitive_paths();
        for expected in ["env_variables", "configuration.env_variables", "entity.job_run.configuration.env_variables", "results.entity.job_run.configuration.env_variables"] {
            assert!(paths.contains(&expected.to_string()), "missing {expected}: {paths:?}");
        }
        // Identity is the env marker: storage env_marker, and the clobbered name must
        // not be diffed (no immutable name → no Recreate loop; parser drops name from
        // state_fields for identity-hash kinds).
        let ih = schema.resource.reconciliation.identity_hash.as_ref().expect("identity_hash block");
        assert!(matches!(ih.storage, wxctl_schema::schema::HashStorage::EnvMarker));
        assert!(!schema.resource.reconciliation.immutable_fields.contains(&"name".to_string()), "name must not be immutable-compared — the server clobbers it to 'Notebook Job'");
        let sf = schema.resource.reconciliation.state_fields.as_ref().expect("state_fields computed");
        assert!(!sf.contains(&"name".to_string()), "name must not be a state field");
        assert!(sf.contains(&"identity_hash".to_string()), "synthetic identity_hash state field expected");
    }
}
