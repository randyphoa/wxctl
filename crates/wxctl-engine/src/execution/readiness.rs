//! Reference-readiness gate. Before a resource's create POST, poll every
//! reference marked `require_ready` until its target reaches the target
//! kind's declared readiness state (`api.readiness`). Skips references whose
//! target kind is unknown, declares no readiness block, has no resolved id,
//! or whose service is not part of this apply. Runs on the Create path only
//! (not Recreate/Update). Spec:
//! docs/specs/2026-07-07-openscale-data-mart-readiness-spec.md.

use super::ExecutionState;
use anyhow::{Result, bail};
use reqwest::Method;
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;
use wxctl_core::client::{BodyKind, RequestSpec};
use wxctl_core::logging::error_codes;
use wxctl_core::registry::ResourceDescriptor;
use wxctl_schema::schema::ReadinessDefinition;

/// A referenced resource whose readiness gates the consumer's create.
struct ReadyTarget {
    kind: String,
    service: String,
    id: String,
    get_endpoint: String,
    id_field: String,
    readiness: ReadinessDefinition,
}

enum ReadyOutcome {
    Ready,
    Failed(String),
    Pending,
}

/// Poll budget resolved from a readiness block (+ optional env override).
struct Budget {
    secs: u32,
    interval: Duration,
    max_attempts: u32,
}

/// Read a dot-path (e.g. `entity.status.state`) from a JSON body as a string.
fn read_state_path(response: &Value, state_path: &str) -> Option<String> {
    let mut cur = response;
    for part in state_path.split('.') {
        cur = cur.get(part)?;
    }
    cur.as_str().map(str::to_string)
}

/// Classify an observed state against the readiness contract.
fn classify(state: &str, readiness: &ReadinessDefinition, kind: &str, id: &str) -> ReadyOutcome {
    if readiness.ready.iter().any(|s| s == state) {
        ReadyOutcome::Ready
    } else if readiness.failed.iter().any(|s| s == state) {
        ReadyOutcome::Failed(format!("[{}/readiness] {} {} entered failure state '{}'", error_codes::H002, kind, id, state))
    } else {
        ReadyOutcome::Pending
    }
}

/// Resolve the poll budget: env override (if set, parseable, > 0) else the
/// schema default; interval floored to 1s; at least one attempt.
fn poll_budget(readiness: &ReadinessDefinition) -> Budget {
    let secs = readiness.timeout_env.as_deref().and_then(|var| std::env::var(var).ok()).and_then(|raw| raw.parse::<u32>().ok()).filter(|&n| n > 0).unwrap_or(readiness.timeout_default);
    let interval_secs = readiness.interval_secs.max(1);
    let max_attempts = (secs / interval_secs).max(1);
    Budget { secs, interval: Duration::from_secs(interval_secs as u64), max_attempts }
}

/// Poll one target until ready, using `fetch` to GET its current body.
/// Ok when a ready value is observed; bails on a failed value or budget
/// exhaustion; a persistent GET error propagates via `?`.
async fn poll_target_ready<F, Fut>(target: &ReadyTarget, mut fetch: F) -> Result<()>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    let budget = poll_budget(&target.readiness);
    let mut last: Option<String> = None;
    for attempt in 1..=budget.max_attempts {
        let response = fetch().await?;
        let state = read_state_path(&response, &target.readiness.state_path).unwrap_or_else(|| "unknown".to_string());
        if last.as_deref() != Some(state.as_str()) {
            tracing::debug!(target: "wxctl::substage::execution", kind = %target.kind, id = %target.id, status = %state, attempt, max_attempts = budget.max_attempts, "readiness gate: state observed");
            last = Some(state.clone());
        }
        match classify(&state, &target.readiness, &target.kind, &target.id) {
            ReadyOutcome::Ready => return Ok(()),
            ReadyOutcome::Failed(msg) => bail!(msg),
            ReadyOutcome::Pending => {
                if attempt < budget.max_attempts {
                    tokio::time::sleep(budget.interval).await;
                }
            }
        }
    }
    bail!("[{}/readiness] {} {} did not reach ready state within {}s", error_codes::H002, target.kind, target.id, budget.secs)
}

/// Collect the consumer's `require_ready` references into pollable targets.
/// Skips (debug log) a reference whose target kind is unknown to `lookup`,
/// declares no readiness block, or has no non-empty resolved id.
fn collect_ready_targets(descriptor: &ResourceDescriptor, resolved_data: &Value, lookup: &dyn Fn(&str) -> Option<Arc<ResourceDescriptor>>) -> Vec<ReadyTarget> {
    let mut targets = Vec::new();
    for field in &descriptor.schema.resource.schema.fields {
        let Some(refs) = field.references.as_ref() else { continue };
        if !refs.require_ready {
            continue;
        }
        let Some(ref_desc) = lookup(&refs.resource) else {
            tracing::debug!(target: "wxctl::substage::execution", consumer = %descriptor.name, field = %field.name, ref_kind = %refs.resource, "readiness gate: referenced kind not in registry; skipping");
            continue;
        };
        let Some(readiness) = ref_desc.schema.resource.api.readiness.as_ref() else {
            tracing::debug!(target: "wxctl::substage::execution", consumer = %descriptor.name, field = %field.name, ref_kind = %refs.resource, "readiness gate: referenced kind declares no readiness block; skipping");
            continue;
        };
        let Some(id) = resolved_data.get(&field.name).and_then(|v| v.as_str()).filter(|s| !s.is_empty()) else {
            tracing::debug!(target: "wxctl::substage::execution", consumer = %descriptor.name, field = %field.name, ref_kind = %refs.resource, "readiness gate: no resolved id for reference; skipping");
            continue;
        };
        targets.push(ReadyTarget { kind: ref_desc.name.clone(), service: ref_desc.service.clone(), id: id.to_string(), get_endpoint: ref_desc.endpoints.get.clone(), id_field: ref_desc.id_field.clone(), readiness: readiness.clone() });
    }
    targets
}

/// Gate a resource's create on every `require_ready` reference reaching its
/// target's declared ready state. Called from the Create op's `execute`
/// after dependency resolution, before the create POST. A reference whose
/// service has no client in this apply is skipped with a debug log.
pub(in crate::execution) async fn gate_references_ready(resolved_data: &Value, descriptor: &ResourceDescriptor, state: &ExecutionState) -> Result<()> {
    let targets = collect_ready_targets(descriptor, resolved_data, &|kind| state.registry.get_descriptor(kind).cloned());
    for target in &targets {
        let Some(client) = state.clients.get(&target.service) else {
            tracing::debug!(target: "wxctl::substage::execution", kind = %target.kind, service = %target.service, "readiness gate: no client for referenced service in this apply; skipping");
            continue;
        };
        poll_target_ready(target, || {
            let spec = RequestSpec::new(Method::GET, target.get_endpoint.clone()).path_var(target.id_field.clone(), target.id.clone()).body(BodyKind::None);
            client.execute::<Value>(&state.operation_id, spec)
        })
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use wxctl_core::registry::ResourceDescriptor;
    use wxctl_schema::schema::{ReadinessDefinition, SchemaParser};

    fn readiness(ready: &[&str], failed: &[&str], timeout_default: u32, interval_secs: u32) -> ReadinessDefinition {
        ReadinessDefinition { state_path: "entity.status.state".to_string(), ready: ready.iter().map(|s| s.to_string()).collect(), failed: failed.iter().map(|s| s.to_string()).collect(), timeout_env: None, timeout_default, interval_secs }
    }

    fn target(readiness: ReadinessDefinition) -> ReadyTarget {
        ReadyTarget { kind: "data_mart".to_string(), service: "openscale".to_string(), id: "dm-1".to_string(), get_endpoint: "/v2/data_marts/{id}".to_string(), id_field: "id".to_string(), readiness }
    }

    fn desc(yaml: &str) -> Arc<ResourceDescriptor> {
        Arc::new(ResourceDescriptor::from_schema(&SchemaParser::parse_str(yaml).unwrap()).unwrap())
    }

    const DATA_MART_YAML: &str = r#"
resource:
  name: data_mart
  service: openscale
  kind: data_mart
  version: v1
  api:
    base_path: /v2/data_marts
    id_field: id
    get_endpoint: /v2/data_marts/{id}
    create_method: POST
    delete_method: DELETE
    readiness:
      state_path: entity.status.state
      ready: [active]
      failed: [error, disabled]
      timeout_default: 300
      interval_secs: 5
  schema:
    fields: []
  reconciliation:
    discovery:
      method: singleton
    update_strategy: patch
"#;

    fn consumer_yaml(require_ready: bool) -> String {
        format!(
            r#"
resource:
  name: monitor_instance
  service: openscale
  kind: monitor_instance
  version: v1
  api:
    base_path: /v2/monitor_instances
    id_field: id
    get_endpoint: /v2/monitor_instances/{{id}}
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
    - name: data_mart_id
      type: string
      references:
        resource: data_mart
        field: id
        require_ready: {require_ready}
  reconciliation:
    discovery:
      method: list_and_get
    update_strategy: patch
"#
        )
    }

    #[test]
    fn read_state_path_reads_nested_and_missing() {
        let body = json!({"entity": {"status": {"state": "active"}}});
        assert_eq!(read_state_path(&body, "entity.status.state").as_deref(), Some("active"));
        assert_eq!(read_state_path(&body, "entity.status.missing"), None);
    }

    #[test]
    fn classify_maps_ready_failed_pending() {
        let r = readiness(&["active"], &["error", "disabled"], 300, 5);
        assert!(matches!(classify("active", &r, "data_mart", "dm-1"), ReadyOutcome::Ready));
        assert!(matches!(classify("error", &r, "data_mart", "dm-1"), ReadyOutcome::Failed(_)));
        assert!(matches!(classify("preparing", &r, "data_mart", "dm-1"), ReadyOutcome::Pending));
    }

    #[test]
    fn poll_budget_uses_default_and_floors_interval() {
        let b = poll_budget(&readiness(&["active"], &[], 300, 5));
        assert_eq!(b.secs, 300);
        assert_eq!(b.max_attempts, 60);
        // interval 0 is floored to 1s so max_attempts never divides by zero.
        let z = poll_budget(&readiness(&["active"], &[], 3, 0));
        assert_eq!(z.interval, Duration::from_secs(1));
        assert_eq!(z.max_attempts, 3);
    }

    #[tokio::test(start_paused = true)]
    async fn preparing_then_active_proceeds() {
        let t = target(readiness(&["active"], &["error"], 300, 5));
        let calls = AtomicUsize::new(0);
        let script = ["preparing", "preparing", "active"];
        let result = poll_target_ready(&t, || {
            let i = calls.fetch_add(1, Ordering::SeqCst);
            let s = script.get(i).copied().unwrap_or(script[script.len() - 1]);
            async move { Ok(json!({"entity": {"status": {"state": s}}})) }
        })
        .await;
        assert!(result.is_ok());
        assert_eq!(calls.load(Ordering::SeqCst), 3, "polls until the ready value is observed");
    }

    #[tokio::test(start_paused = true)]
    async fn failed_state_bails_immediately() {
        let t = target(readiness(&["active"], &["error", "disabled"], 300, 5));
        let calls = AtomicUsize::new(0);
        let result = poll_target_ready(&t, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(json!({"entity": {"status": {"state": "error"}}})) }
        })
        .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("entered failure state"), "got: {err}");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "bails on the first failed observation");
    }

    #[tokio::test(start_paused = true)]
    async fn never_ready_times_out_with_clear_error() {
        let t = target(readiness(&["active"], &["error"], 10, 5));
        let calls = AtomicUsize::new(0);
        let result = poll_target_ready(&t, || {
            calls.fetch_add(1, Ordering::SeqCst);
            async move { Ok(json!({"entity": {"status": {"state": "preparing"}}})) }
        })
        .await;
        let err = result.unwrap_err().to_string();
        assert!(err.contains("did not reach ready state"), "got: {err}");
        assert!(err.contains("data_mart") && err.contains("dm-1"), "names kind + id: {err}");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "polls up to the budget (10s / 5s)");
    }

    #[test]
    fn collect_targets_selects_require_ready_reference() {
        let data_mart = desc(DATA_MART_YAML);
        let consumer = desc(&consumer_yaml(true));
        let lookup = |kind: &str| if kind == "data_mart" { Some(data_mart.clone()) } else { None };
        let targets = collect_ready_targets(&consumer, &json!({"data_mart_id": "dm-1"}), &lookup);
        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].kind, "data_mart");
        assert_eq!(targets[0].id, "dm-1");
        assert_eq!(targets[0].get_endpoint, "/v2/data_marts/{id}");
        assert_eq!(targets[0].id_field, "id");
    }

    #[test]
    fn collect_targets_empty_without_require_ready() {
        let data_mart = desc(DATA_MART_YAML);
        let consumer = desc(&consumer_yaml(false));
        let lookup = |kind: &str| if kind == "data_mart" { Some(data_mart.clone()) } else { None };
        let targets = collect_ready_targets(&consumer, &json!({"data_mart_id": "dm-1"}), &lookup);
        assert!(targets.is_empty(), "no require_ready => no target => no GET");
    }
}
