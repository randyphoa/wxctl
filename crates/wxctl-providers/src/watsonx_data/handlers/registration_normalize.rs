//! `post_discover` normalisation helpers for the watsonx.data registration
//! schemas. `backfill_associated_catalog` is shared between
//! `storage_registration` and `database_registration`; the other two helpers
//! are database-registration-specific and documented as such. Both kinds
//! accept a singular `associated_catalog` object in YAML, but the v3 list/get
//! APIs either return it as a plural `associated_catalogs` array or omit
//! sub-fields that land in `properties[]` — without backfilling, every
//! re-plan flags immutable/state fields as "changed" and proposes
//! Recreate/Update.

use serde_json::{Map, Value};

/// Insert `value` at `obj[key]` only when the current value is absent or an
/// empty string. Leaves non-empty values intact.
fn set_str_if_missing(obj: &mut Map<String, Value>, key: &str, value: String) {
    if obj.get(key).and_then(|v| v.as_str()).is_none_or(str::is_empty) {
        obj.insert(key.to_string(), Value::String(value));
    }
}

/// Copy `catalog_name` and `catalog_type` from `associated_catalogs[0]`
/// into the singular `associated_catalog` object (creating it when absent),
/// and backfill the top-level computed `catalog_name` from the same source.
/// When the response carries only the singular `associated_catalog` object
/// (e.g. v3 `/database_registrations` POST/GET), backfill the top-level
/// `catalog_name` from that instead. Existing non-empty fields on the
/// nested object are left intact.
pub(super) fn backfill_associated_catalog(data: &mut Value) {
    let plural_name = data.pointer("/associated_catalogs/0/catalog_name").and_then(|v| v.as_str()).map(str::to_string);
    let plural_type = data.pointer("/associated_catalogs/0/catalog_type").and_then(|v| v.as_str()).map(str::to_string);

    if let Some(obj) = data.as_object_mut() {
        if plural_name.is_some() || plural_type.is_some() {
            let nested = obj.entry("associated_catalog").or_insert_with(|| Value::Object(Map::new()));
            if let Some(nested_obj) = nested.as_object_mut() {
                if let Some(name) = plural_name.clone() {
                    set_str_if_missing(nested_obj, "catalog_name", name);
                }
                if let Some(kind) = plural_type {
                    set_str_if_missing(nested_obj, "catalog_type", kind);
                }
            }
        }

        // Top-level `catalog_name` sources from whichever form the response carried —
        // the plural array (storage/hdfs list responses) or the singular object
        // (v3 /database_registrations POST). Downstream engines' template refs read it.
        let top_level = plural_name.or_else(|| obj.get("associated_catalog").and_then(|v| v.pointer("/catalog_name")).and_then(|v| v.as_str()).map(str::to_string));
        if let Some(name) = top_level {
            set_str_if_missing(obj, "catalog_name", name);
        }
    }
}

/// v3 `/database_registrations` GET returns `associated_catalog` without
/// `catalog_type`, but that field is marked `immutable` in the schema — absent
/// a backfill, every re-plan sees `desired=db2 vs remote=null` and proposes
/// Recreate. For DB registrations the top-level `type` and the
/// `associated_catalog.catalog_type` are the same connector string, so copy
/// the former into the latter when the response omits it. Storage registrations
/// don't get this treatment: their top-level `type` (`ibm_cos`) and
/// `catalog_type` (`iceberg`/`hive`) are unrelated.
pub(super) fn backfill_db_catalog_type(data: &mut Value) {
    let Some(top_type) = data.pointer("/type").and_then(|v| v.as_str()).map(str::to_string) else { return };
    let Some(obj) = data.as_object_mut() else { return };
    let Some(Value::Object(nested_obj)) = obj.get_mut("associated_catalog") else { return };
    set_str_if_missing(nested_obj, "catalog_type", top_type);
}

/// v3 `/database_registrations` GET hides `connection.username` in the
/// `properties` array (keyed `connection-user`) rather than echoing it under
/// `connection.username`. State-field drift detection then sees
/// `desired="6ec78814" vs remote=null` and proposes a spurious Update. Copy it
/// back onto `connection.username` when the server stashed it in `properties`.
pub(super) fn backfill_connection_username_from_properties(data: &mut Value) {
    let Some(props) = data.pointer("/properties").and_then(|v| v.as_array()) else { return };
    let Some(user) = props.iter().find(|p| p.get("key").and_then(|v| v.as_str()) == Some("connection-user")).and_then(|p| p.get("value")).and_then(|v| v.as_str()).map(str::to_string) else {
        return;
    };
    let Some(obj) = data.as_object_mut() else { return };
    let Some(Value::Object(conn_obj)) = obj.get_mut("connection") else { return };
    set_str_if_missing(conn_obj, "username", user);
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // All `backfill_associated_catalog` cases — each input shape + expected output is
    // load-bearing for NoChange re-plan. `expect` asserts the value at a pointer (None
    // = the key must be absent / no-fabricate).
    #[test]
    fn backfill_associated_catalog_cases() {
        type Case<'a> = (&'a str, Value, &'a [(&'a str, Option<&'a str>)]);
        let cases: &[Case] = &[
            // Plural array → both nested sub-fields + top-level catalog_name backfilled.
            (
                "from associated_catalogs array",
                json!({"id": "abc", "associated_catalogs": [{"catalog_name": "wxctl_iceberg", "catalog_type": "iceberg", "base_path": "/"}]}),
                &[("/associated_catalog/catalog_name", Some("wxctl_iceberg")), ("/associated_catalog/catalog_type", Some("iceberg")), ("/catalog_name", Some("wxctl_iceberg"))],
            ),
            // Existing non-empty nested values are preserved over the plural source.
            (
                "preserves pre-existing nested values",
                json!({"associated_catalogs": [{"catalog_name": "from_array", "catalog_type": "iceberg"}], "associated_catalog": {"catalog_name": "nested_name", "catalog_type": "hive"}}),
                &[("/associated_catalog/catalog_name", Some("nested_name")), ("/associated_catalog/catalog_type", Some("hive"))],
            ),
            // Only the missing nested sub-field is filled.
            ("fills only missing sub-fields", json!({"associated_catalogs": [{"catalog_name": "top", "catalog_type": "iceberg"}], "associated_catalog": {"catalog_type": "hive"}}), &[("/associated_catalog/catalog_name", Some("top")), ("/associated_catalog/catalog_type", Some("hive"))]),
            // No source → nested object never fabricated.
            ("noop when source missing", json!({"id": "abc"}), &[("/associated_catalog", None)]),
            ("noop when source empty array", json!({"id": "abc", "associated_catalogs": []}), &[("/associated_catalog", None)]),
            // Singular nested object alone → top-level catalog_name sources from it (db_registration POST shape).
            ("top-level catalog_name from singular nested", json!({"id": "db-reg-1", "associated_catalog": {"catalog_name": "db2_catalog_v2", "catalog_type": "db2"}}), &[("/catalog_name", Some("db2_catalog_v2"))]),
            // Plural wins over singular for the top-level value.
            ("plural wins over singular for top-level", json!({"associated_catalogs": [{"catalog_name": "from_plural"}], "associated_catalog": {"catalog_name": "from_singular"}}), &[("/catalog_name", Some("from_plural"))]),
        ];
        for (msg, mut data, expectations) in cases.iter().map(|(m, d, e)| (*m, d.clone(), *e)) {
            backfill_associated_catalog(&mut data);
            for (ptr, expected) in expectations {
                assert_eq!(data.pointer(ptr).and_then(|v| v.as_str()), *expected, "{msg}: {ptr}");
            }
        }
    }

    // `backfill_db_catalog_type` copies the top-level connector `type` into the nested
    // `catalog_type` only when absent (immutable-field compare for db registrations).
    #[test]
    fn backfill_db_catalog_type_cases() {
        let cases: &[(&str, Value, Option<&str>)] = &[
            ("copies from top-level type", json!({"id": "db2716", "type": "db2", "associated_catalog": {"catalog_name": "wxctl_probe", "catalog_tags": []}}), Some("db2")),
            ("preserves existing catalog_type", json!({"type": "db2", "associated_catalog": {"catalog_name": "x", "catalog_type": "preset"}}), Some("preset")),
            ("noop without top-level type", json!({"associated_catalog": {"catalog_name": "x"}}), None),
        ];
        for (msg, mut data, expected) in cases.iter().map(|(m, d, e)| (*m, d.clone(), *e)) {
            backfill_db_catalog_type(&mut data);
            assert_eq!(data.pointer("/associated_catalog/catalog_type").and_then(|v| v.as_str()), expected, "{msg}");
        }
    }

    // `backfill_connection_username_from_properties` copies `connection-user` from the
    // properties[] array onto `connection.username` (state-field drift fix), only when
    // missing and only when the connection parent exists.
    #[test]
    fn backfill_connection_username_cases() {
        let cases: &[(&str, Value, Option<&str>)] = &[
            ("backfills from properties array", json!({"connection": {"hostname": "h", "port": 31030, "name": "bludb", "ssl": true}, "properties": [{"key": "connector.name", "value": "db2"}, {"key": "connection-user", "value": "6ec78814"}]}), Some("6ec78814")),
            ("preserves existing username", json!({"connection": {"username": "already_set"}, "properties": [{"key": "connection-user", "value": "other"}]}), Some("already_set")),
            ("noop when properties missing", json!({"connection": {"hostname": "h"}}), None),
            ("noop when connection-user key absent", json!({"connection": {"hostname": "h"}, "properties": [{"key": "connector.name", "value": "db2"}]}), None),
        ];
        for (msg, mut data, expected) in cases.iter().map(|(m, d, e)| (*m, d.clone(), *e)) {
            backfill_connection_username_from_properties(&mut data);
            assert_eq!(data.pointer("/connection/username").and_then(|v| v.as_str()), expected, "{msg}");
        }

        // Distinct branch: no `connection` parent at all → the field is never created.
        let mut no_parent = json!({"properties": [{"key": "connection-user", "value": "u"}]});
        backfill_connection_username_from_properties(&mut no_parent);
        assert!(no_parent.get("connection").is_none(), "connection parent missing → not fabricated");
    }
}
