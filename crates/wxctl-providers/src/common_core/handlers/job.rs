//! `common_core/job` handler — the create API wraps its payload under a
//! top-level `job` key. Live evidence (SaaS + CP4D) and the published IBM
//! Data & AI Common Core OpenAPI spec (`JobsJobPostBody`) agree:
//! `POST /v2/jobs` rejects a flat body with 400 "must have required
//! property 'job'". The generic materializer has no concept of a body
//! envelope — it assembles the declared Body fields (including the nested
//! `configuration` object via `api_field` mappings) correctly, but serialises
//! them flat at the top level. This handler owns `pre_create` so it can wrap
//! that same assembled shape under `{"job": {...}}` before POSTing — the same
//! family as `job_run`'s `{"job_run": {...}}` submit wrapper.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec, error_matches};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct JobHandler;

/// Extract the job asset id from a create/get response: `metadata.asset_id`
/// (CAMS envelope, the documented shape), a bare top-level `asset_id`
/// (already-transformed), then `metadata.id` (defensive fallback).
fn extract_asset_id(value: &Value) -> Option<String> {
    value.pointer("/metadata/asset_id").or_else(|| value.get("asset_id")).or_else(|| value.pointer("/metadata/id")).and_then(|v| v.as_str()).map(str::to_string)
}

/// Convert an `env_variables` array of `{name, value}` objects (the config/schema
/// shape — readable YAML, individually `sensitive`-markable) into the wire shape
/// the API actually accepts: an array of `"NAME=value"` strings. Live evidence
/// (identical 400 on SaaS and CP4D, 2026-07-05): `POST /v2/jobs` rejects the object
/// shape with "/job/configuration/env_variables/0 must be string". Entries missing
/// a string `name` or `value` are skipped (debug-logged) rather than failing the
/// whole build — best-effort so one malformed entry doesn't block the rest.
pub(crate) fn env_pairs_to_strings(env_vars: &[Value]) -> Vec<String> {
    env_vars
        .iter()
        .filter_map(|entry| {
            let name = entry.get("name").and_then(|v| v.as_str());
            let value = entry.get("value").and_then(|v| v.as_str());
            match (name, value) {
                (Some(name), Some(value)) => Some(format!("{name}={value}")),
                _ => {
                    tracing::debug!(target: "wxctl::substage::provider", entry = %entry, "skipping malformed env_variables entry (expected {{name, value}} strings)");
                    None
                }
            }
        })
        .collect()
}

/// Sensitive dotted paths for every handler-owned job/job_run RequestSpec, covering
/// BOTH directions of the wire: request envelopes ({"job": {...}} / {"job_run": {...}}
/// create wrappers) and response envelopes (create/GET/poll bodies nest under
/// `entity.job` / `entity.job_run`; LIST wraps items under `results[]` —
/// `redact_by_schema` skips array indices, so the `results.` spellings match every
/// item). Responses echo the submitted `configuration.env_variables` back verbatim
/// (live-pinned 2026-07-05: a runs-LIST response carried a plaintext TRAINING_APIKEY
/// into the WXCTL_LOG_PATH sink). Values are "NAME=value" strings, so the whole
/// array is masked — secret-safety over log readability. Redaction is applied to
/// the LOGGED copy only (wxctl-core http.rs `redact_for_log`); the in-memory
/// response the identity matching reads is untouched.
pub(crate) fn job_env_sensitive_paths() -> Vec<String> {
    [
        "env_variables",
        "configuration.env_variables",
        "job.configuration.env_variables",
        "job_run.configuration.env_variables",
        "entity.job.configuration.env_variables",
        "entity.job_run.configuration.env_variables",
        "results.entity.job.configuration.env_variables",
        "results.entity.job_run.configuration.env_variables",
    ]
    .map(String::from)
    .to_vec()
}

/// Find a job by exact name in the list-response `results[]` (the CAMS shape
/// GET /v2/jobs returns) and shape it like a create response: the matched item
/// with `asset_id` + `name` hoisted top-level so downstream `${job.x.asset_id}`
/// refs and state comparison resolve.
pub(crate) fn match_job_by_name(list_response: &Value, name: &str) -> Option<Value> {
    let results = list_response.get("results").and_then(|v| v.as_array())?;
    let item = results.iter().find(|item| item.pointer("/metadata/name").or_else(|| item.get("name")).and_then(|v| v.as_str()) == Some(name))?;
    let id = extract_asset_id(item)?;
    let mut adopted = item.clone();
    if let Some(obj) = adopted.as_object_mut() {
        obj.insert("asset_id".to_string(), json!(id));
        obj.insert("name".to_string(), json!(name));
    }
    Some(adopted)
}

/// Build the inner `job` object POSTed as `{"job": <this>}`: `name` /
/// `description` / `asset_ref` at the top, plus a `configuration` object
/// nesting `env_id` and `env_variables`. Mirrors the schema's `api_field`
/// mappings (`asset` → `asset_ref`, `environment` → `configuration.env_id`,
/// `env_variables` → `configuration.env_variables`, transformed via
/// `env_pairs_to_strings` — see its doc comment). `asset_ref_type` is
/// deliberately not sent — the API rejects a body containing both `asset_ref`
/// and `asset_ref_type` with 400 "mutually exclusive" (live-verified
/// 2026-07-05, both SaaS and CP4D); the server derives the runnable type from
/// the referenced asset.
///
/// `schedule` / `schedule_info` are top-level under the inner `job` object, so
/// they ride the same `{"job": {...}}` create envelope. Updates use
/// `update_strategy: recreate` (destroy + create), whose create half re-enters
/// `pre_create` → `build_job_body`, so the update path carries the identical
/// nesting — no separate PATCH body shaping is needed.
fn build_job_body(resource: &Value) -> Result<Value> {
    let name = resource.get("name").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("job requires 'name'"))?;
    let asset_ref = resource.get("asset").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("job requires 'asset'"))?;

    let mut job = json!({
        "name": name,
        "asset_ref": asset_ref,
    });
    if let Some(description) = resource.get("description").and_then(|v| v.as_str()) {
        job["description"] = json!(description);
    }
    if let Some(schedule) = resource.get("schedule").and_then(|v| v.as_str()) {
        job["schedule"] = json!(schedule);
    }
    if let Some(schedule_info) = resource.get("schedule_info") {
        job["schedule_info"] = schedule_info.clone();
    }

    let mut configuration = json!({});
    if let Some(env_id) = resource.get("environment").and_then(|v| v.as_str()) {
        configuration["env_id"] = json!(env_id);
    }
    if let Some(env_vars) = resource.get("env_variables").and_then(|v| v.as_array()) {
        configuration["env_variables"] = json!(env_pairs_to_strings(env_vars));
    }
    job["configuration"] = configuration;

    Ok(job)
}

impl ResourceHandler for JobHandler {
    /// Own the full create: the materializer's flat serialisation 400s live,
    /// so build the `{"job": {...}}` envelope here and POST it directly,
    /// returning Handled so the engine skips its default (flat) POST. Jobs
    /// are project-scoped — `project_id` is required, not merely optional.
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let project_id = resource.get("project_id").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).ok_or_else(|| anyhow!("[{operation_id}] job requires 'project_id' (jobs are project-scoped)"))?.to_string();
            let job_body = build_job_body(resource).map_err(|e| anyhow!("[{operation_id}] {e}"))?;

            let spec = RequestSpec::new(Method::POST, "/v2/jobs").query_param("project_id", project_id.clone()).body(BodyKind::Json(json!({"job": job_body}))).sensitive_paths(job_env_sensitive_paths());
            let mut resp: Value = client.execute(operation_id, spec).await.map_err(|e| anyhow!("[{operation_id}] job create POST failed (project_id={project_id}): {e}"))?;

            if let Some(id) = extract_asset_id(&resp)
                && let Some(obj) = resp.as_object_mut()
            {
                obj.insert("asset_id".to_string(), json!(id));
            }

            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job", project_id = %project_id, "job created via job-envelope wrapper");

            Ok(HookOutcome::Handled(resp))
        })
    }

    /// Adopt-on-conflict backstop: `POST /v2/jobs` rejects a duplicate name with
    /// 400 "Cannot create the job. A job with the same name already exists."
    /// (live-proven, SaaS + CP4D, 2026-07-05 — hit whenever reconciliation
    /// couldn't discover the existing job ahead of the create, e.g. the
    /// CreateUnchecked deferred path). Recover by listing the project's jobs and
    /// adopting the one with the exact name. `pre_create` owns the POST, so the
    /// engine routes its error here (create.rs pre_create-Err branch).
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            // Only attempt recovery on duplicate-name conflicts, not auth/5xx.
            if !(error_matches(error, 409, &[]) || error_matches(error, 400, &["already", "exist"])) {
                return Ok(None);
            }
            let (Some(name), Some(project_id)) = (resource.get("name").and_then(|v| v.as_str()), resource.get("project_id").and_then(|v| v.as_str())) else {
                return Ok(None);
            };
            let spec = RequestSpec::new(Method::GET, "/v2/jobs").query_param("project_id", project_id).body(BodyKind::None).sensitive_paths(job_env_sensitive_paths());
            let resp: Value = match client.execute(operation_id, spec).await {
                Ok(v) => v,
                Err(_) => return Ok(None),
            };
            match match_job_by_name(&resp, name) {
                Some(existing) => {
                    tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "job", name = %name, "adopt: existing job matched by name after duplicate-name create error");
                    Ok(Some(existing))
                }
                None => Ok(None),
            }
        })
    }

    /// Hoist `asset_id` (and `name`, if absent) top-level on discovery so
    /// `${job.x.asset_id}` refs and state comparison resolve regardless of
    /// whether the data came from create or `list_and_get` discovery.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(id) = extract_asset_id(remote_data)
                && let Some(obj) = remote_data.as_object_mut()
            {
                obj.insert("asset_id".to_string(), json!(id));
            }
            if remote_data.get("name").and_then(|v| v.as_str()).is_none()
                && let Some(name) = remote_data.pointer("/metadata/name").and_then(|v| v.as_str()).map(str::to_string)
                && let Some(obj) = remote_data.as_object_mut()
            {
                obj.insert("name".to_string(), json!(name));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // extract_asset_id scans metadata.asset_id, then a bare top-level asset_id,
    // then metadata.id.
    #[test]
    fn extract_asset_id_scans_common_locations() {
        assert_eq!(extract_asset_id(&json!({"metadata": {"asset_id": "a-1"}})).as_deref(), Some("a-1"));
        assert_eq!(extract_asset_id(&json!({"asset_id": "a-2"})).as_deref(), Some("a-2"));
        assert_eq!(extract_asset_id(&json!({"metadata": {"id": "a-3"}})).as_deref(), Some("a-3"));
        assert_eq!(extract_asset_id(&json!({"nope": true})), None);
    }

    // build_job_body wraps name/asset_ref at the top and nests
    // environment/env_variables under configuration per the api_field mappings.
    // asset_ref_type is deliberately absent — the API rejects it alongside
    // asset_ref as mutually exclusive. env_variables is converted from
    // {name, value} objects to "NAME=value" strings — the live API rejects the
    // object shape (400 "/job/configuration/env_variables/0 must be string").
    #[test]
    fn build_job_body_maps_fields_into_envelope_shape() {
        let resource = json!({
            "name": "churn-scoring",
            "description": "nightly scoring job",
            "asset": "script-123",
            "environment": "env-456",
            "env_variables": [{"name": "THRESHOLD", "value": "0.5"}],
        });
        let body = build_job_body(&resource).expect("build_job_body should succeed");
        assert_eq!(body["name"], json!("churn-scoring"));
        assert_eq!(body["description"], json!("nightly scoring job"));
        assert_eq!(body["asset_ref"], json!("script-123"));
        assert!(body.get("asset_ref_type").is_none());
        assert_eq!(body["configuration"]["env_id"], json!("env-456"));
        assert_eq!(body["configuration"]["env_variables"], json!(["THRESHOLD=0.5"]));
    }

    // schedule / schedule_info ride the top level of the inner job object; both
    // are omitted when absent.
    #[test]
    fn build_job_body_includes_schedule_fields() {
        let resource = json!({
            "name": "nightly-ingest",
            "asset": "notebook-1",
            "schedule": "0 2 * * *",
            "schedule_info": {"repeat": true, "startOn": 1_700_000_000_000_i64},
        });
        let body = build_job_body(&resource).expect("build_job_body should succeed");
        assert_eq!(body["schedule"], json!("0 2 * * *"));
        assert_eq!(body["schedule_info"]["repeat"], json!(true));
        assert_eq!(body["schedule_info"]["startOn"], json!(1_700_000_000_000_i64));

        let no_sched = build_job_body(&json!({"name": "n", "asset": "a"})).unwrap();
        assert!(no_sched.get("schedule").is_none());
        assert!(no_sched.get("schedule_info").is_none());
    }

    // env_pairs_to_strings converts {name, value} objects into "NAME=value"
    // strings, and skips malformed entries (missing/non-string name or value)
    // rather than failing the whole conversion.
    #[test]
    fn env_pairs_to_strings_converts_and_skips_malformed() {
        let pairs = vec![json!({"name": "THRESHOLD", "value": "0.5"}), json!({"name": "MODE", "value": "prod"}), json!({"value": "missing-name"}), json!({"name": "missing-value"}), json!({"name": "BAD", "value": 5}), json!("not-an-object")];
        assert_eq!(env_pairs_to_strings(&pairs), vec!["THRESHOLD=0.5".to_string(), "MODE=prod".to_string()]);
    }

    #[test]
    fn env_pairs_to_strings_empty_input_yields_empty_output() {
        assert!(env_pairs_to_strings(&[]).is_empty());
    }

    // match_job_by_name adopts by exact metadata.name from the /v2/jobs list
    // shape, hoisting asset_id + name top-level; misses return None.
    #[test]
    fn match_job_by_name_adopts_exact_name_only() {
        let list = json!({"total_rows": 2, "results": [
            {"metadata": {"name": "other-job", "asset_id": "job-1"}},
            {"metadata": {"name": "traditional-ml-train-job", "asset_id": "job-2"}, "entity": {"job": {}}},
        ]});
        let adopted = match_job_by_name(&list, "traditional-ml-train-job").expect("match");
        assert_eq!(adopted.get("asset_id").and_then(|v| v.as_str()), Some("job-2"));
        assert_eq!(adopted.get("name").and_then(|v| v.as_str()), Some("traditional-ml-train-job"));
        assert_eq!(adopted.pointer("/metadata/asset_id").and_then(|v| v.as_str()), Some("job-2"));

        assert!(match_job_by_name(&list, "absent-job").is_none());
        assert!(match_job_by_name(&json!({"results": []}), "traditional-ml-train-job").is_none());
        assert!(match_job_by_name(&json!({}), "traditional-ml-train-job").is_none());
    }

    // Required fields missing (name/asset) surface a descriptive error rather
    // than silently building a partial body.
    #[test]
    fn build_job_body_requires_name_and_asset() {
        assert!(build_job_body(&json!({"asset": "a"})).is_err());
        assert!(build_job_body(&json!({"name": "n"})).is_err());
        assert!(build_job_body(&json!({"name": "n", "asset": "a"})).is_ok());
    }
}
