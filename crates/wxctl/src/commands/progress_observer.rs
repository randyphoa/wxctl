use parking_lot::Mutex;
use std::sync::Arc;
use std::time::Duration;
use wxctl_core::ResourceKey;
use wxctl_core::logging::ErrorEvent;
use wxctl_engine::ExecutionObserver;
use wxctl_sdk::TestObserver;

use crate::output::OutputCollector;

pub struct CliProgressObserver {
    collector: Arc<Mutex<OutputCollector>>,
}

impl CliProgressObserver {
    pub fn new(collector: Arc<Mutex<OutputCollector>>) -> Self {
        Self { collector }
    }
}

impl ExecutionObserver for CliProgressObserver {
    fn on_task_start(&self, key: &ResourceKey) {
        // Build the plan under lock, then execute (multi.add) outside it.
        let plan = self.collector.lock().log_start(&key.kind, &key.name);
        let (k, pb, row_id) = plan.execute();
        self.collector.lock().install_exec_spinner_pb(k, pb, row_id);
    }

    fn on_task_complete(&self, key: &ResourceKey, success: bool, duration: Duration, response: Option<&serde_json::Value>) {
        // Pull the backend-assigned id out of the create/update response for the
        // Execution row's `[id=…]` suffix (Terraform-style). None on delete/failure.
        let id = response.and_then(extract_resource_id);
        // Detach spinner under lock, then clear (finish_and_clear) outside it.
        let plan = self.collector.lock().record_operation(&key.kind, &key.name, success, duration, id);
        plan.execute();
    }

    fn on_task_skipped(&self, key: &ResourceKey, reason: &str) {
        self.collector.lock().record_skipped(&key.kind, &key.name, reason);
    }

    fn on_task_error(&self, key: &ResourceKey, error: &str) {
        let event = ErrorEvent {
            operation_id: String::new(),
            stage: "execution".to_string(),
            error_code: wxctl_core::logging::error_codes::E001.to_string(),
            resource_type: Some(key.kind.to_string()),
            resource_name: Some(key.name.to_string()),
            field_path: None,
            message: error.to_string(),
            cause: None,
            caused_by: None,
            expected: None,
            actual: None,
            context: None,
            fix: "Check the error message and fix the resource configuration".to_string(),
        };
        self.collector.lock().add_error(event);
    }

    fn on_reconcile_start(&self, total: usize) {
        self.collector.lock().on_reconcile_start(total);
    }

    fn on_reconcile_resource_start(&self, key: &ResourceKey) {
        self.collector.lock().reconcile_resource_start(&key.kind, &key.name);
    }

    fn on_reconcile_resource_complete(&self, key: &ResourceKey, _success: bool) {
        let _ = key;
        self.collector.lock().reconcile_resource_complete();
    }
}

/// Extract the backend-assigned resource id from a create/update response for the
/// Execution row's `[id=…]` suffix. Mirrors `extract_resource_url` (apply.rs): tries
/// a list of candidate id fields in priority order. Prefers the server-assigned UUID
/// (`id`, or `connection_id` for orchestrate connections) over config-supplied logical
/// names like `app_id`, which the row already shows as the resource name. Returns
/// `None` when none are present (e.g. delete responses).
fn extract_resource_id(response: &serde_json::Value) -> Option<String> {
    super::common::first_string_field(response, &["id", "connection_id", "agent_id", "tool_id", "_id"])
}

pub struct CliTestObserver {
    collector: Arc<Mutex<OutputCollector>>,
}

impl CliTestObserver {
    pub fn new(collector: Arc<Mutex<OutputCollector>>) -> Self {
        Self { collector }
    }
}

impl TestObserver for CliTestObserver {
    fn on_test_start(&self, test_name: &str) {
        // Build the plan under lock, then execute (multi.add) outside it.
        let plan = self.collector.lock().log_test_start(test_name);
        let (k, pb, row_id) = plan.execute();
        self.collector.lock().install_exec_spinner_pb(k, pb, row_id);
    }

    fn on_test_complete(&self, test_name: &str, passed: bool, completed: usize, total: usize) {
        // Detach spinner under lock, then clear (finish_and_clear) outside it.
        let plan = self.collector.lock().record_test_complete(test_name, passed, completed, total);
        plan.execute();
    }
}

#[cfg(test)]
mod tests {
    use super::extract_resource_id;
    use serde_json::json;

    #[test]
    fn extract_resource_id_picks_id_fields_else_none() {
        // tool/agent create responses: {"id": "<uuid>"}
        assert_eq!(extract_resource_id(&json!({"id": "805591f1"})).as_deref(), Some("805591f1"));
        assert_eq!(extract_resource_id(&json!({"id": "5d59f99e", "is_update": false})).as_deref(), Some("5d59f99e"));
        // orchestrate_connection: no `id`, but a server-assigned `connection_id`.
        // `app_id` is the config-supplied logical name (already the row's name) — not chosen.
        assert_eq!(extract_resource_id(&json!({"app_id": "httpbin-bearer", "connection_id": "dd33a261"})).as_deref(), Some("dd33a261"));
        // None when there is no id-shaped field, or it's empty.
        assert_eq!(extract_resource_id(&json!({"detail": "uploaded"})), None);
        assert_eq!(extract_resource_id(&json!({"id": ""})), None);
        assert_eq!(extract_resource_id(&json!({})), None);
    }
}
