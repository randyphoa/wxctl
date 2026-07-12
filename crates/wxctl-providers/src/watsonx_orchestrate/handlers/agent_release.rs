//! `agent_release` handler — owns the non-CRUD watsonx Orchestrate deploy verbs the schema
//! DSL cannot express. The default create is a hardcoded POST of declared body fields, but a
//! release needs an `environment_id` resolved from the `live` env first (an undeclared key the
//! materializer would drop — see docs/troubleshoot/pre-create-body-reshape-dropped-fix.md), so
//! `pre_create` issues the request itself and returns `HookOutcome::Handled`. `pre_delete` owns
//! the undeploy (the default delete is a hardcoded DELETE). `post_discover` reduces the matched
//! `live` Environment object to the release's fields. `recover_from_create_error` adopts an
//! already-released state so re-apply is idempotent.
//!
//! Discovery is `list_and_get` + `identity_match { environment -> name }`: an un-released agent
//! (draft-only environment array) yields no match => Create; a released agent matches the `live`
//! env carrying `current_version`. When `agent_id` is an unresolved `${agent.ref}` (agent created
//! in the same apply), discovery is skipped and the op is `CreateUnchecked`; `pre_create` re-runs
//! the full resolve/create/release at execution time, so that path is safe (mirrors the
//! `environment` kind — docs/troubleshoot/environment-adopt-only-createunchecked-fix.md).
//!
//! Version pinning is a plan-visible compare, not custom diff code. `state_fields: [environment,
//! version]` + `reduce_env` mirroring remote `current_version` onto `version` (the `id_field`)
//! means: (a) omitting `version` locally makes `schema_reconciler::compare` skip it
//! (`field_exists` is false) — no perpetual diff on the auto-latest path; and (b) pinning
//! `version: N` diffs N against the mirrored remote value A => `Update { fields: [version] }` =>
//! plan `~ update ... [~version]`. `pre_update` owns the repoint POST (returns `Handled`), so the
//! engine's default update (a POST of a materialized body against `get_endpoint` = `.../environment`)
//! never runs.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct AgentReleaseHandler;

/// The only environment this kind targets. `allowed_values: [live]` enforces it in the schema.
const LIVE_ENV: &str = "live";

fn env_path(agent_id: &str) -> String {
    format!("/v1/orchestrate/agents/{agent_id}/environment")
}

fn releases_path(agent_id: &str) -> String {
    format!("/v1/orchestrate/agents/{agent_id}/releases")
}

/// Repoint the live environment to an existing version:
/// `POST /v1/orchestrate/agents/{agent_id}/releases/{version}/environment/{environment_id}`.
/// Shared by `pre_create` (version-set branch) and `pre_update`.
fn repoint_path(agent_id: &str, version: i64, environment_id: &str) -> String {
    format!("/v1/orchestrate/agents/{agent_id}/releases/{version}/environment/{environment_id}")
}

/// GET the agent's environment array (a bare `Environment[]`).
async fn get_environments(client: &HttpClient, operation_id: &str, agent_id: &str) -> Result<Value> {
    let spec = RequestSpec::new(Method::GET, env_path(agent_id)).body(BodyKind::None);
    client.execute(operation_id, spec).await
}

/// Find the `live` environment object in an `Environment[]` (matched by `name`).
fn find_live_env(envs: &Value) -> Option<&Value> {
    envs.as_array()?.iter().find(|e| e.get("name").and_then(|v| v.as_str()) == Some(LIVE_ENV))
}

/// Reduce a matched `live` Environment object to the release's canonical fields. `current_version`
/// is mirrored onto `version` (the `id_field`) so the engine's pre_delete id-injection and the
/// `version` state-field compare both see it.
fn reduce_env(env: &Value, agent_id: &str) -> Value {
    let mut out = json!({ "agent_id": agent_id, "environment": LIVE_ENV });
    if let Some(id) = env.get("id") {
        out["environment_id"] = id.clone();
    }
    if let Some(cv) = env.get("current_version").filter(|v| !v.is_null()) {
        out["current_version"] = cv.clone();
        out["version"] = cv.clone();
    }
    if let Some(s) = env.get("deployment_status").or_else(|| env.get("status")).filter(|v| !v.is_null()) {
        out["status"] = s.clone();
    }
    out
}

impl ResourceHandler for AgentReleaseHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let agent_id = resource.get("agent_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("agent_release requires a resolved 'agent_id'"))?.to_string();

            // Resolve the live env id, creating it on first deploy (GET returns only `draft`).
            let envs = get_environments(client, operation_id, &agent_id).await?;
            let environment_id = match find_live_env(&envs).and_then(|e| e.get("id")).and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    let body = json!({ "name": LIVE_ENV, "description": "Live environment (managed by wxctl)" });
                    let created: Value = client.execute(operation_id, RequestSpec::new(Method::POST, env_path(&agent_id)).body(BodyKind::Json(body))).await?;
                    created.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("creating the live environment for agent '{agent_id}' returned no id"))?.to_string()
                }
            };

            // version omitted => release the current draft; version set => pin the live env to it.
            match resource.get("version").and_then(|v| v.as_i64()) {
                None => {
                    let mut body = json!({ "environment_id": environment_id });
                    if let Some(comments) = resource.get("comments").filter(|c| !c.is_null()) {
                        body["comments"] = comments.clone();
                    }
                    let _resp: Value = client.execute(operation_id, RequestSpec::new(Method::POST, releases_path(&agent_id)).body(BodyKind::Json(body))).await?;
                }
                Some(version) => {
                    let path = repoint_path(&agent_id, version, &environment_id);
                    let _resp: Value = client.execute(operation_id, RequestSpec::new(Method::POST, &path).body(BodyKind::None)).await?;
                }
            }

            // Re-read the live env so the created resource carries the resulting current_version.
            let after = get_environments(client, operation_id, &agent_id).await?;
            let reduced = find_live_env(&after).map(|e| reduce_env(e, &agent_id)).unwrap_or_else(|| json!({ "agent_id": agent_id, "environment": LIVE_ENV, "environment_id": environment_id }));
            Ok(HookOutcome::Handled(reduced))
        })
    }

    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, _error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            // A failed recovery must NOT replace the original create error (the engine `.await?`s
            // this) — return Ok(None) so the engine falls back to it.
            let Some(agent_id) = resource.get("agent_id").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            match get_environments(client, operation_id, agent_id).await {
                Ok(envs) => match find_live_env(&envs) {
                    Some(env) if env.get("current_version").map(|v| !v.is_null()).unwrap_or(false) => Ok(Some(reduce_env(env, agent_id))),
                    _ => Ok(None),
                },
                Err(e) => {
                    tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, kind = "agent_release", error = %e, "recovery env GET failed — falling back to the original create error");
                    Ok(None)
                }
            }
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // agent_id: from the resolved local (authoritative); reduce_env also mirrors it onto the remote.
            let agent_id = desired.get("agent_id").and_then(|v| v.as_str()).or_else(|| current.get("agent_id").and_then(|v| v.as_str())).ok_or_else(|| anyhow!("agent_release update requires a resolved 'agent_id'"))?.to_string();

            // The desired pinned version. `version` is the only diffable state field that can route
            // here (environment is always `live`), so an absent version means compare mis-fired.
            let version = desired.get("version").and_then(|v| v.as_i64()).ok_or_else(|| anyhow!("agent_release update requires a pinned integer 'version'"))?;

            // environment_id comes from discovery: post_discover/reduce_env mirrored it onto the
            // remote. Fall back to a fresh env GET if the remote didn't carry it (mirrors pre_create).
            let environment_id = match current.get("environment_id").and_then(|v| v.as_str()) {
                Some(id) => id.to_string(),
                None => {
                    let envs = get_environments(client, operation_id, &agent_id).await?;
                    find_live_env(&envs).and_then(|e| e.get("id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("no live environment to repoint for agent '{agent_id}'"))?.to_string()
                }
            };

            let path = repoint_path(&agent_id, version, &environment_id);
            let _resp: Value = client.execute(operation_id, RequestSpec::new(Method::POST, &path).body(BodyKind::None)).await?;

            // Re-read the live env so the updated resource carries the resulting current_version.
            let after = get_environments(client, operation_id, &agent_id).await?;
            let reduced = find_live_env(&after).map(|e| reduce_env(e, &agent_id)).unwrap_or_else(|| json!({ "agent_id": agent_id, "environment": LIVE_ENV, "environment_id": environment_id, "version": version }));
            Ok(HookOutcome::Handled(reduced))
        })
    }

    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let agent_id = resource.get("agent_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("agent_release delete requires a resolved 'agent_id'"))?.to_string();

            // The engine injects the discovered id_field (`version`) into the resource before
            // pre_delete (execution/operations/delete.rs). Fall back to a fresh env GET.
            let version = match resource.get("version").and_then(|v| v.as_i64()) {
                Some(v) => Some(v),
                None => {
                    let envs = get_environments(client, operation_id, &agent_id).await?;
                    find_live_env(&envs).and_then(|e| e.get("current_version")).and_then(|v| v.as_i64())
                }
            };

            let Some(version) = version else {
                // No live release => already undeployed. Destroy is a no-op, not an error.
                return Ok(HookOutcome::Handled(json!({ "undeployed": false })));
            };

            let path = format!("/v1/orchestrate/agents/{agent_id}/releases/{version}/undeploy");
            let _resp: Value = client.execute(operation_id, RequestSpec::new(Method::POST, &path).body(BodyKind::None).not_found_ok()).await?;
            Ok(HookOutcome::Handled(json!({ "undeployed": version })))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Discovery matched the `live` Environment object; reduce it to the release fields.
            let agent_id = remote_data.get("agent_id").and_then(|v| v.as_str()).unwrap_or_default().to_string();
            *remote_data = reduce_env(remote_data, &agent_id);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_live_env_matches_by_name() {
        let envs = json!([
            { "id": "e-draft", "name": "draft", "current_version": null },
            { "id": "e-live", "name": "live", "current_version": 5 }
        ]);
        let live = find_live_env(&envs).expect("live env present");
        assert_eq!(live.get("id").and_then(|v| v.as_str()), Some("e-live"));
    }

    #[test]
    fn find_live_env_absent_when_draft_only() {
        let envs = json!([{ "id": "e-draft", "name": "draft", "current_version": null }]);
        assert!(find_live_env(&envs).is_none());
    }

    #[test]
    fn reduce_env_mirrors_current_version_onto_version() {
        let env = json!({ "id": "e-live", "agent_id": "a1", "name": "live", "current_version": 7, "deployment_status": "deployed" });
        let out = reduce_env(&env, "a1");
        assert_eq!(out.get("environment_id").and_then(|v| v.as_str()), Some("e-live"));
        assert_eq!(out.get("environment").and_then(|v| v.as_str()), Some("live"));
        assert_eq!(out.get("current_version").and_then(|v| v.as_i64()), Some(7));
        assert_eq!(out.get("version").and_then(|v| v.as_i64()), Some(7), "current_version mirrored to version (id_field)");
        assert_eq!(out.get("status").and_then(|v| v.as_str()), Some("deployed"));
    }

    #[test]
    fn reduce_env_unreleased_live_has_no_version() {
        let env = json!({ "id": "e-live", "agent_id": "a1", "name": "live", "current_version": null });
        let out = reduce_env(&env, "a1");
        assert_eq!(out.get("environment_id").and_then(|v| v.as_str()), Some("e-live"));
        assert!(out.get("version").is_none(), "no current_version => no version");
    }
}
