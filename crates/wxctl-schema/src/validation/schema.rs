use super::error_codes;
use super::types::ValidationError;
use crate::resource::RawResource;
use crate::schema::{FieldDefinition, FieldLocation, ReconciliationDefinition, ResourceSchema, SchemaDefinition, UpdateStrategy};
use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

/// Cache for compiled regex patterns used in field validation.
static REGEX_CACHE: LazyLock<Mutex<HashMap<String, regex::Regex>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Fields that are part of the config system but not defined in resource schemas.
const META_FIELDS: &[&str] = &["kind", "ref_name", "_from_id", "id", "on_destroy", "metadata", "depends_on"];

pub fn apply_defaults(resource: &mut RawResource, schema: &ResourceSchema) {
    let def = &schema.resource;
    let active_variant = active_variant_value(&def.schema, &resource.data);

    // Apply defaults for common fields + fields of the active variant (if any).
    // Inactive-variant defaults are not applied — they have no semantic meaning
    // when the discriminator selects a different variant.
    let fields = fields_for_active(&def.schema, active_variant.as_deref());

    for field in fields {
        if let Some(default_value) = &field.default
            && resource.data.get(&field.name).is_none()
            && let Some(obj) = resource.data.as_object_mut()
        {
            obj.insert(field.name.clone(), default_value.clone());
        }
    }
}

/// Read the discriminator value from the raw resource data, if the schema
/// declares a discriminator and the value is a string. Returns None when the
/// schema has no variants, the discriminator field is missing, or the value
/// is not a string.
fn active_variant_value(schema: &SchemaDefinition, data: &Value) -> Option<String> {
    let disc = schema.discriminator.as_ref()?;
    let value = data.get(disc)?;
    value.as_str().map(str::to_string)
}

/// Fields in effect for the resolved active variant: the variant's fields when the
/// discriminator selects one, else the common top-level fields.
fn fields_for_active<'a>(schema: &'a SchemaDefinition, active_variant: Option<&str>) -> Vec<&'a FieldDefinition> {
    match active_variant {
        Some(v) => schema.fields_for_variant(v),
        None => schema.fields.iter().collect(),
    }
}

/// True when `field_name` appears in the top-level common `fields` list.
/// Used to distinguish variant-scoped requireds (V402) from common requireds.
fn is_common_field(schema: &SchemaDefinition, field_name: &str) -> bool {
    schema.fields.iter().any(|f| f.name == field_name)
}

pub fn validate_schema(resource: &RawResource, schema: &ResourceSchema) -> Result<(), ValidationError> {
    let def = &schema.resource;

    // Check if this resource is from an ID dereference
    let is_from_id = resource.data.get("_from_id").and_then(|v| v.as_bool()).unwrap_or(false);

    // Resolve the active variant (if the schema declares a discriminator).
    // When the discriminator is unset or not a string, fall back to common-only
    // validation — the discriminator field itself is validated in the normal
    // required/type loop below.
    let active_variant = active_variant_value(&def.schema, &resource.data);
    let active_fields: Vec<&FieldDefinition> = fields_for_active(&def.schema, active_variant.as_deref());

    let resource_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");

    // Required fields (skip if dereferencing existing resource by ID). Walks
    // the active variant. Missing variant-scoped requireds encode WXCTL-V402
    // in the message so log consumers can distinguish variant-specific gaps
    // from ordinary missing-field errors (V001/V003).
    if !is_from_id {
        for field in &active_fields {
            if field.required && resource.data.get(&field.name).is_none() {
                if is_common_field(&def.schema, &field.name) {
                    return Err(ValidationError::MissingField { field: field.name.clone() });
                }
                let variant = active_variant.as_deref().unwrap_or("?");
                let msg = format!("[{}] WXCTL-V402: field '{}' is required for variant '{}' but is not set", error_codes::V402, field.name, variant);
                return Err(ValidationError::InvalidFieldValue { field: field.name.clone(), message: msg });
            }
        }
    }

    // `all_fields()` walks common + every variant (deduped) — shared below by
    // the computed-field check and the unknown-field check so we only build
    // the full surface once per resource.
    let all_fields = def.schema.all_fields();

    // Computed fields cannot be set by the user — check across every known
    // field so an inactive-variant computed field still errors instead of
    // being silently accepted.
    for field in &all_fields {
        if field.location == FieldLocation::Computed && resource.data.get(&field.name).is_some() {
            return Err(ValidationError::ComputedFieldSet { field: field.name.clone() });
        }
    }

    // Type / range / pattern / allowed-values validation on the active field set.
    for field in &active_fields {
        if let Some(value) = resource.data.get(&field.name) {
            validate_field_type(value, &field.field_type, &field.name)?;

            if let Some(allowed) = &field.allowed_values {
                validate_allowed_values(value, allowed, &field.name)?;
            }

            if let Some(rules) = &field.validation {
                validate_rules(value, rules, &field.name)?;
                warn_soft_allowed_values(value, rules, &field.name, &resource.kind, resource_name);
                validate_extra_rules(value, rules, &field.name)?;
            }
        }
    }

    // Warn on inactive-variant fields whose values are set — WXCTL-V401. The
    // field would never reach the API under the active discriminator value,
    // so fail-open with a nudge rather than a hard error (keeps user configs
    // moving when they toggle `type:` and forget to strip the old auth block).
    warn_inactive_variant_fields(&def.schema, &resource.data, active_variant.as_deref(), &active_fields, &resource.kind, resource_name);

    // Cross-field: oneOf groups at resource-root scope. Evaluate against the
    // active field set so ADLS's {account_key, sas_token, service_principal}
    // group fires only when the active variant declares it.
    validate_one_of_groups_refs(&resource.data, &active_fields)?;

    // Unknown-field check: the known set is common + every variant. Fields
    // declared for other variants are still known — they get the V401 nudge
    // above, not a hard UnknownField.
    if let Some(obj) = resource.data.as_object() {
        let known: HashSet<&str> = all_fields.iter().map(|f| f.name.as_str()).chain(META_FIELDS.iter().copied()).collect();
        for key in obj.keys() {
            if !known.contains(key.as_str()) {
                return Err(ValidationError::UnknownField { field: key.clone() });
            }
        }
    }

    // `on_destroy` is a universal meta-field with a fixed enum surface. Reject
    // any other string (or non-string) value so typos (e.g. `retains`) surface
    // at validate time, not silently as the default `Delete`.
    if let Some(v) = resource.data.get("on_destroy") {
        match v.as_str() {
            Some("retain") | Some("delete") => {}
            _ => return Err(ValidationError::InvalidFieldValue { field: "on_destroy".into(), message: format!("[{}] WXCTL-V009: on_destroy must be 'retain' or 'delete', got {:?}", error_codes::V009, v) }),
        }
    }

    Ok(())
}

/// Emit `WXCTL-V401` at warn level for every field declared in an inactive
/// variant that carries a value in `data`. Fields that overlap with the
/// active variant (same name) are silent — they are semantically in-scope.
fn warn_inactive_variant_fields(schema: &SchemaDefinition, data: &Value, active_variant: Option<&str>, active_fields: &[&FieldDefinition], resource_kind: &str, resource_name: &str) {
    let Some(variants) = &schema.variants else { return };
    let Some(active) = active_variant else { return };

    let active_names: HashSet<&str> = active_fields.iter().map(|f| f.name.as_str()).collect();

    for variant in variants.values() {
        let applies = variant.applies_to.iter().any(|v| v == active);
        if applies {
            continue;
        }
        for field in &variant.fields {
            if active_names.contains(field.name.as_str()) {
                continue;
            }
            if data.get(&field.name).is_some() {
                let msg = format!("field '{}' is declared for variants {:?} and has no effect when {}='{}'", field.name, variant.applies_to, schema.discriminator.as_deref().unwrap_or("type"), active);
                tracing::warn!(target: "wxctl::warning", error_code = %error_codes::V401, resource_type = %resource_kind, resource_name = %resource_name, field_path = %field.name, value = %active, known_values = ?variant.applies_to.clone(), "{}: {}", error_codes::V401, msg);
            }
        }
    }
}

fn validate_field_type(value: &Value, field_type: &crate::schema::FieldType, _field_name: &str) -> Result<(), ValidationError> {
    use crate::schema::FieldType;

    let expected = match field_type {
        FieldType::String => {
            if value.is_string() {
                return Ok(());
            }
            "string"
        }
        FieldType::Integer => {
            if value.is_i64() || value.is_u64() {
                return Ok(());
            }
            "integer"
        }
        FieldType::Float => {
            if value.is_f64() {
                return Ok(());
            }
            "float"
        }
        FieldType::Boolean => {
            if value.is_boolean() {
                return Ok(());
            }
            "boolean"
        }
        FieldType::Object => {
            if value.is_object() {
                return Ok(());
            }
            "object"
        }
        FieldType::Array => {
            if value.is_array() {
                return Ok(());
            }
            "array"
        }
        FieldType::Timestamp => {
            if value.is_string()
                && let Some(s) = value.as_str()
                && chrono::DateTime::parse_from_rfc3339(s).is_ok()
            {
                return Ok(());
            }
            "timestamp (ISO 8601)"
        }
    };

    Err(ValidationError::TypeMismatch { expected, got: type_name(value) })
}

fn type_name(value: &Value) -> String {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "float",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
    .to_string()
}

fn validate_allowed_values(value: &Value, allowed: &[String], field_name: &str) -> Result<(), ValidationError> {
    if let Some(s) = value.as_str()
        && !allowed.iter().any(|a| a == s)
    {
        return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Must be one of: {}", allowed.join(", ")) });
    }
    Ok(())
}

fn validate_rules(value: &Value, rules: &crate::schema::ValidationRules, field_name: &str) -> Result<(), ValidationError> {
    if let Some(s) = value.as_str() {
        if let Some(min) = rules.min_length
            && s.chars().count() < min
        {
            return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Minimum length is {}", min) });
        }

        if let Some(max) = rules.max_length
            && s.chars().count() > max
        {
            return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Maximum length is {}", max) });
        }

        if let Some(max_bytes) = rules.max_length_bytes
            && s.len() > max_bytes
        {
            return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Maximum UTF-8 byte length is {}", max_bytes) });
        }

        if let Some(pattern) = &rules.pattern {
            let re = {
                let mut cache = REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(existing) = cache.get(pattern) {
                    existing.clone()
                } else {
                    let compiled = regex::Regex::new(pattern).map_err(|e| ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Invalid validation pattern '{}': {}", pattern, e) })?;
                    cache.entry(pattern.clone()).or_insert(compiled).clone()
                }
            };

            if !re.is_match(s) {
                return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Must match pattern: {}", pattern) });
            }
        }
    }

    if let Some(n) = value.as_i64() {
        if let Some(min) = rules.min_value
            && n < min
        {
            return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Minimum value is {}", min) });
        }

        if let Some(max) = rules.max_value
            && n > max
        {
            return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Maximum value is {}", max) });
        }
    }

    if let Some(arr) = value.as_array()
        && let Some(max) = rules.max_items
        && arr.len() > max
    {
        return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Maximum number of items is {}", max) });
    }

    Ok(())
}

/// Apply named `extra_rules` declared on a field. Each rule is a small,
/// self-contained predicate; surface a concrete error message when violated
/// so users know exactly which rule rejected the value. Unknown rule names
/// are ignored (forward-compatibility — older engine reading newer schema).
fn validate_extra_rules(value: &Value, rules: &crate::schema::ValidationRules, field_name: &str) -> Result<(), ValidationError> {
    let Some(extras) = &rules.extra_rules else { return Ok(()) };
    let Some(s) = value.as_str() else { return Ok(()) };

    for rule in extras {
        match rule.as_str() {
            "no_consecutive_dots" => {
                if s.contains("..") {
                    return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: "must not contain consecutive dots ('..')".to_string() });
                }
            }
            "not_ip_address" => {
                // Reject dotted-quad that looks like an IPv4 literal.
                let octets: Vec<&str> = s.split('.').collect();
                if octets.len() == 4 && octets.iter().all(|o| !o.is_empty() && o.chars().all(|c| c.is_ascii_digit()) && o.parse::<u8>().is_ok()) {
                    return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: "must not be formatted as an IPv4 address".to_string() });
                }
            }
            "no_reserved_prefix" => {
                if s.starts_with("xn--") {
                    return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: "must not start with the reserved prefix 'xn--'".to_string() });
                }
            }
            "no_reserved_suffix" if s.ends_with("-s3alias") || s.ends_with("--ol-s3") => {
                return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: "must not end with the reserved suffix '-s3alias' or '--ol-s3'".to_string() });
            }
            "no_reserved_suffix" => {}
            _ => {
                // Unknown rule name — forward-compat silence.
            }
        }
    }
    Ok(())
}

/// Walk every `one_of:` group declared on any field's validation rules and
/// ensure exactly one of the named siblings is set in `data`. Emits
/// `WXCTL-V501` on violation. The groups themselves live on the field that
/// *depends* on the exclusivity (cos_object.path/content), not on the
/// sibling fields — this keeps the declaration next to the semantics.
fn validate_one_of_groups_refs(data: &Value, fields: &[&FieldDefinition]) -> Result<(), ValidationError> {
    let Some(obj) = data.as_object() else { return Ok(()) };

    for field in fields {
        let Some(rules) = &field.validation else { continue };
        let Some(groups) = &rules.one_of else { continue };
        for group in groups {
            let set: Vec<&str> = group.iter().filter(|name| obj.get(name.as_str()).is_some_and(|v| !v.is_null())).map(|s| s.as_str()).collect();
            if set.len() != 1 {
                let list = group.join(", ");
                let msg = if set.is_empty() { format!("[{}] WXCTL-V501: exactly one of ({list}) must be set; none provided", error_codes::V501) } else { format!("[{}] WXCTL-V501: exactly one of ({list}) must be set; got {} ({})", error_codes::V501, set.len(), set.join(", ")) };
                return Err(ValidationError::InvalidFieldValue { field: field.name.clone(), message: msg });
            }
        }
    }
    Ok(())
}

/// Emit `WXCTL-V401` at warn level when a string field's value falls outside
/// `soft_allowed_values`. Does not fail validation — unlike `allowed_values`,
/// the soft variant trusts the API as the authority and only nudges the user.
fn warn_soft_allowed_values(value: &Value, rules: &crate::schema::ValidationRules, field_name: &str, resource_kind: &str, resource_name: &str) {
    let Some(soft) = &rules.soft_allowed_values else { return };
    let Some(s) = value.as_str() else { return };
    if soft.iter().any(|a| a == s) {
        return;
    }
    tracing::warn!(target: "wxctl::warning", error_code = %error_codes::V401, resource_type = %resource_kind, resource_name = %resource_name, field_path = %field_name, value = %s, known_values = ?soft, "{}: field '{field_name}' value '{s}' is outside the known list; plan continues but the API may reject it", error_codes::V401);
}

/// Global load-time guard: a schema that PATCHes with JSON-Patch must declare
/// `reconciliation.json_patch_path_prefix`. The misplacement of this key under
/// `api:` (where serde silently drops it) is a generic foot-gun. The
/// engine's `update.rs` errors `json_patch_path_prefix required` at apply time
/// when it is `None`; this turns that runtime failure into a load-time schema
/// error. `""` (RFC-6902 entity-relative) is a valid prefix and passes.
pub fn validate_reconciliation_patch_prefix(reconciliation: &ReconciliationDefinition, kind: &str) -> Result<(), ValidationError> {
    if matches!(reconciliation.update_strategy, UpdateStrategy::Patch) && reconciliation.use_json_patch && reconciliation.json_patch_path_prefix.is_none() {
        return Err(ValidationError::InvalidFieldValue {
            field: "reconciliation.json_patch_path_prefix".into(),
            message: format!("schema '{kind}' uses update_strategy: patch with use_json_patch: true but has no reconciliation.json_patch_path_prefix — set `reconciliation.json_patch_path_prefix` (use \"\" for RFC-6902 entity-relative paths)"),
        });
    }
    Ok(())
}

pub fn check_duplicate_names(resources: &[RawResource]) -> Result<(), ValidationError> {
    use wxctl_graph::ResourceKey;
    let mut seen = HashMap::new();

    for resource in resources {
        let ref_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");

        let key = ResourceKey::new(&resource.kind, ref_name);

        if seen.insert(key.clone(), ()).is_some() {
            return Err(ValidationError::DuplicateName { kind: key.kind.to_string(), name: key.name.to_string() });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldType, HookDefinition, HttpMethod, ReconciliationDefinition, ResourceDefinition, SchemaDefinition, UpdateStrategy, ValidationRules};
    use serde_json::json;

    fn make_schema(fields: Vec<FieldDefinition>) -> ResourceSchema {
        ResourceSchema {
            resource: ResourceDefinition {
                name: "test".into(),
                service: "test".into(),
                kind: "test".into(),
                version: "v1".into(),
                api: ApiDefinition {
                    base_path: "/api/test".into(),
                    id_field: "id".into(),
                    list_endpoint: None,
                    get_endpoint: "/api/test/{id}".into(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: None,
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                },
                schema: SchemaDefinition { fields, ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::GetById, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, id_source: "id".into() },
                    state_fields: Some(vec![]),
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec![],
                    reject_on_immutable_drift: false,
                    use_json_patch: false,
                    json_patch_path_prefix: None,
                },
                hooks: HookDefinition::default(),
                deployments: None,
                unsupported_on: vec![],
                description: None,
                prompt: None,
            },
        }
    }

    fn make_field(name: &str, field_type: FieldType, required: bool) -> FieldDefinition {
        FieldDefinition {
            name: name.into(),
            field_type,
            required,
            immutable: false,
            location: FieldLocation::default(),
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
            properties: None,
        }
    }

    /// Build a one-field schema whose single field carries `validation`.
    fn schema_with_rules(name: &str, ft: FieldType, rules: ValidationRules) -> ResourceSchema {
        let mut field = make_field(name, ft, false);
        field.validation = Some(rules);
        make_schema(vec![field])
    }

    /// `ValidationRules` with every slot None — set the one(s) under test on the result.
    fn empty_rules() -> ValidationRules {
        ValidationRules { min_length: None, max_length: None, pattern: None, min_value: None, max_value: None, max_length_bytes: None, max_items: None, soft_allowed_values: None, one_of: None, extra_rules: None }
    }

    // ── apply_defaults ──

    #[test]
    fn apply_defaults_inserts_and_preserves() {
        let mut field = make_field("color", FieldType::String, false);
        field.default = Some(json!("blue"));
        let schema = make_schema(vec![field]);

        // Missing field → default inserted.
        let mut absent = RawResource { kind: "test".into(), data: json!({}) };
        apply_defaults(&mut absent, &schema);
        assert_eq!(absent.data.get("color"), Some(&json!("blue")));

        // Present field → existing value preserved (default must not overwrite).
        let mut present = RawResource { kind: "test".into(), data: json!({"color": "red"}) };
        apply_defaults(&mut present, &schema);
        assert_eq!(present.data.get("color"), Some(&json!("red")));
    }

    // ── validate_schema: required / from_id / computed ──

    #[test]
    fn validate_schema_required_field_paths() {
        let schema = make_schema(vec![make_field("name", FieldType::String, true)]);

        // Present → ok.
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"name": "hello"}) }, &schema).is_ok());

        // Missing common required → MissingField (logged V001/V003).
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({}) }, &schema).unwrap_err();
        assert!(matches!(err, ValidationError::MissingField { field } if field == "name"));

        // `_from_id` dereference skips the required check entirely.
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"_from_id": true}) }, &schema).is_ok());
    }

    #[test]
    fn validate_schema_computed_field_set() {
        let mut field = make_field("hash", FieldType::String, false);
        field.location = FieldLocation::Computed;
        let schema = make_schema(vec![field]);

        let resource = RawResource { kind: "test".into(), data: json!({"hash": "abc"}) };
        let err = validate_schema(&resource, &schema).unwrap_err();
        assert!(matches!(err, ValidationError::ComputedFieldSet { field } if field == "hash"));
    }

    // ── validate_schema: type / range / pattern / allowed-values rejects ──
    //
    // One table over every value-validation reject path: each row drives the
    // same `validate_schema` call to the same `Err`. Grouped by the error
    // variant they must surface so each distinct branch survives folding.

    #[test]
    fn validate_schema_type_mismatch_rejects() {
        // Each row: (field_type, value, expected `TypeMismatch.expected`).
        let cases: &[(FieldType, Value, &str)] = &[
            (FieldType::Integer, json!("not_a_number"), "integer"),              // string where integer wanted
            (FieldType::Timestamp, json!("not-a-date"), "timestamp (ISO 8601)"), // non-RFC3339 string
        ];
        for (ft, value, expected) in cases {
            let schema = make_schema(vec![make_field("f", ft.clone(), false)]);
            let resource = RawResource { kind: "test".into(), data: json!({ "f": value }) };
            let err = validate_schema(&resource, &schema).unwrap_err();
            match err {
                ValidationError::TypeMismatch { expected: got, .. } => assert_eq!(got, *expected, "value {value:?}"),
                other => panic!("expected TypeMismatch({expected}) for {value:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn validate_schema_valid_value_types_pass() {
        // Accept cases: well-formed values for type/timestamp must validate clean.
        let valid_ts = make_schema(vec![make_field("created_at", FieldType::Timestamp, false)]);
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"created_at": "2024-01-15T10:30:00Z"}) }, &valid_ts).is_ok());
    }

    #[test]
    fn validate_schema_invalid_field_value_rejects() {
        // Each row: (field, schema-with-rule, value, why). All must surface
        // `InvalidFieldValue` naming the field — one row per distinct rule branch.
        let mut allowed_field = make_field("status", FieldType::String, false);
        allowed_field.allowed_values = Some(vec!["active".into(), "inactive".into()]);
        let allowed_schema = make_schema(vec![allowed_field]);

        let cases: Vec<(&str, ResourceSchema, Value)> = vec![
            // allowed_values reject (hard list)
            ("status", allowed_schema, json!("deleted")),
            // min_length
            ("name", schema_with_rules("name", FieldType::String, ValidationRules { min_length: Some(3), ..empty_rules() }), json!("ab")),
            // max_length
            ("name", schema_with_rules("name", FieldType::String, ValidationRules { max_length: Some(5), ..empty_rules() }), json!("toolong")),
            // pattern mismatch
            ("code", schema_with_rules("code", FieldType::String, ValidationRules { pattern: Some("^[A-Z]+$".into()), ..empty_rules() }), json!("abc")),
            // min_value
            ("age", schema_with_rules("age", FieldType::Integer, ValidationRules { min_value: Some(0), max_value: Some(150), ..empty_rules() }), json!(-1)),
            // max_value
            ("age", schema_with_rules("age", FieldType::Integer, ValidationRules { min_value: Some(0), max_value: Some(150), ..empty_rules() }), json!(200)),
        ];
        for (field_name, schema, value) in cases {
            let resource = RawResource { kind: "test".into(), data: json!({ field_name: value }) };
            let err = validate_schema(&resource, &schema).unwrap_err();
            assert!(matches!(err, ValidationError::InvalidFieldValue { ref field, .. } if field == field_name), "field {field_name} value {value:?} → {err:?}");
        }
    }

    #[test]
    fn soft_allowed_values_never_fails_validation() {
        // soft_allowed_values only nudges via tracing; in-list and out-of-list
        // both validate OK (unlike the hard `allowed_values` reject above).
        let schema = schema_with_rules("type", FieldType::String, ValidationRules { soft_allowed_values: Some(vec!["db2".into(), "mysql".into()]), ..empty_rules() });
        for v in ["db2", "not_a_real_connector"] {
            assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({ "type": v }) }, &schema).is_ok(), "soft value {v} must not fail");
        }
    }

    // ── validate_schema: unknown / meta fields ──

    #[test]
    fn validate_schema_unknown_field() {
        let schema = make_schema(vec![make_field("name", FieldType::String, false)]);
        let resource = RawResource { kind: "test".into(), data: json!({"name": "ok", "bogus": 42}) };
        let err = validate_schema(&resource, &schema).unwrap_err();
        assert!(matches!(err, ValidationError::UnknownField { field } if field == "bogus"));
    }

    #[test]
    fn validate_schema_meta_fields_pass_through() {
        let schema = make_schema(vec![]);
        // Core meta-fields plus `depends_on` must all pass the unknown-field check.
        let resource = RawResource { kind: "test".into(), data: json!({"kind": "test", "ref_name": "foo", "_from_id": true, "id": "abc"}) };
        assert!(validate_schema(&resource, &schema).is_ok());
        let with_depends = RawResource { kind: "test".into(), data: json!({"ref_name": "b", "depends_on": ["catalog.a"]}) };
        assert!(validate_schema(&with_depends, &schema).is_ok(), "depends_on must pass the unknown-field check as a meta-field");
    }

    // ── on_destroy meta-field ──

    #[test]
    fn validate_schema_on_destroy_enum() {
        let schema = make_schema(vec![]);
        // Accept: both valid enum values.
        for v in ["retain", "delete"] {
            assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({ "on_destroy": v }) }, &schema).is_ok(), "on_destroy={v}");
        }
        // Reject: a typo'd string must surface V009 (not silently default to Delete).
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"on_destroy": "retains"}) }, &schema).unwrap_err();
        match err {
            ValidationError::InvalidFieldValue { field, message } => {
                assert_eq!(field, "on_destroy");
                assert!(message.contains("WXCTL-V009"));
            }
            other => panic!("expected V009 InvalidFieldValue, got {:?}", other),
        }
        // Reject: a non-string value too.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"on_destroy": true}) }, &schema).unwrap_err();
        assert!(matches!(err, ValidationError::InvalidFieldValue { ref field, .. } if field == "on_destroy"));
    }

    // ── check_duplicate_names ──

    #[test]
    fn check_duplicate_names_keyed_by_kind_and_name() {
        // Unique (kind,name) pairs → ok.
        let unique = vec![RawResource { kind: "catalog".into(), data: json!({"ref_name": "a"}) }, RawResource { kind: "catalog".into(), data: json!({"ref_name": "b"}) }];
        assert!(check_duplicate_names(&unique).is_ok());

        // Same name under a different kind → still ok (key includes kind).
        let cross_kind = vec![RawResource { kind: "catalog".into(), data: json!({"ref_name": "a"}) }, RawResource { kind: "connection".into(), data: json!({"ref_name": "a"}) }];
        assert!(check_duplicate_names(&cross_kind).is_ok());

        // Same kind + same name → DuplicateName.
        let dup = vec![RawResource { kind: "catalog".into(), data: json!({"ref_name": "a"}) }, RawResource { kind: "catalog".into(), data: json!({"ref_name": "a"}) }];
        let err = check_duplicate_names(&dup).unwrap_err();
        assert!(matches!(err, ValidationError::DuplicateName { kind, name } if kind == "catalog" && name == "a"));
    }

    // ── patch-prefix guard (global) ──

    fn recon(update_strategy: UpdateStrategy, use_json_patch: bool, prefix: Option<&str>) -> ReconciliationDefinition {
        ReconciliationDefinition {
            discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, id_source: "id".into(), name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None },
            state_fields: None,
            update_strategy,
            immutable_fields: vec![],
            reject_on_immutable_drift: false,
            use_json_patch,
            json_patch_path_prefix: prefix.map(str::to_string),
        }
    }

    #[test]
    fn patch_prefix_guard_branches() {
        // Reject: patch + use_json_patch + no prefix → error naming the kind.
        let r = recon(UpdateStrategy::Patch, true, None);
        let err = validate_reconciliation_patch_prefix(&r, "monitor_instance").unwrap_err();
        match err {
            ValidationError::InvalidFieldValue { field, message } => {
                assert_eq!(field, "reconciliation.json_patch_path_prefix");
                assert!(message.contains("monitor_instance"), "message must name the kind: {message}");
            }
            other => panic!("expected InvalidFieldValue, got {other:?}"),
        }

        // Accept: empty prefix ("" = RFC-6902 entity-relative) is a valid prefix.
        assert!(validate_reconciliation_patch_prefix(&recon(UpdateStrategy::Patch, true, Some("")), "monitor_instance").is_ok());

        // Ignore: guardrails_policy uses update_strategy: replace — guard must not fire even with no prefix.
        assert!(validate_reconciliation_patch_prefix(&recon(UpdateStrategy::Replace, true, None), "guardrails_policy").is_ok());

        // Ignore: use_json_patch: false → prefix irrelevant, guard must not fire.
        assert!(validate_reconciliation_patch_prefix(&recon(UpdateStrategy::Patch, false, None), "x").is_ok());
    }

    // ── variant-aware validation (V401 / V402) ──

    fn make_variant_schema() -> ResourceSchema {
        use crate::schema::definition::VariantDefinition;

        let type_field = make_field("type", FieldType::String, true);
        let name_field = make_field("name", FieldType::String, true);

        let mut variants = std::collections::HashMap::new();
        variants.insert("s3_family".to_string(), VariantDefinition { applies_to: vec!["ibm_cos".into(), "aws_s3".into()], fields: vec![make_field("access_key", FieldType::String, true), make_field("secret_key", FieldType::String, true)] });
        variants.insert("adls_family".to_string(), VariantDefinition { applies_to: vec!["adls_gen2".into()], fields: vec![make_field("account_name", FieldType::String, true), make_field("sas_token", FieldType::String, false)] });

        let schema_def = SchemaDefinition { fields: vec![type_field, name_field], discriminator: Some("type".into()), variants: Some(variants) };
        let mut rs = make_schema(vec![]);
        rs.resource.schema = schema_def;
        rs
    }

    #[test]
    fn variant_aware_accept_paths() {
        let schema = make_variant_schema();

        // Required variant field present → ok. (Also covers `variant_fields_are_known_fields`:
        // s3_family's access_key lives in a variant, not common fields, yet must pass
        // the UnknownField check when set under type=ibm_cos.)
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak", "secret_key": "sk"}) }, &schema).is_ok());

        // An ADLS-family field set while type=ibm_cos emits V401 warn but validation still succeeds.
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak", "secret_key": "sk", "account_name": "leftover"}) }, &schema).is_ok());
    }

    #[test]
    fn variant_aware_reject_paths() {
        let schema = make_variant_schema();

        // Missing variant-scoped required → V402 naming the field and variant.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak"}) }, &schema).unwrap_err();
        match err {
            ValidationError::InvalidFieldValue { field, message } => {
                assert_eq!(field, "secret_key");
                assert!(message.contains("WXCTL-V402"));
                assert!(message.contains("variant 'ibm_cos'"));
            }
            other => panic!("expected V402 InvalidFieldValue, got {:?}", other),
        }

        // A missing top-level required field should still surface as MissingField
        // (pipeline logs under V001/V003), not V402.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "access_key": "ak", "secret_key": "sk"}) }, &schema).unwrap_err();
        assert!(matches!(err, ValidationError::MissingField { field } if field == "name"));

        // A field declared by no variant is still a hard UnknownField.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak", "secret_key": "sk", "completely_bogus": 1}) }, &schema).unwrap_err();
        assert!(matches!(err, ValidationError::UnknownField { field } if field == "completely_bogus"));
    }

    #[test]
    fn apply_defaults_only_active_variant() {
        use crate::schema::definition::VariantDefinition;

        // Two variants, each with a field defaulted to a distinct value.
        let mut s3_key = make_field("access_key", FieldType::String, false);
        s3_key.default = Some(json!("s3-default"));
        let mut adls_key = make_field("sas_token", FieldType::String, false);
        adls_key.default = Some(json!("adls-default"));

        let mut variants = std::collections::HashMap::new();
        variants.insert("s3_family".into(), VariantDefinition { applies_to: vec!["ibm_cos".into()], fields: vec![s3_key] });
        variants.insert("adls_family".into(), VariantDefinition { applies_to: vec!["adls_gen2".into()], fields: vec![adls_key] });

        let mut rs = make_schema(vec![]);
        rs.resource.schema = SchemaDefinition { fields: vec![make_field("type", FieldType::String, true)], discriminator: Some("type".into()), variants: Some(variants) };

        let mut resource = RawResource { kind: "test".into(), data: json!({"type": "ibm_cos"}) };
        apply_defaults(&mut resource, &rs);

        assert_eq!(resource.data.get("access_key"), Some(&json!("s3-default")));
        assert!(resource.data.get("sas_token").is_none(), "inactive variant default must not leak");
    }
}
