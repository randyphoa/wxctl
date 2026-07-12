//! `pa_chore` handler — a TM1 chore schedules an ordered list of tasks, each binding a
//! process via the OData `Tasks[].Process@odata.bind` key (dotted -> inexpressible as a
//! declared `api_field`, dropped by the default materializer:
//! docs/troubleshoot/pre-create-body-reshape-dropped-fix.md). So this handler OWNS both the
//! create POST and the update PATCH, building the bind body itself. A chore's active state is
//! not a writable body property — it is toggled by the `tm1.Activate` / `tm1.Deactivate`
//! OData actions — so the handler reconciles the declared `active` field via those actions
//! after writing the body (a chore must be inactive to be modified, so pre_update deactivates
//! first). pre_delete deactivates before the default DELETE.
//!
//! NOTE (confirm live in Phase 4): the TM1 REST OpenAPI's generated `ChoreTask-create` shows
//! a `Chore@odata.bind` reverse-nav artifact; the task->process bind key used here is
//! `Process@odata.bind` per the spec. Confirm the exact key + bind-URI form (relative
//! `Processes('<name>')`) against the live gateway.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct ChoreHandler;

impl ResourceHandler for ChoreHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = chore_name(resource)?.to_string();
            let body = build_chore_body(resource)?;
            let spec = RequestSpec::new(Method::POST, endpoint).body(BodyKind::Json(body));
            let mut response: Value = client.execute(operation_id, spec).await?;
            // A chore is created inactive; activate it if desired (activation is an action, not
            // a body field, so it must happen here — Handled skips the default post_create).
            if is_active(resource) {
                activate(client, operation_id, &name).await?;
            }
            set_active(&mut response, is_active(resource));
            Ok(HookOutcome::Handled(response))
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = chore_name(desired)?.to_string();
            // TM1 rejects edits to an active chore, so deactivate before the PATCH.
            if is_active(current) {
                deactivate(client, operation_id, &name).await?;
            }
            let body = build_chore_body(desired)?;
            // Handler-owned PATCH bypasses the default update flow, which injects the id
            // path_var — set it here or the endpoint template's '{name}' reaches TM1 literally.
            let spec = RequestSpec::new(Method::PATCH, endpoint).body(BodyKind::Json(body)).path_var("name", &name);
            let mut response: Value = client.execute(operation_id, spec).await?;
            if is_active(desired) {
                activate(client, operation_id, &name).await?;
            }
            set_active(&mut response, is_active(desired));
            Ok(HookOutcome::Handled(response))
        })
    }

    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // An active chore cannot be deleted; deactivate first (idempotent even if already
            // inactive) — best-effort, since delete shouldn't fail on a deactivate hiccup —
            // then let the default DELETE run.
            if let Ok(name) = chore_name(resource) {
                let _ = deactivate(client, operation_id, name).await;
            }
            Ok(HookOutcome::Continue)
        })
    }
}

fn chore_name(resource: &Value) -> Result<&str> {
    resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_chore requires a 'name' field"))
}

/// True when the resource's declared `active` field is set true (defaults false).
fn is_active(resource: &Value) -> bool {
    resource.get("active").and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Reflect the reconciled active state onto the response so the merged record matches desired.
fn set_active(response: &mut Value, active: bool) {
    if let Value::Object(map) = response {
        map.insert("Active".to_string(), json!(active));
    }
}

/// Build the create/update body from the declared chore resource. `Active` is intentionally
/// omitted — activation is an action, not a body field. Each task's `process` name becomes
/// `Process@odata.bind: "Processes('<name>')"`.
fn build_chore_body(resource: &Value) -> Result<Value> {
    let name = chore_name(resource)?;
    let mut body = Map::new();
    body.insert("Name".to_string(), json!(name));
    for (field, key) in [("start_time", "StartTime"), ("execution_mode", "ExecutionMode"), ("frequency", "Frequency")] {
        if let Some(v) = resource.get(field).and_then(|v| v.as_str()) {
            body.insert(key.to_string(), json!(v));
        }
    }
    if let Some(b) = resource.get("dst_sensitive").and_then(|v| v.as_bool()) {
        body.insert("DSTSensitive".to_string(), json!(b));
    }
    if let Some(tasks) = resource.get("tasks").and_then(|v| v.as_array()) {
        let built: Result<Vec<Value>> = tasks.iter().map(build_task).collect();
        body.insert("Tasks".to_string(), Value::Array(built?));
    }
    Ok(Value::Object(body))
}

/// Build one `ChoreTask` wire object: `Step`, the `Process@odata.bind` reference, and optional
/// `Parameters` (Name/Value pairs).
fn build_task(task: &Value) -> Result<Value> {
    let step = task.get("step").cloned().ok_or_else(|| anyhow!("pa_chore task requires a 'step'"))?;
    let process = task.get("process").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("pa_chore task requires a 'process'"))?;
    let mut t = Map::new();
    t.insert("Step".to_string(), step);
    t.insert("Process@odata.bind".to_string(), json!(format!("Processes('{process}')")));
    if let Some(params) = task.get("parameters").and_then(|v| v.as_array()) {
        let built: Vec<Value> = params
            .iter()
            .map(|p| {
                let mut m = Map::new();
                if let Some(n) = p.get("name") {
                    m.insert("Name".to_string(), n.clone());
                }
                if let Some(v) = p.get("value") {
                    m.insert("Value".to_string(), v.clone());
                }
                Value::Object(m)
            })
            .collect();
        t.insert("Parameters".to_string(), Value::Array(built));
    }
    Ok(Value::Object(t))
}

/// POST `/Chores('{name}')/tm1.Activate`. TM1 rejects a body-less request with 400 error 278
/// ("content type ... not supported") — it requires `Content-Type: application/json` even for
/// an empty payload, so this sends `{}` rather than `BodyKind::None`. The action is idempotent
/// (204 whether the chore is already active or not), so no status tolerance is needed: any
/// error here is real and must propagate.
async fn activate(client: &HttpClient, operation_id: &str, name: &str) -> Result<()> {
    let path = format!("/Chores('{name}')/tm1.Activate");
    let spec = RequestSpec::new(Method::POST, &path).body(BodyKind::Json(json!({})));
    client.execute::<Value>(operation_id, spec).await.map(|_| ())
}

/// POST `/Chores('{name}')/tm1.Deactivate`. Same content-type requirement and idempotency as
/// `activate` — see its doc comment.
async fn deactivate(client: &HttpClient, operation_id: &str, name: &str) -> Result<()> {
    let path = format!("/Chores('{name}')/tm1.Deactivate");
    let spec = RequestSpec::new(Method::POST, &path).body(BodyKind::Json(json!({})));
    client.execute::<Value>(operation_id, spec).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure-function unit tests of the body-builder (no I/O) — matches the co-located test
    // convention of every existing handler (e.g. concert/handlers/source_repo.rs).
    #[test]
    fn build_chore_body_binds_task_process_and_omits_active() {
        let resource = json!({
            "name": "NightlyLoad",
            "start_time": "2026-01-01T00:00:00Z",
            "active": true,
            "execution_mode": "SingleCommit",
            "frequency": "P1DT0H0M0S",
            "tasks": [{"step": 0, "process": "LoadSales", "parameters": [{"name": "pMonth", "value": "Jan"}]}]
        });
        let body = build_chore_body(&resource).expect("body");
        assert_eq!(body.get("Name").and_then(|v| v.as_str()), Some("NightlyLoad"));
        assert_eq!(body.get("ExecutionMode").and_then(|v| v.as_str()), Some("SingleCommit"));
        assert!(!body.as_object().unwrap().contains_key("Active"), "Active is reconciled via actions, never in the body");
        let task = &body.get("Tasks").and_then(|v| v.as_array()).unwrap()[0];
        assert_eq!(task.get("Process@odata.bind").and_then(|v| v.as_str()), Some("Processes('LoadSales')"));
        assert_eq!(task.get("Step").and_then(|v| v.as_i64()), Some(0));
        let param = &task.get("Parameters").and_then(|v| v.as_array()).unwrap()[0];
        assert_eq!(param.get("Name").and_then(|v| v.as_str()), Some("pMonth"));
    }

    #[test]
    fn is_active_defaults_false_and_reads_flag() {
        assert!(!is_active(&json!({"name": "c"})));
        assert!(is_active(&json!({"name": "c", "active": true})));
    }

    #[test]
    fn build_task_requires_process() {
        assert!(build_task(&json!({"step": 1})).is_err());
    }
}
