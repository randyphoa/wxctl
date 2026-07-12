//! Shared helpers for Concert handlers — the resilience kinds (library / profile /
//! posture) are structurally identical apart from one id key + list path, and the
//! collection-delete kinds (credential / automation_rule) share the same
//! `delete_ids` query shape. These helpers hold the single copy of that logic.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Value, json};
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec, error_has_status};
use wxctl_core::traits::HookOutcome;

/// Copy the create response's `source_key` (e.g. `library_id`, `profile_id`,
/// `posture_id`) into the canonical `id` field the schema's `id_field` names.
/// Idempotent: an existing non-empty `id` is left untouched; a missing `source_key`
/// is a no-op (the engine then reports the missing id).
pub(crate) fn map_create_id(response: &mut Value, source_key: &str) {
    if response.get("id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
        return;
    }
    let source_id = response.get(source_key).and_then(|v| v.as_str()).map(str::to_string);
    if let (Some(obj), Some(source_id)) = (response.as_object_mut(), source_id) {
        obj.insert("id".to_string(), Value::String(source_id));
    }
}

/// Search a list response (`{ items: [...] }` envelope or a bare array) for the item
/// whose `name` matches, returning a clone of that object (carrying `id`).
pub(crate) fn find_by_name_in_items(value: &Value, name: &str) -> Option<Value> {
    let items = match value {
        Value::Array(a) => a,
        Value::Object(o) => o.get("items").and_then(|v| v.as_array())?,
        _ => return None,
    };
    items.iter().find(|item| item.get("name").and_then(|v| v.as_str()) == Some(name)).cloned()
}

/// Shared `recover_from_create_error` body: adopt an already-existing resource by
/// listing `list_path` and matching on `name`. Failure semantics matter here — the
/// engine `.await?`s this hook, so any `Err` it returns REPLACES the original create
/// error. A failed recovery list therefore warns and returns `Ok(None)` (the engine
/// then falls back to the real error), and a resource with no `name` to match on
/// returns `Ok(None)` for the same reason.
pub(crate) async fn recover_by_name_from_list(client: &HttpClient, operation_id: &str, list_path: &str, name: Option<&str>, kind_label: &str) -> Result<Option<Value>> {
    let Some(name) = name else {
        return Ok(None);
    };
    // Read every page — an already-existing resource may be beyond page 1.
    match crate::util::fetch_all_pages(client, operation_id, list_path, "items").await {
        Ok(items) => Ok(find_by_name_in_items(&json!({ "items": items }), name)),
        Err(e) => {
            tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, kind = %kind_label, error = %e, "recovery list GET failed — falling back to the original create error");
            Ok(None)
        }
    }
}

/// Delete a Concert resource via its COLLECTION endpoint with a `delete_ids` query
/// (`DELETE {collection_path}?delete_ids={id}`) — Concert's credential and
/// automation-rule APIs have no item DELETE. `not_found_ok()` suppresses the
/// spurious WXCTL-H001 event; the call still returns Err on 404, so an
/// already-absent id maps to a no-op success here so destroy is idempotent.
pub(crate) async fn collection_delete_by_id(client: &HttpClient, operation_id: &str, collection_path: &str, resource: &Value, kind_label: &str) -> Result<HookOutcome> {
    let id = resource.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("{kind_label} delete requires a resolved 'id'"))?.to_string();
    let spec = RequestSpec::new(Method::DELETE, collection_path).query_param("delete_ids", &id).body(BodyKind::None).not_found_ok();
    match client.execute::<Value>(operation_id, spec).await {
        Ok(v) => Ok(HookOutcome::Handled(v)),
        Err(e) if error_has_status(&e, 404) => Ok(HookOutcome::Handled(json!({"deleted": id}))),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn map_create_id_copies_source_key_to_id() {
        let mut resp = json!({"library_id": "lib-123"});
        map_create_id(&mut resp, "library_id");
        assert_eq!(resp.get("id").and_then(|v| v.as_str()), Some("lib-123"));
    }

    #[test]
    fn map_create_id_preserves_existing_id() {
        let mut resp = json!({"id": "canonical", "profile_id": "prof-123"});
        map_create_id(&mut resp, "profile_id");
        assert_eq!(resp.get("id").and_then(|v| v.as_str()), Some("canonical"));
    }

    #[test]
    fn map_create_id_missing_source_key_is_noop() {
        let mut resp = json!({"other": "x"});
        map_create_id(&mut resp, "posture_id");
        assert!(resp.get("id").is_none());
    }

    #[test]
    fn find_by_name_in_items_matches_items_envelope() {
        let listed = json!({"pagination": {}, "items": [{"id": "a", "name": "other"}, {"id": "b", "name": "app-resilience"}]});
        let got = find_by_name_in_items(&listed, "app-resilience").expect("match");
        assert_eq!(got.get("id").and_then(|v| v.as_str()), Some("b"));
    }

    #[test]
    fn find_by_name_in_items_matches_bare_array() {
        let listed = json!([{"id": "a", "name": "other"}, {"id": "b", "name": "wanted"}]);
        let got = find_by_name_in_items(&listed, "wanted").expect("match");
        assert_eq!(got.get("id").and_then(|v| v.as_str()), Some("b"));
    }

    #[test]
    fn find_by_name_in_items_no_match_is_none() {
        let listed = json!({"items": [{"id": "a", "name": "other"}]});
        assert!(find_by_name_in_items(&listed, "missing").is_none());
    }
}
