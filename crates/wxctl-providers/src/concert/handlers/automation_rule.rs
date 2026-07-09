//! `automation_rule` handler — Concert's automation-rule API has NO item DELETE. Deletion
//! is a COLLECTION operation: `DELETE /core/api/v1/automation_rules?delete_ids={id}` — the
//! same shape as `concert_credential`. The schema's `delete_endpoint` points at the
//! collection, but the item id must ride as a `delete_ids` query param, which the default
//! delete path cannot add. So `AutomationRuleHandler` owns the DELETE via `pre_delete`
//! returning `HookOutcome::Handled` — which makes `wxctl-engine`'s `delete.rs` skip the
//! default DELETE (it checks for `Handled` before id extraction). It reads the discovered
//! `id`, issues the collection DELETE with the `delete_ids` query, and tolerates an
//! already-absent id (404 → success) so destroy is idempotent. The tolerated 404 is
//! marked `.not_found_ok()` on the `RequestSpec` so it does NOT log a spurious WXCTL-H001
//! error / report a false destroy failure
//! (docs/troubleshoot/destroy-reports-failure-on-tolerated-cascade-400-fix.md).
//!
//! Update is schema-driven (update_method: PATCH + update_strategy: replace → a full JSON
//! body pruned to state_fields, i.e. a merge-patch of those fields) — no update hook here.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::common::collection_delete_by_id;

const AUTOMATION_RULES_PATH: &str = "/core/api/v1/automation_rules";

pub struct AutomationRuleHandler;

impl ResourceHandler for AutomationRuleHandler {
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(collection_delete_by_id(client, operation_id, AUTOMATION_RULES_PATH, resource, "concert_automation_rule"))
    }
}
