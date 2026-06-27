use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec, error_matches};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct AutoaiExperimentHandler;

const TRAININGS: &str = "/ml/v4/trainings";
const PIPELINES: &str = "/ml/v4/pipelines";
const VERSION: &str = "2024-01-01";
const DONE_STATES: &[&str] = &["completed"];
const FAILED_STATES: &[&str] = &["failed", "canceled"];

fn matches_status(status: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|s| s.eq_ignore_ascii_case(status))
}

/// learning_type for the optimization block. AutoAI tabular task types map 1:1
/// onto the wxctl prediction_type allowed_values.
fn learning_type(prediction_type: &str) -> &'static str {
    match prediction_type {
        "multiclass" => "multiclass",
        "regression" => "regression",
        _ => "binary",
    }
}

/// Append `space_id` / `project_id` as query params from the resource.
fn with_scope(mut spec: RequestSpec, resource: &Value) -> RequestSpec {
    if let Some(s) = resource.get("space_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("space_id", s);
    }
    if let Some(p) = resource.get("project_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("project_id", p);
    }
    spec
}

/// Build the AutoAI pipeline document (doc_type: pipeline) from user fields.
fn build_pipeline_doc(resource: &Value) -> Value {
    let prediction_type = resource.get("prediction_type").and_then(|v| v.as_str()).unwrap_or("binary");
    let mut optimization = json!({
        "learning_type": learning_type(prediction_type),
        "label": resource.get("prediction_column").and_then(|v| v.as_str()).unwrap_or(""),
        "run_cognito_flag": true,
    });
    if let Some(scoring) = resource.get("scoring").and_then(|v| v.as_str()) {
        optimization["scorer_for_ranking"] = json!(scoring);
    }
    if let Some(holdout) = resource.get("holdout_size").and_then(|v| v.as_f64()) {
        optimization["holdout_param"] = json!(holdout);
    }
    if let Some(est) = resource.get("include_only_estimators").and_then(|v| v.as_array()) {
        optimization["include_only_estimators"] = json!(est);
    }
    // The v4 pipelines API rejects an empty hardware_spec ("No id or name provided
    // when validating the hardware specification"). Mirror the SDK's t_shirt_size
    // mapping: a short size string (≤2 chars, e.g. "L"/"XS") is a hardware-spec
    // NAME (upper-cased); a longer value is treated as a hardware-spec id. Default
    // to "L" — the SDK's default for binary/multiclass remote AutoAI runs.
    let t_size = resource.get("t_shirt_size").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).unwrap_or("L");
    let hardware_spec = if t_size.len() <= 2 { json!({"name": t_size.to_uppercase()}) } else { json!({"id": t_size}) };
    json!({
        "doc_type": "pipeline",
        "version": "2.0",
        "primary_pipeline": "autoai",
        "pipelines": [{
            "id": "autoai",
            "runtime_ref": "hybrid",
            "nodes": [{
                "id": "automl",
                "type": "execution_node",
                "op": "kube",
                "runtime_ref": "autoai",
                "parameters": {"stage_flag": true, "output_logs": true, "optimization": optimization},
            }],
        }],
        "runtimes": [{"id": "autoai", "name": "auto_ai.kb", "app_data": {"wml_data": {"hardware_spec": hardware_spec}}}],
    })
}

impl ResourceHandler for AutoaiExperimentHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] autoai_experiment requires name"))?.to_string();
            let asset_id = resource.get("training_data").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] autoai_experiment requires training_data (data_asset id)"))?.to_string();
            let space_id = resource.get("space_id").and_then(|v| v.as_str()).map(|s| s.to_string());
            // Read project_id and space_id for poll before any mutable borrow of resource.
            let project_id = resource.get("project_id").and_then(|v| v.as_str()).map(|s| s.to_string());
            let space_id_poll = space_id.clone();

            // 1. Store the AutoAI pipeline document.
            let pipeline_doc = build_pipeline_doc(resource);
            let mut pipeline_body = json!({"name": format!("{name}-pipeline"), "document": pipeline_doc});
            if let Some(s) = &space_id {
                pipeline_body["space_id"] = json!(s);
            }
            let pipe_spec = RequestSpec::new(Method::POST, PIPELINES).query_param("version", VERSION).body(BodyKind::Json(pipeline_body));
            let pipe_resp: Value = client.execute(operation_id, pipe_spec).await.map_err(|e| anyhow!("[{operation_id}] autoai pipeline store failed: {e}"))?;
            let pipeline_id = pipe_resp.pointer("/metadata/id").or_else(|| pipe_resp.get("id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] no pipeline id in store response: {}", serde_json::to_string_pretty(&pipe_resp).unwrap_or_default()))?.to_string();
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "autoai_experiment",
                pipeline_id = %pipeline_id,
                "stored AutoAI pipeline document"
            );

            // 2. Build the training-submit body and POST it directly.
            let data_href = match &space_id {
                Some(s) => format!("/v2/assets/{asset_id}?space_id={s}"),
                None => format!("/v2/assets/{asset_id}"),
            };
            // The training results_reference differs by deployment flavor:
            //  - Software (CP4D): type `fs` — managed CP4D file storage. The server rejects
            //    `container` ("results_reference can only be of type 'fs'"). The SDK
            //    substitutes the `{option}`/`{id}` template CLIENT-SIDE (`spaces`/`projects`
            //    + the scope id) before POST; sending the raw template stores a poisoned
            //    path so the per-pipeline `request.json` fetch 404s. Substitute it here; the
            //    pipeline_id (a server uuid) keeps the path unique.
            //  - SaaS (IBM Cloud): type `container` — the space's BUNDLED COS container. The
            //    body carries no creds/bucket; the path is `<space_id>/default_autoai_out`.
            //    `fs` managed file storage does not exist on SaaS. Phase 2's `wml_model`
            //    resolves the bundled COS connection to fetch the resulting `request.json`.
            let results_reference = if client.deployment().flavor() == wxctl_core::types::Flavor::Saas {
                let s = space_id.as_deref().ok_or_else(|| anyhow!("[{operation_id}] autoai_experiment on SaaS requires space_id to build the container results_reference path"))?;
                json!({"type": "container", "location": {"path": format!("{s}/default_autoai_out")}})
            } else {
                let results_path = if let Some(s) = &space_id {
                    format!("/spaces/{s}/assets/auto_ml/auto_ml.{pipeline_id}/wml_data")
                } else if let Some(p) = &project_id {
                    format!("/projects/{p}/assets/auto_ml/auto_ml.{pipeline_id}/wml_data")
                } else {
                    return Err(anyhow!("[{operation_id}] autoai_experiment requires space_id or project_id to build the results_reference path"));
                };
                json!({"type": "fs", "location": {"path": results_path}})
            };
            let mut training_body = json!({
                "name": name,
                "tags": ["autoai"],
                "pipeline": {"id": pipeline_id},
                "training_data_references": [{"type": "data_asset", "location": {"href": data_href}, "connection": {}}],
                "results_reference": results_reference,
            });
            if let Some(s) = &space_id {
                training_body["space_id"] = json!(s);
            }
            if let Some(p) = &project_id {
                training_body["project_id"] = json!(p);
            }

            let train_spec = RequestSpec::new(Method::POST, TRAININGS).query_param("version", VERSION).body(BodyKind::Json(training_body));
            let mut response: Value = client.execute(operation_id, train_spec).await.map_err(|e| anyhow!("[{operation_id}] autoai training submit failed: {e}"))?;

            // 3. Carry pipeline_id onto the response immediately.
            if let Some(obj) = response.as_object_mut() {
                obj.insert("pipeline_id".to_string(), json!(pipeline_id));
            }

            // 4. Poll to terminal state and fold leaderboard into the response.
            if let Some(training_id) = response.pointer("/metadata/id").or_else(|| response.get("id")).and_then(|v| v.as_str()).map(|s| s.to_string()) {
                let terminal = wait_for_training_terminal(client, &training_id, space_id_poll.as_deref(), operation_id).await?;
                if let Some(obj) = response.as_object_mut() {
                    normalize_run_into(&terminal, obj);
                }
                // Surface the ranked leaderboard in the run record (deliverable: the
                // CLI run shows the experiment reaching completed with ≥1 ranked pipeline).
                let n = response.get("leaderboard").and_then(|v| v.as_array()).map(|a| a.len()).unwrap_or(0);
                let best = response.pointer("/best_pipeline/name").and_then(|v| v.as_str()).unwrap_or("-");
                let state = response.get("state").and_then(|v| v.as_str()).unwrap_or("unknown");
                tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "autoai_experiment", training_id = %training_id, state = %state, pipelines = n, best_pipeline = %best, "autoai_experiment finished: {n} ranked pipeline(s), best={best}");
            }

            Ok(HookOutcome::Handled(response))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, client: &'a HttpClient, operation_id: &'a str, is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        // Normalize the discovered run so a completed experiment re-plans clean
        // (state / best_pipeline / leaderboard from entity.status). On the apply
        // path only, if the discovered run is still non-terminal, tail it to a
        // terminal state so a re-apply doesn't return mid-run.
        Box::pin(async move {
            let state = remote_data.pointer("/entity/status/state").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let snapshot = remote_data.clone();
            if let Some(obj) = remote_data.as_object_mut() {
                normalize_run_into(&snapshot, obj);
            }
            if !is_apply || matches_status(&state, DONE_STATES) || matches_status(&state, FAILED_STATES) {
                return Ok(());
            }
            let Some(training_id) = remote_data.pointer("/metadata/id").or_else(|| remote_data.get("id")).and_then(|v| v.as_str()).map(|s| s.to_string()) else {
                return Ok(());
            };
            let terminal = wait_for_training_terminal(client, &training_id, None, operation_id).await?;
            if let Some(obj) = remote_data.as_object_mut() {
                normalize_run_into(&terminal, obj);
            }
            Ok(())
        })
    }

    fn post_delete<'a>(&'a self, resource: &'a Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        // The training DELETE (hard_delete=true) is the default op. Cascade-delete
        // the stored AutoAI pipeline document; 404 is tolerated (partial apply).
        Box::pin(async move {
            let Some(pipeline_id) = resource.get("pipeline_id").and_then(|v| v.as_str()) else {
                return Ok(());
            };
            let spec = with_scope(RequestSpec::new(Method::DELETE, format!("{PIPELINES}/{pipeline_id}")).query_param("version", VERSION).body(BodyKind::None), resource);
            match client.execute::<Value>(operation_id, spec).await {
                Ok(_) => Ok(()),
                Err(e) if error_matches(&e, 404, &[]) => {
                    tracing::debug!(
                        target: "wxctl::substage::provider",
                        operation_id = %operation_id,
                        resource_type = "autoai_experiment",
                        pipeline_id = %pipeline_id,
                        "pipeline already absent on cascade delete (404 tolerated)"
                    );
                    Ok(())
                }
                Err(e) => Err(anyhow!("[{operation_id}] failed to delete AutoAI pipeline {pipeline_id}: {e}")),
            }
        })
    }

    fn recover_from_create_error<'a>(&'a self, _resource: &'a Value, _error: &'a anyhow::Error, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        // A training submit has no natural already-exists conflict (the server
        // mints a fresh id), so there is nothing to adopt — surface the error.
        Box::pin(async move { Ok(None) })
    }
}

/// Extract state / pipeline_id / best_pipeline / leaderboard from a run value
/// (the GET shape: entity.status.{state,metrics}) into the resource object so a
/// completed run re-plans clean and the CLI can display the leaderboard.
fn normalize_run_into(run: &Value, obj: &mut serde_json::Map<String, Value>) {
    // Expose the training id top-level so downstream refs (${autoai_experiment.x.id})
    // resolve — the engine stores a Handled response (and a discovered run) verbatim and
    // does NOT synthesize the schema's computed `id` from metadata.id (mirrors data_asset's
    // top-level asset_id at common_core/handlers/data_asset.rs).
    if let Some(id) = run.pointer("/metadata/id").and_then(|v| v.as_str()) {
        obj.insert("id".to_string(), json!(id));
    }
    if let Some(state) = run.pointer("/entity/status/state").and_then(|v| v.as_str()) {
        obj.insert("state".to_string(), json!(state));
    }
    if let Some(pid) = run.pointer("/entity/pipeline/id").and_then(|v| v.as_str()) {
        obj.insert("pipeline_id".to_string(), json!(pid));
    }
    if let Some(metrics) = run.pointer("/entity/status/metrics").and_then(|v| v.as_array()) {
        obj.insert("leaderboard".to_string(), json!(metrics));
        // Best pipeline = the first ranked metric entry (the API orders best-first).
        if let Some(first) = metrics.first() {
            let name = first.pointer("/context/intermediate_model/name").or_else(|| first.pointer("/context/intermediate_model/pipeline_name")).and_then(|v| v.as_str()).unwrap_or("Pipeline_1");
            obj.insert("best_pipeline".to_string(), json!({"name": name, "metrics": first.get("ml_metrics").cloned().unwrap_or(Value::Null)}));
        }
    }
}

/// Poll GET /ml/v4/trainings/{id} until a terminal state. Returns the terminal
/// run value. Timeout 40 min (160 attempts @ 15 s). On failed/canceled, bail
/// with the AutoAI failure message (not a generic poll timeout).
async fn wait_for_training_terminal(client: &HttpClient, training_id: &str, space_id: Option<&str>, operation_id: &str) -> Result<Value> {
    let max_attempts = 160u32;
    let space_id = space_id.map(|s| s.to_string());
    let training_id = training_id.to_string();
    let operation_id = operation_id.to_string();

    crate::util::poll_until(max_attempts, Duration::from_secs(15), crate::util::PollTimeout::Bail(format!("[{operation_id}] timed out (40 min) waiting for autoai_experiment {training_id} to reach a terminal state")), None::<String>, move |attempt, mut prev_state| {
        let space_id = space_id.clone();
        let training_id = training_id.clone();
        let operation_id = operation_id.clone();
        async move {
            let mut path = format!("{TRAININGS}/{training_id}?version={VERSION}");
            if let Some(s) = &space_id {
                path.push_str(&format!("&space_id={s}"));
            }
            let spec = RequestSpec::new(Method::GET, &path).body(BodyKind::None);
            let resp: Value = client.execute(&operation_id, spec).await?;
            let state = resp.pointer("/entity/status/state").and_then(|v| v.as_str()).unwrap_or("unknown").to_string();
            if prev_state.as_deref() != Some(state.as_str()) {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "autoai_experiment",
                    training_id = %training_id,
                    status = %state,
                    attempt = attempt,
                    max_attempts = max_attempts,
                    "autoai_experiment status observed"
                );
                prev_state = Some(state.clone());
            }
            let outcome = if matches_status(&state, DONE_STATES) {
                crate::util::PollOutcome::Done(resp.clone())
            } else if matches_status(&state, FAILED_STATES) {
                let reason = resp.pointer("/entity/status/failure/errors/0/message").or_else(|| resp.pointer("/entity/status/message/text")).or_else(|| resp.pointer("/entity/status/message")).and_then(|v| v.as_str()).unwrap_or("unknown AutoAI failure");
                crate::util::PollOutcome::Failed(format!("[{operation_id}] autoai_experiment {training_id} {state}: {reason}"))
            } else {
                crate::util::PollOutcome::Pending
            };
            Ok((outcome, prev_state))
        }
    })
    .await
}
