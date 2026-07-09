//! `openscale/monitor_instance` handler — fire the monitor's first evaluation run
//! after create when `evaluate_on_create: true`. OpenScale initializes a monitor
//! instance asynchronously: for a few seconds after create it is not yet `active`,
//! and the runs endpoint rejects with 400 `AIQMM0010E "Monitor Instance ... is not
//! active"` (live-proven on both quality and fairness monitors). So this handler
//! first polls the monitor to `active`, THEN fires the run fire-and-forget: POST
//! the run — 2xx = fired; 429 `AIQMM0012E` = OpenScale auto-triggered an initial
//! run itself when the monitor activated with enough records already stored (the
//! subscription seeding satisfies e.g. fairness `min_records`), so a first
//! evaluation is already in progress and `evaluate_on_create` is satisfied (also
//! success); anything else = H901. No run-completion poll — `wxctl test`'s
//! `expect_metrics` polls for the result in a later phase. The generic reconciler
//! handles create/update/discovery; this handler only adds the optional
//! post_create trigger. DAG ordering guarantees the subscription (and its seeded
//! records) exist before the run fires (monitors depend on the subscription).

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::HttpClient;
use wxctl_core::logging::error_codes;
use wxctl_core::traits::ResourceHandler;

pub struct MonitorInstanceHandler;

impl ResourceHandler for MonitorInstanceHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if !should_evaluate(resource) {
                return Ok(());
            }
            let Some(id) = crate::util::resource_id(response).map(str::to_string) else {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "monitor_instance", reason = "missing_id_in_create_response", "skipping first evaluation run");
                return Ok(());
            };
            wait_for_monitor_active(client, &id, operation_id).await?;
            fire_first_run(client, &id, operation_id).await
        })
    }
}

/// True when the config set `evaluate_on_create: true` (a real JSON boolean).
fn should_evaluate(resource: &Value) -> bool {
    resource.get("evaluate_on_create").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// POST the monitor's first evaluation run (`triggered_by: user`). Fire-and-forget:
/// accept any 2xx and return. 429 AIQMM0012E means OpenScale's auto-triggered
/// initial run is already processing the monitor — the `evaluate_on_create` intent
/// is already satisfied, so that collision is success too. Anything else is fatal
/// (H901).
async fn fire_first_run(client: &HttpClient, monitor_id: &str, operation_id: &str) -> Result<()> {
    let token = client.get_token().await.context("monitor_instance: failed to get token for first run")?;
    let url = format!("{}/v2/monitor_instances/{monitor_id}/runs", client.base_url().trim_end_matches('/'));
    tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, monitor_instance_id = %monitor_id, "firing monitor first evaluation run");
    // Raw request (not execute()) because the collision tolerance below needs the raw
    // status + body; apply_auth_scheme keeps zenapikey/CP4D auth working.
    let req = client.raw_client().post(&url).json(&json!({"triggered_by": "user"}));
    let resp = client.apply_auth_scheme(req, &token)?.send().await.context("monitor_instance: first-run request failed")?;
    let status = resp.status().as_u16();
    if is_accepted(status) {
        return Ok(());
    }
    let body = resp.text().await.unwrap_or_default();
    if run_collision(status, &body) {
        tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, monitor_instance_id = %monitor_id, "evaluation run already in progress — treating evaluate_on_create as satisfied");
        return Ok(());
    }
    bail!("[{}] monitor_instance: first evaluation run for {monitor_id} returned {status}: {body}", error_codes::H901);
}

/// 2xx run-accepted codes (the runs endpoint returns 201).
fn is_accepted(status: u16) -> bool {
    (200..300).contains(&status)
}

/// True only for 429 AIQMM0012E — "There is another run ... processing this monitor
/// instance": OpenScale auto-triggered an initial evaluation when the monitor
/// activated with enough stored records, so our explicit run collided with it.
fn run_collision(status: u16, body: &str) -> bool {
    status == 429 && body.contains("AIQMM0012E")
}

/// Poll the monitor instance until `entity.status.state` is `active` (or `error`).
/// OpenScale initializes a monitor instance asynchronously after create; the runs
/// endpoint rejects with 400 AIQMM0010E until the monitor reaches `active`.
/// Bounded at ~2 minutes (24 x 5s).
async fn wait_for_monitor_active(client: &HttpClient, monitor_id: &str, operation_id: &str) -> Result<Value> {
    let max_attempts = 24;
    crate::util::poll_until(max_attempts, Duration::from_secs(5), crate::util::PollTimeout::Bail(format!("[{}] monitor instance {monitor_id} did not reach active state", error_codes::H901)), None::<String>, |attempt, mut prev| async move {
        let path = format!("/v2/monitor_instances/{monitor_id}");
        let spec = wxctl_core::client::RequestSpec::new(wxctl_core::client::Method::GET, &path).body(wxctl_core::client::BodyKind::None);
        let response: Value = client.execute(operation_id, spec).await?;
        let state = monitor_state(&response).unwrap_or("unknown").to_string();
        if prev.as_deref() != Some(state.as_str()) {
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, monitor_instance_id = %monitor_id, status = %state, attempt = attempt, max_attempts = max_attempts, "monitor instance status observed");
            prev = Some(state.clone());
        }
        let outcome = match monitor_state_outcome(&state) {
            MonitorOutcome::Done => crate::util::PollOutcome::Done(response.clone()),
            MonitorOutcome::Failed => crate::util::PollOutcome::Failed(format!("[{}] monitor instance {monitor_id} initialized into error state (configuration incomplete)", error_codes::H901)),
            MonitorOutcome::Pending => crate::util::PollOutcome::Pending,
        };
        Ok((outcome, prev))
    })
    .await
}

/// Read the monitor instance lifecycle state from a GET response (`entity.status.state`).
fn monitor_state(response: &Value) -> Option<&str> {
    response.pointer("/entity/status/state").and_then(|v| v.as_str())
}

/// Classify a monitor-instance `state` into a poll step. `active` = ready for runs;
/// `error` = failed; everything else (`preparing`, `pending`, ...) keeps polling.
fn monitor_state_outcome(state: &str) -> MonitorOutcome {
    match state {
        "active" => MonitorOutcome::Done,
        "error" => MonitorOutcome::Failed,
        _ => MonitorOutcome::Pending,
    }
}

/// Terminal classification of one monitor-instance poll attempt.
enum MonitorOutcome {
    Done,
    Failed,
    Pending,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn should_evaluate_reads_flag() {
        assert!(should_evaluate(&json!({"evaluate_on_create": true})));
        assert!(!should_evaluate(&json!({"evaluate_on_create": false})));
        assert!(!should_evaluate(&json!({})));
        assert!(!should_evaluate(&json!({"evaluate_on_create": "true"})), "string is not a bool true");
    }

    #[test]
    fn is_accepted_classifies() {
        assert!(is_accepted(201));
        assert!(is_accepted(200));
        assert!(!is_accepted(400));
        assert!(!is_accepted(500));
    }

    #[test]
    fn monitor_state_read() {
        let active = json!({"entity": {"status": {"state": "active"}}});
        assert_eq!(monitor_state(&active), Some("active"));
        let preparing = json!({"entity": {"status": {"state": "preparing"}}});
        assert_eq!(monitor_state(&preparing), Some("preparing"));
        let absent = json!({"entity": {"status": {}}});
        assert_eq!(monitor_state(&absent), None);
    }

    #[test]
    fn monitor_state_classified() {
        assert!(matches!(monitor_state_outcome("active"), MonitorOutcome::Done));
        assert!(matches!(monitor_state_outcome("error"), MonitorOutcome::Failed));
        assert!(matches!(monitor_state_outcome("preparing"), MonitorOutcome::Pending));
    }

    #[test]
    fn run_collision_classified() {
        assert!(run_collision(429, r#"{"errors":[{"code":"AIQMM0012E","message":"There is another run abc-123 processing this monitor instance"}]}"#));
        assert!(!run_collision(429, r#"{"errors":[{"code":"AIQOTHER","message":"rate limited"}]}"#), "other 429s still bail");
        assert!(!run_collision(400, r#"{"errors":[{"code":"AIQMM0012E"}]}"#), "AIQMM0012E on a non-429 status still bails");
    }
}
