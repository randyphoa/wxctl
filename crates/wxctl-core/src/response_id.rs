//! Response-id extraction — the single home for pulling a backend-assigned id
//! (or a chosen field) out of a create/get/list-item JSON response.
//!
//! IBM APIs wrap ids inconsistently: SaaS common-core puts them at
//! `metadata.id`, on-prem/software puts them at the top level, and some
//! endpoints nest under `entity`. These helpers capture the three extraction
//! shapes the codebase relies on — deliberately distinct because their
//! precedence differs:
//!
//! - [`resource_id`] — `metadata.id` then top-level `id` (SaaS-first).
//! - [`resource_id_field`] — top-level `<field>` then `metadata.<field>` then
//!   `entity.<field>` (top-level-first, arbitrary field name). The field's
//!   value may be a JSON string or integer; both stringify to the returned id.
//! - [`first_string_field`] — first non-empty string among a priority list of
//!   top-level field names.

use serde_json::Value;

/// Extract a resource id from a create/get response, handling both SaaS
/// (`metadata.id`) and on-prem (`id`) shapes. Metadata wins when both are
/// present.
pub fn resource_id(value: &Value) -> Option<&str> {
    value.pointer("/metadata/id").or_else(|| value.get("id")).and_then(|v| v.as_str())
}

/// Extract an arbitrary id field, trying the top level first, then the CP4D
/// `metadata.<field>` and `entity.<field>` envelopes. Used by the engine's
/// delete/update/recreate paths where the id field name is schema-driven.
///
/// The id value may be a JSON string or integer (e.g. `agent_release`'s
/// `version` field) — integers are stringified. Other JSON types (bool,
/// float, object, array, null) are not valid ids and yield `None`.
pub fn resource_id_field(value: &Value, field: &str) -> Option<String> {
    let found = value.get(field).or_else(|| value.get("metadata").and_then(|m| m.get(field))).or_else(|| value.get("entity").and_then(|e| e.get(field)))?;
    found.as_str().map(str::to_string).or_else(|| found.as_i64().map(|n| n.to_string())).or_else(|| found.as_u64().map(|n| n.to_string()))
}

/// Return the first non-empty string value among `fields`, probed in order at
/// the top level. Empty strings are skipped (treated as absent). Used to pick a
/// server-assigned id or url from a response that may carry any of several
/// candidate field names.
pub fn first_string_field(value: &Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| value.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resource_id_prefers_metadata_then_top_level() {
        assert_eq!(resource_id(&json!({"metadata": {"id": "m-1"}, "id": "top"})), Some("m-1"), "metadata.id wins over top-level id");
        assert_eq!(resource_id(&json!({"id": "top"})), Some("top"), "falls back to top-level id");
        assert_eq!(resource_id(&json!({"entity": {"id": "e"}})), None, "does not look under entity");
        assert_eq!(resource_id(&json!({})), None);
    }

    #[test]
    fn resource_id_field_prefers_top_level_then_envelopes() {
        // Opposite precedence to resource_id: top-level wins.
        assert_eq!(resource_id_field(&json!({"asset_id": "top", "metadata": {"asset_id": "m"}}), "asset_id").as_deref(), Some("top"));
        assert_eq!(resource_id_field(&json!({"metadata": {"asset_id": "m"}}), "asset_id").as_deref(), Some("m"));
        assert_eq!(resource_id_field(&json!({"entity": {"asset_id": "e"}}), "asset_id").as_deref(), Some("e"));
        // metadata beats entity.
        assert_eq!(resource_id_field(&json!({"metadata": {"asset_id": "m"}, "entity": {"asset_id": "e"}}), "asset_id").as_deref(), Some("m"));
        assert_eq!(resource_id_field(&json!({}), "asset_id"), None);
    }

    /// `agent_release`'s id_field is `version`, an integer — the engine's
    /// delete/update/recreate paths must not fail with "Missing ID field"
    /// just because the id is a JSON number rather than a string.
    #[test]
    fn resource_id_field_stringifies_integer_ids() {
        assert_eq!(resource_id_field(&json!({"version": 2}), "version").as_deref(), Some("2"), "top-level integer id");
        assert_eq!(resource_id_field(&json!({"metadata": {"version": 3}}), "version").as_deref(), Some("3"), "integer id in metadata envelope");
        assert_eq!(resource_id_field(&json!({"entity": {"version": 4}}), "version").as_deref(), Some("4"), "integer id in entity envelope");
        // Negative and large (u64-only) integers both stringify.
        assert_eq!(resource_id_field(&json!({"version": -1}), "version").as_deref(), Some("-1"));
        assert_eq!(resource_id_field(&json!({"version": u64::MAX}), "version").as_deref(), Some(u64::MAX.to_string().as_str()));
    }

    /// Only strings and integers are valid ids — floats, bools, objects,
    /// arrays, and null must not be silently stringified.
    #[test]
    fn resource_id_field_rejects_non_string_non_integer_types() {
        assert_eq!(resource_id_field(&json!({"version": 1.5}), "version"), None, "float is not a valid id");
        assert_eq!(resource_id_field(&json!({"version": true}), "version"), None, "bool is not a valid id");
        assert_eq!(resource_id_field(&json!({"version": {"nested": 1}}), "version"), None, "object is not a valid id");
        assert_eq!(resource_id_field(&json!({"version": [1, 2]}), "version"), None, "array is not a valid id");
        assert_eq!(resource_id_field(&json!({"version": null}), "version"), None, "null is not a valid id");
    }

    #[test]
    fn first_string_field_skips_empty_and_missing() {
        assert_eq!(first_string_field(&json!({"id": "805591f1"}), &["id", "connection_id"]).as_deref(), Some("805591f1"));
        // Empty string is skipped, next candidate chosen.
        assert_eq!(first_string_field(&json!({"id": "", "connection_id": "dd33"}), &["id", "connection_id"]).as_deref(), Some("dd33"));
        assert_eq!(first_string_field(&json!({"detail": "x"}), &["id", "connection_id"]), None);
        assert_eq!(first_string_field(&json!({"id": ""}), &["id"]), None);
    }
}
