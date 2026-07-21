use serde_json::Value;

/// Sensitive field names to redact
const SENSITIVE_FIELDS: &[&str] = &["password", "token", "secret", "key", "auth", "authorization", "api_key", "apikey", "access_token", "refresh_token"];

/// Redact sensitive fields from JSON value based on a keyword heuristic.
/// First line of defence; `redact_by_schema` layers precision on top.
pub fn redact_sensitive(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut redacted = serde_json::Map::new();
            for (k, v) in map {
                let key_lower = k.to_lowercase();
                let is_sensitive = SENSITIVE_FIELDS.iter().any(|s| key_lower.contains(s));
                if is_sensitive {
                    redacted.insert(k.clone(), Value::String("***REDACTED***".to_string()));
                } else {
                    redacted.insert(k.clone(), redact_sensitive(v));
                }
            }
            Value::Object(redacted)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(redact_sensitive).collect()),
        _ => value.clone(),
    }
}

/// Redact fields whose dotted path appears in `sensitive_paths`.
/// Complements `redact_sensitive` when the schema marks fields explicitly
/// — precise, and masks fields whose names don't match the keyword list.
/// Array indices are skipped in the path (arrays are traversed but the
/// path does not gain an `[i]` segment), matching the dotted-path syntax
/// used elsewhere in the schema (`connection.password`, not
/// `connection.password[0]`).
pub fn redact_by_schema(value: &Value, sensitive_paths: &[String]) -> Value {
    if sensitive_paths.is_empty() {
        return value.clone();
    }
    redact_by_schema_at(value, sensitive_paths, "")
}

fn redact_by_schema_at(value: &Value, paths: &[String], current: &str) -> Value {
    match value {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map {
                let child = if current.is_empty() { k.clone() } else { format!("{current}.{k}") };
                if paths.iter().any(|p| p == &child) {
                    out.insert(k.clone(), Value::String("***".to_string()));
                } else {
                    out.insert(k.clone(), redact_by_schema_at(v, paths, &child));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(|item| redact_by_schema_at(item, paths, current)).collect()),
        _ => value.clone(),
    }
}

/// Redaction for debug "hook payload diff" logs: schema-precise first, keyword
/// heuristic second (the layering the `redact_sensitive` doc describes). The
/// keyword pass alone is fooled by `{key, value}` secret bundles — e.g. Concert's
/// `credentials: [{key: "access_token", value: "<secret>"}]`, where the field
/// literally named `key` trips the "key" keyword (masking the harmless key name)
/// while the field named `value` holds the real secret and matches no keyword.
/// Passing the schema's `sensitive_paths` (here `credentials.value`) masks the
/// secret precisely regardless of its field name.
pub fn redact_for_log(value: &Value, sensitive_paths: &[String]) -> Value {
    redact_sensitive(&redact_by_schema(&mask_ref_enrichment(value), sensitive_paths))
}

/// Mask engine-injected `__ref__<field>` enrichment subtrees wholesale. The
/// enrichment (see `REF_ENRICH_PREFIX` in wxctl-engine execution/resolution.rs /
/// `REF_PREFIX` in wxctl-providers util.rs) embeds FULL linked-resource specs —
/// including the *referenced* kind's sensitive fields, which the logging
/// resource's own `sensitive_paths` can never cover (live-pinned 2026-07-06: a
/// wml_deployment pre_create hook diff carried the job's plaintext
/// TRAINING_APIKEY at `__ref__asset.__ref__job_run.__ref__job…configuration.env_variables`,
/// where neither the keyword heuristic — the values are "NAME=value" strings under
/// `env_variables` — nor wml_deployment's schema paths could reach it). Hook
/// diffs are about what a hook changed in the resource's own data; enrichment is
/// read-only input, so masking whole subtrees loses nothing that matters.
fn mask_ref_enrichment(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| if k.starts_with("__ref__") { (k.clone(), Value::String("***".to_string())) } else { (k.clone(), mask_ref_enrichment(v)) }).collect()),
        Value::Array(arr) => Value::Array(arr.iter().map(mask_ref_enrichment).collect()),
        _ => value.clone(),
    }
}

use wxctl_schema::ir::FieldIr;

/// Collect dotted paths of fields marked `sensitive: true` from a field slice,
/// recursing into nested object schemas. Mirrors `SchemaDefinition::sensitive_paths`
/// but operates on a bare field slice (what the request materializer holds).
///
/// Each sensitive field emits BOTH its wxctl-name path and its `api_field` path
/// (when they differ): request bodies are keyed by `api_field` (see
/// `RequestMaterializer::materialize`), while discovery responses and other
/// name-keyed sinks use the wxctl name. The superset redacts harmlessly wherever
/// only one of the two keys appears.
pub fn sensitive_paths_from_fields(fields: &[FieldIr]) -> Vec<String> {
    let mut out = Vec::new();
    collect(fields, &[String::new()], &mut out);
    return out;

    fn collect(fields: &[FieldIr], prefixes: &[String], out: &mut Vec<String>) {
        for field in fields {
            let mut segments: Vec<&str> = vec![field.name];
            if let Some(api) = field.api_field
                && api != field.name
            {
                segments.push(api);
            }
            let paths: Vec<String> = prefixes.iter().flat_map(|p| segments.iter().map(move |s| if p.is_empty() { (*s).to_string() } else { format!("{p}.{s}") })).collect();
            if field.sensitive {
                out.extend(paths.iter().cloned());
            }
            if let Some(inner) = field.schema {
                collect(inner.fields, &paths, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_schema::ir::{FieldLocationIr, FieldTypeIr, SchemaBodyIr};

    /// Minimal `FieldIr` literal builder for tests: a `Body`/`String` field with
    /// every other attribute at its zero value. `FieldIr` derives neither `Clone`
    /// nor `Copy` (nested `ValidationIr`/`FieldReferencesIr` don't either), so
    /// tests build a fresh instance per field via this helper + struct-update
    /// syntax rather than cloning a shared base.
    fn field_ir(name: &'static str) -> FieldIr {
        FieldIr {
            name,
            field_type: FieldTypeIr::String,
            required: false,
            immutable: false,
            location: FieldLocationIr::Body,
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references: None,
            api_field: None,
            sensitive: false,
            also_query: false,
            is_path: false,
            synthesize: None,
            synth_shape: None,
        }
    }

    #[test]
    fn redact_by_schema_masks_listed_paths_only() {
        // Top-level listed path masked, siblings retained.
        let v = json!({"name": "foo", "api_key": "12345"});
        let out = redact_by_schema(&v, &["api_key".into()]);
        assert_eq!(out["api_key"], json!("***"));
        assert_eq!(out["name"], json!("foo"));

        // Nested dotted path masked, sibling retained.
        let v = json!({"connection": {"host": "h", "password": "p"}});
        let out = redact_by_schema(&v, &["connection.password".into()]);
        assert_eq!(out["connection"]["password"], json!("***"));
        assert_eq!(out["connection"]["host"], json!("h"));

        // Unlisted field left untouched (precise — no keyword heuristic here).
        let v = json!({"credit_card": "4111"});
        let out = redact_by_schema(&v, &["password".into()]);
        assert_eq!(out["credit_card"], json!("4111"));

        // Arrays traversed: index is skipped in the dotted path, so every element matches.
        let v = json!({"connections": [{"password": "p1"}, {"password": "p2"}]});
        let out = redact_by_schema(&v, &["connections.password".into()]);
        assert_eq!(out["connections"][0]["password"], json!("***"));
        assert_eq!(out["connections"][1]["password"], json!("***"));
    }

    #[test]
    fn redact_sensitive_still_catches_keyword_hits() {
        let v = json!({"api_key": "secret", "normal": "plain"});
        let out = redact_sensitive(&v);
        assert_eq!(out["api_key"], json!("***REDACTED***"));
        assert_eq!(out["normal"], json!("plain"));
    }

    #[test]
    fn redact_for_log_masks_key_value_secret_bundle() {
        // Concert credential shape: keyword-only redaction leaks `value` (the secret)
        // while over-masking `key`. redact_for_log with the schema path fixes it.
        let body = json!({"type": "github", "credentials": [{"key": "access_token", "value": "SEEDED-SECRET"}, {"key": "base_url", "value": "https://host/api/v1"}]});
        let out = redact_for_log(&body, &["credentials.value".to_string()]);
        let s = serde_json::to_string(&out).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "credential secret leaked: {s}");
        // The non-secret base_url value is also masked by the schema path (same field), acceptable for a debug log.
        assert!(s.contains("\"type\":\"github\""), "non-sensitive field retained: {s}");
    }

    #[test]
    fn redact_for_log_masks_bare_array_list_response() {
        // AC6 regression: a discovery LIST response for instana_alerting_channel is a
        // BARE ARRAY (no envelope object) of channel objects that echo `webhookUrls`
        // straight back from the API. redact_by_schema's array recursion keeps the
        // path prefix unchanged across array elements, so a top-level `webhookUrls`
        // path matches inside every `[{...}]` item without an `[i]` segment.
        let body = json!([{"name": "c1", "webhookUrls": ["https://hooks.example.invalid/secret"]}]);
        let out = redact_for_log(&body, &["webhookUrls".to_string()]);
        assert_eq!(out[0]["webhookUrls"], json!("***"));
        assert_eq!(out[0]["name"], json!("c1"));
        let s = serde_json::to_string(&out).unwrap();
        assert!(!s.contains("hooks.example.invalid"), "webhook URL leaked: {s}");
    }

    #[test]
    fn redact_for_log_masks_ref_enrichment_subtrees() {
        // Live-shaped leak (2026-07-06): wml_deployment pre_create hook diff carrying
        // the parent job's env wire-strings through the nested __ref__ chain — no
        // keyword hit ("env_variables"), no schema-path hit (wrong kind's schema).
        let body = json!({"name": "ml_online", "asset": "promoted-id", "__ref__asset": {"id": "promoted-id", "__ref__job_run": {"__ref__job": {"entity": {"job": {"configuration": {"env_variables": ["TRAINING_APIKEY=SEEDED-SECRET", "TRAINING_WML_URL=https://x"]}}}}}}});
        let out = redact_for_log(&body, &[]);
        let s = serde_json::to_string(&out).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "enrichment secret leaked: {s}");
        assert_eq!(out["__ref__asset"], json!("***"), "enrichment subtree must be masked wholesale");
        assert_eq!(out["name"], json!("ml_online"), "own fields retained");

        // Enrichment nested inside arrays is masked too.
        let body = json!({"items": [{"__ref__connection": {"password": "SEEDED-SECRET"}}]});
        let s = serde_json::to_string(&redact_for_log(&body, &[])).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "array-nested enrichment leaked: {s}");
    }

    #[test]
    fn schema_then_keyword_double_redacts() {
        let body = serde_json::json!({"username": "u", "api_key": "SEEDED-SECRET", "nested": {"password": "SEEDED-SECRET"}});
        let by_schema = redact_by_schema(&body, &["nested.password".to_string()]);
        let out = redact_sensitive(&by_schema);
        let s = serde_json::to_string(&out).unwrap();
        assert!(!s.contains("SEEDED-SECRET"), "secret leaked: {s}");
        assert!(s.contains("\"username\":\"u\""), "non-sensitive retained: {s}");
    }

    #[test]
    fn sensitive_paths_from_fields_collects_nested() {
        // Nested sensitive field under a non-sensitive parent, plus a top-level
        // sensitive field: only the sensitive leaves emit paths.
        let pwd_nested = FieldIr { sensitive: true, ..field_ir("password") };
        let host = FieldIr { sensitive: false, ..field_ir("host") };
        let schema_body: &'static SchemaBodyIr = Box::leak(Box::new(SchemaBodyIr { fields: Box::leak(vec![host, pwd_nested].into_boxed_slice()), discriminator: None, variants: None }));
        let conn = FieldIr { field_type: FieldTypeIr::Object, sensitive: false, schema: Some(schema_body), ..field_ir("connection") };
        let api_key = FieldIr { sensitive: true, ..field_ir("api_key") };
        let paths = sensitive_paths_from_fields(&[conn, api_key]);
        assert!(paths.contains(&"connection.password".to_string()));
        assert!(paths.contains(&"api_key".to_string()));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn sensitive_paths_from_fields_emits_api_field_variant() {
        // Top-level sensitive field renamed in the request body via api_field
        // (api_field may itself be a dotted path).
        let key = FieldIr { api_field: Some("credentials.apiKey"), sensitive: true, ..field_ir("api_key") };
        // Nested sensitive field under a renamed parent: both parent segments prefix the child.
        let pwd = FieldIr { sensitive: true, ..field_ir("password") };
        let schema_body: &'static SchemaBodyIr = Box::leak(Box::new(SchemaBodyIr { fields: Box::leak(vec![pwd].into_boxed_slice()), discriminator: None, variants: None }));
        let conn = FieldIr { field_type: FieldTypeIr::Object, api_field: Some("connectionProperties"), schema: Some(schema_body), ..field_ir("connection") };
        let paths = sensitive_paths_from_fields(&[key, conn]);
        for expected in ["api_key", "credentials.apiKey", "connection.password", "connectionProperties.password"] {
            assert!(paths.contains(&expected.to_string()), "missing {expected}: {paths:?}");
        }
        assert_eq!(paths.len(), 4);
    }
}
