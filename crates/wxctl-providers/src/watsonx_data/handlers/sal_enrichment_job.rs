//! `sal_enrichment_job` handler — triggers a SAL metadata-enrichment job and
//! polls it to a terminal state.
//!
//! `POST /v3/sal_integration/enrichment` returns 204 (no id) and **ignores any
//! project_id** — SAL auto-provisions an IKC project per enriched schema named
//! `SAL Mapping /{catalog}/{schema} <sal-id>` and drives a metadata-import
//! (`SAL_MDI`) + metadata-enrichment (`SAL_MDE`) job under it. So to observe the
//! job we, per `changes[]` target:
//!   1. resolve that project via `GET /v2/projects` (a CPD-root endpoint — uses
//!      the raw client, since it is not under the watsonx.data `/lakehouse/api`
//!      prefix and so cannot go through the normal `execute`),
//!   2. find the `SAL_MDE` job's `asset_id` via
//!      `GET /v3/sal_integration/enrichment/jobs?project_id=<proj>` (the list is
//!      an array of opaque JSON *strings*),
//!   3. poll `GET /v3/sal_integration/enrichment/jobs/{asset_id}/runs?project_id=<proj>`
//!      for `entity.job_run.state` (`Completed` / `Failed`).
//!
//! Poll exhaustion returns Ok (best-effort — does not fail the apply); only a
//! confirmed failed run bails. There is no `pre_delete` — destroy is carried by
//! the resource's `on_destroy: retain`.

use anyhow::{Result, bail};
use reqwest::Method;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::traits::ResourceHandler;

pub struct SalEnrichmentJobHandler;

const JOBS_PATH: &str = "/v3/sal_integration/enrichment/jobs";
const JOB_RUNS_PATH: &str = "/v3/sal_integration/enrichment/jobs/{job_id}/runs";
const DONE_MARKERS: &[&str] = &["completed", "finished", "succeeded", "success"];
const FAILED_MARKERS: &[&str] = &["failed", "failure", "error", "cancel"];

impl ResourceHandler for SalEnrichmentJobHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, _response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            wait_for_enrichment_terminal(client, resource, operation_id).await?;
            // Q2 local-hash fallback: a confirmed-failed run bailed above (no record →
            // next apply re-runs); success (incl. best-effort poll exhaustion — the
            // apply succeeded) records the desired hash so the next apply is NoChange.
            record_local_hash(resource, client, "sal_enrichment_job", operation_id);
            Ok(())
        })
    }
}

/// One enrichment target (`changes[]` element) and its cached SAL Mapping project.
struct Target {
    catalog: String,
    schema: String,
    project_id: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
enum TargetState {
    Done,
    Failed(String),
    Pending,
}

async fn wait_for_enrichment_terminal(client: &HttpClient, resource: &Value, operation_id: &str) -> Result<()> {
    let mut pending: Vec<Target> = resource.get("changes").and_then(|v| v.as_array()).map(|arr| arr.iter().filter_map(|c| Some(Target { catalog: c.get("catalog")?.as_str()?.to_string(), schema: c.get("schema")?.as_str()?.to_string(), project_id: None })).collect()).unwrap_or_default();

    if pending.is_empty() {
        tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_enrichment_job", "no changes[] catalog/schema entries to track; skipping job-status poll (the enrichment POST already fired)");
        return Ok(());
    }

    let max_attempts = 60;
    let poll_interval = Duration::from_secs(10);

    for attempt in 1..=max_attempts {
        let current = std::mem::take(&mut pending);
        for mut target in current {
            // Resolve (and cache) the auto-created `SAL Mapping /{catalog}/{schema}` project.
            if target.project_id.is_none() {
                match resolve_sal_mapping_project(client, &target.catalog, &target.schema).await {
                    Ok(Some(pid)) => target.project_id = Some(pid),
                    Ok(None) => {
                        pending.push(target); // project not created yet — keep polling
                        continue;
                    }
                    Err(e) => {
                        tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_enrichment_job", error = %e, "failed to list projects while resolving SAL Mapping project; will retry");
                        pending.push(target);
                        continue;
                    }
                }
            }
            let project_id = target.project_id.clone().expect("project_id set above");

            match check_target_state(client, &project_id, operation_id).await {
                Ok(TargetState::Done) => {
                    tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_enrichment_job", catalog = %target.catalog, schema = %target.schema, attempt = attempt, "enrichment reached a terminal (completed) state");
                }
                Ok(TargetState::Failed(detail)) => {
                    bail!("[{operation_id}] sal_enrichment_job for {}/{} reached a failed state: {detail}", target.catalog, target.schema);
                }
                Ok(TargetState::Pending) => pending.push(target),
                Err(e) => {
                    tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_enrichment_job", catalog = %target.catalog, schema = %target.schema, error = %e, "enrichment poll attempt errored; will retry");
                    pending.push(target);
                }
            }
        }

        if pending.is_empty() {
            return Ok(());
        }
        if attempt < max_attempts {
            tokio::time::sleep(poll_interval).await;
        }
    }

    // Opaque/eventual API: never positively saw a terminal marker for every
    // target. Best-effort — log and return rather than fail the apply.
    tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_enrichment_job", pending = pending.len(), "enrichment job poll exhausted without a terminal marker for all targets; returning (best-effort)");
    Ok(())
}

/// Resolve the IKC project `SAL Mapping /{catalog}/{schema} <sal-id>` that SAL
/// auto-creates per enriched schema. Uses the raw client because `/v2/projects`
/// is a CPD-root endpoint (not under the watsonx.data `/lakehouse/api` prefix),
/// so it can't go through the normal path-prefixed `execute`.
async fn resolve_sal_mapping_project(client: &HttpClient, catalog: &str, schema: &str) -> Result<Option<String>> {
    let token = client.get_token().await?;
    let url = format!("{}/v2/projects", client.base_url().trim_end_matches('/'));
    // NOTE: single page, `limit=100`, no bookmark pagination. On a cluster with
    // >100 projects the freshly-created `SAL Mapping /{catalog}/{schema}` project
    // could fall off this page and never be matched — the poll then exhausts and
    // returns Ok (best-effort), so the apply succeeds without positively observing
    // the run. Acceptable for the lab/test envs this targets; revisit with bookmark
    // pagination (or a `?name=`/sort filter) if it bites a large-tenant deployment.
    // apply_auth_scheme (not hardcoded Bearer) keeps zenapikey/CP4D auth working.
    let req = client.raw_client().get(&url).query(&[("limit", "100")]);
    let resp = client.apply_auth_scheme(req, &token)?.send().await?;
    if !resp.status().is_success() {
        return Ok(None);
    }
    let body: Value = resp.json().await?;
    Ok(match_sal_mapping_guid(&body, catalog, schema))
}

/// Pure: match the `SAL Mapping /{catalog}/{schema} <sal-id>` project in a
/// `/v2/projects` response and return its `metadata.guid`.
fn match_sal_mapping_guid(projects_body: &Value, catalog: &str, schema: &str) -> Option<String> {
    let exact = format!("SAL Mapping /{catalog}/{schema}");
    let prefix = format!("SAL Mapping /{catalog}/{schema} ");
    projects_body.get("resources").and_then(|v| v.as_array()).and_then(|arr| {
        arr.iter().find_map(|r| {
            let name = r.get("entity").and_then(|e| e.get("name")).and_then(|n| n.as_str())?;
            if name == exact || name.starts_with(&prefix) { r.get("metadata").and_then(|m| m.get("guid")).and_then(|g| g.as_str()).map(str::to_string) } else { None }
        })
    })
}

/// Find the `SAL_MDE` job under a project, then read its latest run state.
async fn check_target_state(client: &HttpClient, project_id: &str, operation_id: &str) -> Result<TargetState> {
    let jobs_spec = RequestSpec::new(Method::GET, JOBS_PATH).query_param("project_id", project_id).body(BodyKind::None);
    let jobs_resp: Value = client.execute(operation_id, jobs_spec).await?;
    let Some(job_id) = mde_job_asset_id(&jobs_resp) else {
        return Ok(TargetState::Pending); // SAL_MDE job not registered yet
    };
    let runs_spec = RequestSpec::new(Method::GET, JOB_RUNS_PATH).path_var("job_id", &job_id).query_param("project_id", project_id).body(BodyKind::None);
    let runs_resp: Value = client.execute(operation_id, runs_spec).await?;
    Ok(run_state(&runs_resp))
}

/// Pure: the `SAL_MDE` (metadata-enrichment) job's `asset_id` from a jobs-list
/// response. The `jobs` array holds opaque JSON *strings*, each a job document.
fn mde_job_asset_id(jobs_body: &Value) -> Option<String> {
    let jobs = jobs_body.get("jobs").and_then(|v| v.as_array())?;
    jobs.iter().find_map(|j| {
        let doc = j.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok())?;
        let is_mde = doc.get("entity").and_then(|e| e.get("job")).and_then(|jb| jb.get("asset_ref_type")).and_then(|t| t.as_str()) == Some("metadata_enrichment_area") || doc.get("metadata").and_then(|m| m.get("name")).and_then(|n| n.as_str()) == Some("SAL_MDE job");
        if is_mde { doc.get("metadata").and_then(|m| m.get("asset_id")).and_then(|i| i.as_str()).map(str::to_string) } else { None }
    })
}

/// Pure: collapse a job-runs response into a terminal state. Runs are JSON
/// *strings*; the signal is `entity.job_run.state` (`Completed`/`Failed`). A
/// failed run wins; else a completed run; else still pending (no runs yet).
///
/// Done is classified BEFORE failed: a terminal `CompletedWithErrors` /
/// `CompletedWithWarnings` (both real CPD job-run states) carries the "error"
/// substring but is a *successful* completion, so the done markers are matched
/// as a prefix (`completed…`) and win over the failure substring-scan — a naive
/// `state.contains("error")` would otherwise mis-bail an enrichment that
/// actually finished. A genuinely failed / canceled run has no done prefix and
/// trips `FAILED_MARKERS`.
fn run_state(runs_body: &Value) -> TargetState {
    let Some(runs) = runs_body.get("runs").and_then(|v| v.as_array()) else {
        return TargetState::Pending;
    };
    let mut any_done = false;
    for r in runs {
        let Some(doc) = r.as_str().and_then(|s| serde_json::from_str::<Value>(s).ok()) else { continue };
        let state = doc.get("entity").and_then(|e| e.get("job_run")).and_then(|jr| jr.get("state")).and_then(|s| s.as_str()).unwrap_or_default().to_ascii_lowercase();
        if DONE_MARKERS.iter().any(|m| state.starts_with(m)) {
            any_done = true;
        } else if FAILED_MARKERS.iter().any(|m| state.contains(m)) {
            return TargetState::Failed(state);
        }
    }
    if any_done { TargetState::Done } else { TargetState::Pending }
}

/// Record the validation-stamped `identity_hash` in the local run-hash store
/// (Q2 fallback). Best-effort — logs a warning on failure, never errors (an fs
/// hiccup must not fail a run the API accepted). Shared by both sal handlers.
pub(crate) fn record_local_hash(resource: &Value, client: &HttpClient, kind: &str, operation_id: &str) {
    let Some(hash) = resource.get("identity_hash").and_then(|v| v.as_str()) else { return };
    let ref_name = resource.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    let env = crate::local_hash::env_key(client.base_url());
    if let Err(e) = crate::local_hash::record_run_hash(&env, kind, ref_name, hash) {
        tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = kind, error = %e, "failed to record local run hash; next apply will re-run (best-effort)");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Real shapes captured live on CP4D (2026-06-05).

    #[test]
    fn matches_sal_mapping_project_by_name_prefix() {
        let body = json!({"resources": [
            {"metadata": {"guid": "7aef7038"}, "entity": {"name": "Default Data Product Delivery Project"}},
            {"metadata": {"guid": "04e296ac"}, "entity": {"name": "SAL Mapping /iceberg_data/sal_demo 46bd15d6-98ec-4323-b7d6-9dee98bc710c"}},
        ]});
        assert_eq!(match_sal_mapping_guid(&body, "iceberg_data", "sal_demo").as_deref(), Some("04e296ac"));
        // A different schema (prefix-sharing) must not match.
        assert_eq!(match_sal_mapping_guid(&body, "iceberg_data", "sal"), None);
        assert_eq!(match_sal_mapping_guid(&body, "iceberg_data", "missing"), None);
    }

    #[test]
    fn picks_the_mde_job_asset_id() {
        // jobs[] are JSON strings; SAL_MDE (metadata_enrichment_area) + SAL_MDI (metadata_import).
        let mde = r#"{"metadata":{"name":"SAL_MDE job","asset_id":"b327eca1"},"entity":{"job":{"asset_ref_type":"metadata_enrichment_area"}}}"#;
        let mdi = r#"{"metadata":{"name":"SAL_MDI job","asset_id":"cd981f38"},"entity":{"job":{"asset_ref_type":"metadata_import"}}}"#;
        let body = json!({"jobs": [mdi, mde]});
        assert_eq!(mde_job_asset_id(&body).as_deref(), Some("b327eca1"));
        assert_eq!(mde_job_asset_id(&json!({"jobs": null})), None);
        assert_eq!(mde_job_asset_id(&json!({"jobs": [mdi]})), None);
    }

    #[test]
    fn run_state_detects_terminal() {
        let completed = r#"{"entity":{"job_run":{"state":"Completed","job_type":"metadata_enrichment_area"}}}"#;
        let failed = r#"{"entity":{"job_run":{"state":"Failed"}}}"#;
        let running = r#"{"entity":{"job_run":{"state":"Running"}}}"#;
        assert_eq!(run_state(&json!({"runs": [completed]})), TargetState::Done);
        assert_eq!(run_state(&json!({"runs": [failed]})), TargetState::Failed("failed".to_string()));
        assert_eq!(run_state(&json!({"runs": [running]})), TargetState::Pending);
        // No runs yet (empty or null) → still pending, not a false-terminal.
        assert_eq!(run_state(&json!({"runs": []})), TargetState::Pending);
        assert_eq!(run_state(&json!({"runs": null})), TargetState::Pending);
        // A failed run wins even alongside a completed one.
        assert_eq!(run_state(&json!({"runs": [completed, failed]})), TargetState::Failed("failed".to_string()));

        // `CompletedWithErrors` / `CompletedWithWarnings` are real CPD job-run states and are
        // terminal *successes* — the former carries the "error" substring, so a `contains`-based
        // failure scan would mis-bail it. Done-before-failed precedence keeps them Done.
        let completed_with_errors = r#"{"entity":{"job_run":{"state":"CompletedWithErrors"}}}"#;
        let completed_with_warnings = r#"{"entity":{"job_run":{"state":"CompletedWithWarnings"}}}"#;
        assert_eq!(run_state(&json!({"runs": [completed_with_errors]})), TargetState::Done);
        assert_eq!(run_state(&json!({"runs": [completed_with_warnings]})), TargetState::Done);
        // But a real Failed alongside a CompletedWithErrors still bails.
        assert_eq!(run_state(&json!({"runs": [completed_with_errors, failed]})), TargetState::Failed("failed".to_string()));
        // A canceled run is a failure (no done prefix → trips the `cancel` marker).
        let canceled = r#"{"entity":{"job_run":{"state":"Canceled"}}}"#;
        assert_eq!(run_state(&json!({"runs": [canceled]})), TargetState::Failed("canceled".to_string()));
    }
}
