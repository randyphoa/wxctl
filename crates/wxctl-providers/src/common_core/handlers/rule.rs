use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

/// Singular `rule` handler. The default reconciler creates/updates a rule fine;
/// this handler exists only to normalize a *discovered* rule so re-plan is
/// NoChange. `POST /v3/enforcement/rules` returns the created rule, but on
/// re-discovery the API echoes `trigger`/`action`/`state` in a server-canonical
/// shape (e.g. `action` enriched beyond the local `{name: "Deny"}`, `state`/
/// `trigger` re-projected). `values_match`'s selective object/array compare
/// handles remote-only object keys, but a re-keyed `trigger` array or a hoisted
/// `state` would still read as drift; lift the entity's compared fields to the
/// top level so the compare round-trips. No-op when a field is absent (never
/// fabricates a value that would mask a real diff).
pub struct RuleHandler;

/// Copy a discovered rule's compared fields up from the CP4D `entity` envelope
/// to the top level so `state_fields` compare against the local shape. Only
/// hoists when the top-level key is absent (preserves an already-flat SaaS shape).
fn hoist_rule_entity_fields(remote: &mut Value) {
    let Some(entity) = remote.get("entity").cloned() else { return };
    let Some(entity_map) = entity.as_object() else { return };
    if let Some(obj) = remote.as_object_mut() {
        for field in ["trigger", "action", "state", "governance_type_id", "description", "name"] {
            if !obj.contains_key(field)
                && let Some(v) = entity_map.get(field)
            {
                obj.insert(field.to_string(), v.clone());
            }
        }
    }
}

impl ResourceHandler for RuleHandler {
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            hoist_rule_entity_fields(remote_data);
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, "rule discovered; entity fields hoisted for NoChange compare");
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn hoist_rule_entity_fields_lifts_compared_fields() {
        let mut remote = json!({"metadata": {"guid": "rule-1"}, "entity": {"name": "e2e Restrict PII Access", "trigger": ["$Asset.InferredClassification", "CONTAINS", ["term-1"]], "action": {"name": "Deny"}, "state": "draft", "governance_type_id": "Access"}});
        hoist_rule_entity_fields(&mut remote);
        assert_eq!(remote.get("state").and_then(|v| v.as_str()), Some("draft"));
        assert_eq!(remote.get("action"), Some(&json!({"name": "Deny"})));
        assert_eq!(remote.get("trigger").and_then(|v| v.as_array()).map(|a| a.len()), Some(3));
    }

    #[test]
    fn hoist_rule_entity_fields_is_noop_when_flat() {
        let mut remote = json!({"name": "e2e Restrict PII Access", "state": "draft"});
        hoist_rule_entity_fields(&mut remote);
        assert_eq!(remote.get("state").and_then(|v| v.as_str()), Some("draft"));
        assert!(remote.get("entity").is_none());
    }

    #[test]
    fn hoist_rule_entity_fields_does_not_overwrite_existing_top_level() {
        let mut remote = json!({"state": "active", "entity": {"state": "draft"}});
        hoist_rule_entity_fields(&mut remote);
        assert_eq!(remote.get("state").and_then(|v| v.as_str()), Some("active"));
    }
}
