use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use std::time::Duration;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec, error_has_status, error_matches};
use wxctl_core::logging::error_codes;
use wxctl_core::traits::HookOutcome;

const PAUSED_STATES: &[&str] = &["paused", "pausing", "stopped", "stopping"];
const READY_STATES: &[&str] = &["running", "ready", "active"];
const FAILED_STATES: &[&str] = &["failed", "error"];

/// Unknown values are classified as `running` so we never trigger a pause on an engine
/// in a state we don't recognize.
pub(super) fn equivalence_class(status: &str) -> &'static str {
    if PAUSED_STATES.iter().any(|s| s.eq_ignore_ascii_case(status)) { "paused" } else { "running" }
}

pub async fn handle_status_transition(current: &Value, desired: &Value, client: &HttpClient, base_path: &str, operation_id: &str) -> Result<HookOutcome> {
    let id = current.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("engine_lifecycle: missing id on current resource"))?;

    let current_raw = current.get("status").and_then(|v| v.as_str()).unwrap_or("running");
    let desired_raw = desired.get("status").and_then(|v| v.as_str()).unwrap_or("running");

    if equivalence_class(current_raw) == equivalence_class(desired_raw) {
        return Ok(HookOutcome::Continue);
    }

    let target_class = equivalence_class(desired_raw);
    let action = if target_class == "paused" { "pause" } else { "resume" };
    let action_url = format!("{base_path}/{id}/{action}");

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        engine_id = %id,
        from_status = %current_raw,
        to_status = %desired_raw,
        action_url = %action_url,
        "engine_lifecycle: status transition"
    );

    let action_spec = RequestSpec::new(Method::POST, &action_url).body(BodyKind::Json(Value::Object(Default::default())));
    let _: Value = client.execute(operation_id, action_spec).await?;

    // Poll until the engine actually reaches the target equivalence class.
    // The API queues transitions asynchronously — firing another action while
    // the previous one is still in progress returns HTTP 500
    // ("cannot proceed while another operation is in progress").
    let refreshed = wait_for_engine_status_class(client, base_path, id, target_class, operation_id).await?;

    Ok(HookOutcome::Handled(refreshed))
}

/// Collapse the API's raw engine status into the two YAML-enum values
/// (`running`, `paused`) so drift detection matches our desired-state YAML
/// regardless of casing (`RUNNING`) or transitional states (`stopping`,
/// `pausing`). Without this, a successful pause can leave the remote in
/// `STOPPED` which would not literally equal our local `paused`.
pub fn normalize_status(remote: &mut Value) {
    if let Some(obj) = remote.as_object_mut()
        && let Some(Value::String(s)) = obj.get("status")
    {
        let normalized = equivalence_class(s);
        obj.insert("status".to_string(), Value::String(normalized.to_string()));
    }
}

/// Poll `{base_path}/{engine_id}` every 5s until `is_done` matches the observed status
/// string. Fails fast if the engine enters a known failure state.
///
/// The total budget defaults to 5 min (`WXCTL_ENGINE_READY_TIMEOUT` seconds, /5 = attempts).
/// Engine provisioning on a slow cluster (e.g. a contended CP4D presto/prestissimo create)
/// can genuinely exceed 5 min and still succeed — the engine reaches `running`, wxctl just
/// gave up polling. Raise the budget there: `WXCTL_ENGINE_READY_TIMEOUT=900`.
async fn poll_engine_until<F>(client: &HttpClient, base_path: &str, engine_id: &str, operation_id: &str, desc: &str, is_done: F) -> Result<Value>
where
    F: Fn(&str) -> bool,
{
    let timeout_secs: u32 = std::env::var("WXCTL_ENGINE_READY_TIMEOUT").ok().and_then(|v| v.parse().ok()).filter(|&s| s > 0).unwrap_or(300);
    let max_attempts = (timeout_secs / 5).max(1);

    crate::util::poll_until(max_attempts, Duration::from_secs(5), crate::util::PollTimeout::Bail(format!("[{operation_id}] timed out waiting for engine {engine_id} while {desc}")), None::<String>, |attempt, mut prev_status| {
        let is_done = &is_done;
        async move {
            let spec = RequestSpec::new(Method::GET, format!("{base_path}/{engine_id}")).body(BodyKind::None);
            let resp: Value = client.execute(operation_id, spec).await?;
            let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");

            if prev_status.as_deref() != Some(status) {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    engine_id = %engine_id,
                    status = %status,
                    attempt = attempt,
                    max_attempts = max_attempts,
                    phase = %desc,
                    "engine status observed"
                );
                prev_status = Some(status.to_string());
            }

            let outcome = if is_done(status) {
                crate::util::PollOutcome::Done(resp)
            } else if FAILED_STATES.iter().any(|s| s.eq_ignore_ascii_case(status)) {
                crate::util::PollOutcome::Failed(format!("[{operation_id}] engine {engine_id} entered failure state while {desc}: {status}"))
            } else {
                crate::util::PollOutcome::Pending
            };
            Ok((outcome, prev_status))
        }
    })
    .await
}

async fn wait_for_engine_status_class(client: &HttpClient, base_path: &str, engine_id: &str, target_class: &str, operation_id: &str) -> Result<Value> {
    poll_engine_until(client, base_path, engine_id, operation_id, "settling into target class", |status| {
        // Must match the target class AND not be in a transitional "-ing" state
        // (pausing/stopping/starting) — firing the next action while still
        // transitioning triggers HTTP 500.
        equivalence_class(status) == target_class && !status.eq_ignore_ascii_case("starting") && !PAUSED_STATES.iter().any(|s| s.eq_ignore_ascii_case(status) && s.ends_with("ing"))
    })
    .await
}

pub async fn wait_for_engine_ready(client: &HttpClient, base_path: &str, engine_id: &str, operation_id: &str) -> Result<()> {
    poll_engine_until(client, base_path, engine_id, operation_id, "reaching running state", |status| READY_STATES.iter().any(|s| s.eq_ignore_ascii_case(status))).await?;
    Ok(())
}

/// Reconcile `associated_catalogs` drift via the engine's catalogs sub-endpoint.
/// The engine PATCH body does not accept `associated_catalogs`, so without this
/// hook a drift on the catalog list would plan as Update but apply as a no-op.
///
/// POST `{base}/{id}/catalogs` with `{catalog_names: [added]}` attaches; DELETE
/// `{base}/{id}/catalogs?catalog_names=a,b` detaches. Strips the field from
/// `desired` so the subsequent default PATCH doesn't also try to send it.
///
/// Returns the refreshed engine state when any change was applied, or `None`
/// when current and desired catalog sets already matched.
async fn handle_catalog_drift(current: &Value, desired: &mut Value, client: &HttpClient, base_path: &str, operation_id: &str) -> Result<Option<Value>> {
    let id = current.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("engine_lifecycle: missing id on current resource"))?;

    let current_cats = extract_catalogs(current);
    let desired_cats = extract_catalogs(desired);

    let added: Vec<String> = desired_cats.iter().filter(|c| !current_cats.contains(c)).cloned().collect();
    let removed: Vec<String> = current_cats.iter().filter(|c| !desired_cats.contains(c)).cloned().collect();

    if added.is_empty() && removed.is_empty() {
        return Ok(None);
    }

    if !added.is_empty() {
        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            engine_id = %id,
            action = "associate_catalogs",
            catalogs = ?added,
            "engine_lifecycle: associating catalogs"
        );
        let spec = RequestSpec::new(Method::POST, format!("{base_path}/{id}/catalogs")).body(BodyKind::Json(serde_json::json!({ "catalog_names": added })));
        let _: Value = client.execute(operation_id, spec).await?;
    }
    if !removed.is_empty() {
        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            engine_id = %id,
            action = "disassociate_catalogs",
            catalogs = ?removed,
            "engine_lifecycle: disassociating catalogs"
        );
        let mut spec = RequestSpec::new(Method::DELETE, format!("{base_path}/{id}/catalogs")).body(BodyKind::None);
        spec = spec.query_param("catalog_names".to_string(), removed.join(","));
        let result: Result<Value> = client.execute(operation_id, spec).await;
        if let Err(e) = result {
            // A 404 on detach is idempotent success: the catalog was cascade-deleted
            // with its storage_registration / database_registration. Attach (POST)
            // stays strict — a 404 there is still a real error.
            if error_has_status(&e, 404) {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    engine_id = %id,
                    error_code = %error_codes::H601,
                    catalogs = ?removed,
                    "{}: catalog cascade-deleted with its registration; detach is a no-op",
                    error_codes::H601
                );
            } else {
                return Err(e);
            }
        }
    }

    if let Some(obj) = desired.as_object_mut() {
        obj.remove("associated_catalogs");
    }

    let fresh: Value = client.get(operation_id, &format!("{base_path}/{id}")).await?;
    Ok(Some(fresh))
}

fn extract_catalogs(v: &Value) -> Vec<String> {
    v.get("associated_catalogs").and_then(|a| a.as_array()).map(|arr| arr.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default()
}

/// Shared `pre_update` pipeline for presto_engine and spark_engine: reconcile
/// catalog drift first, then status transitions. Returns `Handled` with the
/// fresh engine state when either hook fired, or `Continue` to let the default
/// PATCH run for remaining field drift (display_name, description, tags).
pub async fn run_update_hooks(current: &Value, desired: &mut Value, client: &HttpClient, base_path: &str, operation_id: &str) -> Result<HookOutcome> {
    let catalog_refresh = handle_catalog_drift(current, desired, client, base_path, operation_id).await?;
    let status_outcome = handle_status_transition(current, desired, client, base_path, operation_id).await?;
    if matches!(status_outcome, HookOutcome::Handled(_)) {
        return Ok(status_outcome);
    }
    if let Some(fresh) = catalog_refresh {
        return Ok(HookOutcome::Handled(fresh));
    }
    Ok(HookOutcome::Continue)
}

/// Resolve a display-name conflict at create time by listing engines and adopting
/// the one matching `display_name`. Callers guard with their own `is_conflict`
/// matcher before calling; this helper does not check the error itself.
pub async fn adopt_by_display_name(client: &HttpClient, base_path: &str, resource_type: &'static str, display_name: &str, operation_id: &str) -> Result<Option<Value>> {
    let entries: Vec<Value> = client.list_with_params(operation_id, base_path, None).await?;
    for mut entry in entries {
        if entry.get("display_name").and_then(|v| v.as_str()) == Some(display_name) {
            normalize_status(&mut entry);
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = %resource_type,
                display_name = %display_name,
                error_code = error_codes::H710,
                "recovered from display-name conflict by adopting existing engine"
            );
            return Ok(Some(entry));
        }
    }
    Ok(None)
}

/// Shared `recover_from_create_error` body: when `is_recoverable(error)` holds, adopt the engine whose
/// `display_name` matches `resource` (re-apply / async-create collisions); otherwise `None`. Each handler
/// supplies its own `is_recoverable` matcher — presto/prestissimo share one, spark uses a different phrase.
pub async fn adopt_on_create_error(resource: &Value, error: &anyhow::Error, client: &HttpClient, base_path: &str, resource_type: &'static str, operation_id: &str, is_recoverable: impl Fn(&anyhow::Error) -> bool) -> Result<Option<Value>> {
    if !is_recoverable(error) {
        return Ok(None);
    }
    let Some(display_name) = resource.get("display_name").and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    adopt_by_display_name(client, base_path, resource_type, display_name, operation_id).await
}

/// Remove the desired-state `status` marker before create — the engine create
/// APIs reject unknown fields. `status` is wxctl's pause/resume desired-state,
/// not a create input. Shared by the watsonx.data engine handlers.
pub fn strip_status(resource: &mut Value) {
    if let Some(obj) = resource.as_object_mut() {
        obj.remove("status");
    }
}

/// Recognize the create errors the engine handlers recover from by adopting an
/// existing engine: a `display name already exists` 400, or the CPD async-create
/// 400 (`wxdengines.watsonxdata.ibm.com "..." not found`) that fires while the CR
/// is still being queued.
pub fn is_recoverable_create_error(err: &anyhow::Error) -> bool {
    error_matches(err, 400, &["display name", "already exists"]) || error_matches(err, 400, &["wxdengines.watsonxdata.ibm.com", "not found"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    // Recoverable = a 400 display-name collision OR the CPD async-create 400
    // (`wxdengines.watsonxdata.ibm.com "..." not found`). A 500 (even with the phrase) or
    // an unrelated 400 must NOT recover.
    #[test]
    fn is_recoverable_create_error_cases() {
        let cases: &[(&str, bool)] = &[
            ("WXCTL-H001 HTTP 400 POST: engine display name 'java-engine-3' already exists [trace_id=abc]", true),
            ("WXCTL-H001 HTTP 400 POST: wxdengines.watsonxdata.ibm.com \"lakehouse-\" not found", true),
            ("WXCTL-H002 HTTP 500 POST: display name already exists", false),
            ("WXCTL-H001 HTTP 400 POST: invalid node_type", false),
        ];
        for (msg, expected) in cases {
            assert_eq!(is_recoverable_create_error(&anyhow!("{msg}")), *expected, "{msg}");
        }
    }
}
