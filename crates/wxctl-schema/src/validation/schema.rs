use super::error_codes;
use super::types::ValidationError;
use crate::ir::{FieldIr, FieldLocationIr, FieldTypeIr, SchemaBodyIr, SchemaIr, ValidationIr};
use crate::resource::RawResource;
use anyhow::Result;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{LazyLock, Mutex};

/// Cache for compiled regex patterns used in field validation.
static REGEX_CACHE: LazyLock<Mutex<HashMap<String, regex::Regex>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Fields that are part of the config system but not defined in resource schemas.
const META_FIELDS: &[&str] = &["kind", "ref_name", "_from_id", "id", "on_destroy", "metadata", "depends_on"];

pub fn apply_defaults(resource: &mut RawResource, schema: &SchemaIr) {
    let def = &schema.resource;
    let active_variant = active_variant_value(&def.schema, &resource.data);

    // Apply defaults for common fields + fields of the active variant (if any).
    // Inactive-variant defaults are not applied — they have no semantic meaning
    // when the discriminator selects a different variant.
    let fields = fields_for_active(&def.schema, active_variant.as_deref());

    for field in fields {
        if let Some(default_str) = field.default
            && resource.data.get(field.name).is_none()
            && let Some(obj) = resource.data.as_object_mut()
        {
            let default_value: Value = serde_json::from_str(default_str).expect("canonical json default");
            obj.insert(field.name.to_string(), default_value);
        }
        // Recurse into a present value's nested schema. Absent parents are left
        // absent — defaults never synthesize whole objects, only fill gaps in
        // objects the user chose to set.
        if let Some(inner) = field.schema
            && let Some(value) = resource.data.get_mut(field.name)
        {
            apply_defaults_nested(value, inner);
        }
    }
}

/// Fill sub-field defaults inside an existing object value (or each object
/// element of an array value), recursing through deeper nested schemas.
fn apply_defaults_nested(value: &mut Value, schema: &SchemaBodyIr) {
    match value {
        Value::Object(obj) => {
            for field in schema.fields {
                if let Some(default_str) = field.default
                    && !obj.contains_key(field.name)
                {
                    let default_value: Value = serde_json::from_str(default_str).expect("canonical json default");
                    obj.insert(field.name.to_string(), default_value);
                }
                if let Some(inner) = field.schema
                    && let Some(v) = obj.get_mut(field.name)
                {
                    apply_defaults_nested(v, inner);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                apply_defaults_nested(item, schema);
            }
        }
        _ => {}
    }
}

/// Read the discriminator value from the raw resource data, if the schema
/// declares a discriminator and the value is a string. Returns None when the
/// schema has no variants, the discriminator field is missing, or the value
/// is not a string.
fn active_variant_value(schema: &SchemaBodyIr, data: &Value) -> Option<String> {
    let disc = schema.discriminator?;
    let value = data.get(disc)?;
    value.as_str().map(str::to_string)
}

/// Fields in effect for the resolved active variant: the variant's fields when the
/// discriminator selects one, else the common top-level fields.
fn fields_for_active<'a>(schema: &'a SchemaBodyIr, active_variant: Option<&str>) -> Vec<&'a FieldIr> {
    match active_variant {
        Some(v) => schema.fields_for_variant(v),
        None => schema.fields.iter().collect(),
    }
}

/// True when `field_name` appears in the top-level common `fields` list.
/// Used to distinguish variant-scoped requireds (V402) from common requireds.
fn is_common_field(schema: &SchemaBodyIr, field_name: &str) -> bool {
    schema.fields.iter().any(|f| f.name == field_name)
}

pub fn validate_schema(resource: &RawResource, schema: &SchemaIr) -> Result<(), ValidationError> {
    let def = &schema.resource;

    // Check if this resource is from an ID dereference
    let is_from_id = resource.data.get("_from_id").and_then(|v| v.as_bool()).unwrap_or(false);

    // Resolve the active variant (if the schema declares a discriminator).
    // When the discriminator is unset or not a string, fall back to common-only
    // validation — the discriminator field itself is validated in the normal
    // required/type loop below.
    let active_variant = active_variant_value(&def.schema, &resource.data);
    let active_fields: Vec<&FieldIr> = fields_for_active(&def.schema, active_variant.as_deref());

    let resource_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");

    // Required fields (skip if dereferencing existing resource by ID). Walks
    // the active variant. Missing variant-scoped requireds encode WXCTL-V402
    // in the message so log consumers can distinguish variant-specific gaps
    // from ordinary missing-field errors (V001/V003).
    if !is_from_id {
        for field in &active_fields {
            if field.required && resource.data.get(field.name).is_none() {
                if is_common_field(&def.schema, field.name) {
                    return Err(ValidationError::MissingField { field: field.name.to_string(), reference_kind: field.references.as_ref().map(|r| r.resource.to_string()) });
                }
                let variant = active_variant.as_deref().unwrap_or("?");
                let msg = format!("[{}] WXCTL-V402: field '{}' is required for variant '{}' but is not set", error_codes::V402, field.name, variant);
                return Err(ValidationError::InvalidFieldValue { field: field.name.to_string(), message: msg });
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
        if matches!(field.location, FieldLocationIr::Computed) && resource.data.get(field.name).is_some() {
            return Err(ValidationError::ComputedFieldSet { field: field.name.to_string() });
        }
    }

    // Type / range / pattern / allowed-values validation on the active field set,
    // then recursion into nested `schema:` sub-fields and array elements.
    for field in &active_fields {
        if let Some(value) = resource.data.get(field.name) {
            // `TypeMismatch` carries no field path — rewrap so the error names the
            // offending field (same idiom as the nested-object and array-element
            // branches in `validate_nested_value`).
            if let Err(e) = validate_field_type(value, &field.field_type, field.name) {
                let message = match e {
                    ValidationError::TypeMismatch { expected, got } => format!("must be {expected}, got {got}"),
                    other => other.to_string(),
                };
                return Err(ValidationError::InvalidFieldValue { field: field.name.to_string(), message });
            }

            if let Some(allowed) = field.allowed_values {
                validate_allowed_values(value, allowed, field.name)?;
            }

            if let Some(rules) = field.validation.as_ref() {
                validate_rules(value, rules, field.name)?;
                warn_soft_allowed_values(value, rules, field.name, &resource.kind, resource_name);
                validate_extra_rules(value, rules, field.name)?;
            }

            validate_nested_value(value, field, field.name, &resource.kind, resource_name)?;
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
        let known: HashSet<&str> = all_fields.iter().map(|f| f.name).chain(META_FIELDS.iter().copied()).collect();
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

    // WXCTL-V403 (warn, non-fatal): a Python tool that also declares an inline
    // input_schema/output_schema. Runs on the raw authored config, before
    // post_validate injects the sidecar schema.yaml, so it fires only on
    // user-authored inline blocks. Never turns validation into an error.
    warn_redundant_python_tool_schema(resource);

    Ok(())
}

/// Emit `WXCTL-V401` at warn level for every field declared in an inactive
/// variant that carries a value in `data`. Fields that overlap with the
/// active variant (same name) are silent — they are semantically in-scope.
fn warn_inactive_variant_fields(schema: &SchemaBodyIr, data: &Value, active_variant: Option<&str>, active_fields: &[&FieldIr], resource_kind: &str, resource_name: &str) {
    let Some(variants) = schema.variants else { return };
    let Some(active) = active_variant else { return };

    let active_names: HashSet<&str> = active_fields.iter().map(|f| f.name).collect();

    for (_, variant) in variants {
        let applies = variant.applies_to.contains(&active);
        if applies {
            continue;
        }
        for field in variant.fields {
            if active_names.contains(field.name) {
                continue;
            }
            if data.get(field.name).is_some() {
                let msg = format!("field '{}' is declared for variants {:?} and has no effect when {}='{}'", field.name, variant.applies_to, schema.discriminator.unwrap_or("type"), active);
                tracing::warn!(target: "wxctl::warning", error_code = %error_codes::V401, resource_type = %resource_kind, resource_name = %resource_name, field_path = %field.name, value = %active, known_values = ?variant.applies_to, "{}: {}", error_codes::V401, msg);
            }
        }
    }
}

/// The inline schema field names that make a Python tool's config block redundant
/// (`schema.yaml` in `source_path` is authoritative). Empty for a non-tool, a non-Python
/// tool, or a Python tool with no inline `input_schema`/`output_schema`. Pure data (no FS),
/// so the offline `validate_config` path and unit tests assert on it directly.
pub(crate) fn redundant_python_tool_schema_fields(resource: &RawResource) -> Vec<&'static str> {
    if resource.kind != "tool" || resource.data.pointer("/binding/python").is_none() {
        return Vec::new();
    }
    ["input_schema", "output_schema"].into_iter().filter(|f| resource.data.get(*f).is_some()).collect()
}

/// Emit `WXCTL-V403` at warn level when a Python-binding `tool` carries an inline
/// `input_schema`/`output_schema` in the config. For a Python tool, `schema.yaml`
/// in `source_path` is authoritative — `ToolHandler` loads it and overwrites any
/// inline block — so an inline schema here is dead weight that silently drifts.
/// Never fails validation; the field is still accepted and passed through. Delegates
/// detection to `redundant_python_tool_schema_fields` (pure data check, no FS), so the
/// offline `validate_config` path warns identically.
fn warn_redundant_python_tool_schema(resource: &RawResource) {
    let fields = redundant_python_tool_schema_fields(resource);
    if fields.is_empty() {
        return;
    }
    let resource_name = resource.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed");
    for field in fields {
        tracing::warn!(target: "wxctl::warning", error_code = %error_codes::V403, resource_type = %resource.kind, resource_name = %resource_name, field_path = %field, "{}: inline '{field}' on Python tool '{resource_name}' is redundant; schema.yaml in source_path is authoritative and overwrites it", error_codes::V403);
    }
}

/// True for a string still carrying `${...}` template syntax (resource refs /
/// `${env:VAR}`). Templates resolve after validation, so nested shape checks
/// must not fail on the placeholder string.
fn is_template(value: &Value) -> bool {
    value.as_str().is_some_and(|s| s.contains("${"))
}

/// Recurse into a field's value: array element `item_type` checks, and full
/// sub-field validation against a nested `schema:` block (for `object` fields
/// and for object elements of `array` fields).
fn validate_nested_value(value: &Value, field: &FieldIr, path: &str, resource_kind: &str, resource_name: &str) -> Result<(), ValidationError> {
    match field.field_type {
        FieldTypeIr::Array => {
            let Some(items) = value.as_array() else { return Ok(()) };
            for (i, item) in items.iter().enumerate() {
                let item_path = format!("{path}[{i}]");
                if let Some(item_type) = field.item_type
                    && !is_template(item)
                    && let Err(e) = validate_field_type(item, &item_type, &item_path)
                {
                    let message = match e {
                        ValidationError::TypeMismatch { expected, got } => format!("array element must be {expected}, got {got}"),
                        other => other.to_string(),
                    };
                    return Err(ValidationError::InvalidFieldValue { field: item_path, message });
                }
                // Element-level allowed_values: when an array field declares `allowed_values`,
                // enforce membership on each string element (e.g. environment: [draft, live]).
                // Templates (`${...}`) resolve after validation and are skipped. Scalar
                // `allowed_values` (line ~147) never fires for arrays because `validate_allowed_values`
                // only compares `value.as_str()`, so element enforcement lives here.
                if let Some(allowed) = field.allowed_values
                    && !is_template(item)
                {
                    validate_allowed_values(item, allowed, &item_path)?;
                }
                if let Some(inner) = field.schema
                    && item.is_object()
                {
                    validate_nested_object(item, inner, &item_path, resource_kind, resource_name)?;
                }
            }
        }
        FieldTypeIr::Object => {
            if let Some(inner) = field.schema
                && value.is_object()
            {
                validate_nested_object(value, inner, path, resource_kind, resource_name)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Validate an object value against a nested schema's field list, mirroring the
/// top-level checks: required, type, allowed_values, validation rules, and
/// recursion into deeper nesting. Unknown nested keys warn (`WXCTL-V401`)
/// instead of erroring — nested schemas are frequently partial (declared to
/// carry `references:`/defaults for a few known sub-fields of an otherwise
/// open object, e.g. a JSON-Schema `input_schema` block), so a hard
/// UnknownField here would reject valid configs.
fn validate_nested_object(value: &Value, schema: &SchemaBodyIr, path: &str, resource_kind: &str, resource_name: &str) -> Result<(), ValidationError> {
    let Some(obj) = value.as_object() else { return Ok(()) };

    let known: HashSet<&str> = schema.fields.iter().map(|f| f.name).collect();
    for key in obj.keys() {
        if !known.contains(key.as_str()) {
            tracing::warn!(target: "wxctl::warning", error_code = %error_codes::V401, resource_type = %resource_kind, resource_name = %resource_name, field_path = %format!("{path}.{key}"), "{}: field '{path}.{key}' is not declared in the schema for '{path}'; it is passed through to the API as-is", error_codes::V401);
        }
    }

    for field in schema.fields {
        let field_path = format!("{path}.{}", field.name);
        match obj.get(field.name) {
            None => {
                if field.required {
                    return Err(ValidationError::MissingField { field: field_path, reference_kind: field.references.as_ref().map(|r| r.resource.to_string()) });
                }
            }
            Some(v) => {
                // Skip shape checks for unresolved `${...}` templates (they resolve
                // after validation); recursion below is a no-op for strings anyway.
                if !is_template(v) {
                    // `TypeMismatch` carries no field path — rewrap so the error
                    // names the nested dotted path.
                    if let Err(e) = validate_field_type(v, &field.field_type, &field_path) {
                        let message = match e {
                            ValidationError::TypeMismatch { expected, got } => format!("must be {expected}, got {got}"),
                            other => other.to_string(),
                        };
                        return Err(ValidationError::InvalidFieldValue { field: field_path, message });
                    }
                    if let Some(allowed) = field.allowed_values {
                        validate_allowed_values(v, allowed, &field_path)?;
                    }
                    if let Some(rules) = field.validation.as_ref() {
                        validate_rules(v, rules, &field_path)?;
                        warn_soft_allowed_values(v, rules, &field_path, resource_kind, resource_name);
                        validate_extra_rules(v, rules, &field_path)?;
                    }
                }
                validate_nested_value(v, field, &field_path, resource_kind, resource_name)?;
            }
        }
    }
    Ok(())
}

fn validate_field_type(value: &Value, field_type: &FieldTypeIr, _field_name: &str) -> Result<(), ValidationError> {
    let expected = match field_type {
        FieldTypeIr::String => {
            if value.is_string() {
                return Ok(());
            }
            "string"
        }
        FieldTypeIr::Integer => {
            if value.is_i64() || value.is_u64() {
                return Ok(());
            }
            "integer"
        }
        FieldTypeIr::Float => {
            // Any JSON number is a valid float — integer literals included
            // (JSON-Schema `number` semantics; `weight: 1` is live-proven on
            // the wire). serde_json parses `1` as i64/u64, so an is_f64()-only
            // check rejects it.
            if value.is_number() {
                return Ok(());
            }
            "float"
        }
        FieldTypeIr::Boolean => {
            if value.is_boolean() {
                return Ok(());
            }
            "boolean"
        }
        FieldTypeIr::Object => {
            if value.is_object() {
                return Ok(());
            }
            "object"
        }
        FieldTypeIr::Array => {
            if value.is_array() {
                return Ok(());
            }
            "array"
        }
        FieldTypeIr::Timestamp => {
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

fn validate_allowed_values(value: &Value, allowed: &[&str], field_name: &str) -> Result<(), ValidationError> {
    if let Some(s) = value.as_str()
        && !allowed.contains(&s)
    {
        return Err(ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Must be one of: {}", allowed.join(", ")) });
    }
    Ok(())
}

fn validate_rules(value: &Value, rules: &ValidationIr, field_name: &str) -> Result<(), ValidationError> {
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

        if let Some(pattern) = rules.pattern {
            let re = {
                let mut cache = REGEX_CACHE.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(existing) = cache.get(pattern) {
                    existing.clone()
                } else {
                    let compiled = regex::Regex::new(pattern).map_err(|e| ValidationError::InvalidFieldValue { field: field_name.to_string(), message: format!("Invalid validation pattern '{}': {}", pattern, e) })?;
                    cache.entry(pattern.to_string()).or_insert(compiled).clone()
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
fn validate_extra_rules(value: &Value, rules: &ValidationIr, field_name: &str) -> Result<(), ValidationError> {
    let Some(extras) = rules.extra_rules else { return Ok(()) };
    let Some(s) = value.as_str() else { return Ok(()) };

    for &rule in extras {
        match rule {
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
fn validate_one_of_groups_refs(data: &Value, fields: &[&FieldIr]) -> Result<(), ValidationError> {
    let Some(obj) = data.as_object() else { return Ok(()) };

    for field in fields {
        let Some(rules) = field.validation.as_ref() else { continue };
        let Some(groups) = rules.one_of else { continue };
        for &group in groups {
            let set: Vec<&str> = group.iter().copied().filter(|name| obj.get(*name).is_some_and(|v| !v.is_null())).collect();
            if set.len() != 1 {
                let list = group.join(", ");
                let msg = if set.is_empty() { format!("[{}] WXCTL-V501: exactly one of ({list}) must be set; none provided", error_codes::V501) } else { format!("[{}] WXCTL-V501: exactly one of ({list}) must be set; got {} ({})", error_codes::V501, set.len(), set.join(", ")) };
                return Err(ValidationError::InvalidFieldValue { field: field.name.to_string(), message: msg });
            }
        }
    }
    Ok(())
}

/// Emit `WXCTL-V401` at warn level when a string field's value falls outside
/// `soft_allowed_values`. Does not fail validation — unlike `allowed_values`,
/// the soft variant trusts the API as the authority and only nudges the user.
fn warn_soft_allowed_values(value: &Value, rules: &ValidationIr, field_name: &str, resource_kind: &str, resource_name: &str) {
    let Some(soft) = rules.soft_allowed_values else { return };
    let Some(s) = value.as_str() else { return };
    if soft.contains(&s) {
        return;
    }
    tracing::warn!(target: "wxctl::warning", error_code = %error_codes::V401, resource_type = %resource_kind, resource_name = %resource_name, field_path = %field_name, value = %s, known_values = ?soft, "{}: field '{field_name}' value '{s}' is outside the known list; plan continues but the API may reject it", error_codes::V401);
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

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use serde_json::json;

    /// `resource:` header shared by every literal-fields test schema in this
    /// module: minimal api block, GetById discovery, `patch` update strategy.
    const HEADER: &str = r#"
resource:
  name: test
  service: test
  kind: test
  version: v1
  api:
    base_path: /api/test
    id_field: id
    get_endpoint: /api/test/{id}
    create_method: POST
    delete_method: DELETE
"#;

    /// Explicit empty `state_fields` bypasses the parser's auto-compute path,
    /// so the compiled schema carries exactly the fields the test declares.
    const FOOTER: &str = r#"
  reconciliation:
    discovery:
      method: get_by_id
      id_source: id
    state_fields: []
    update_strategy: patch
"#;

    /// Compile a `  schema:` YAML fragment (2-space indented under `resource:`,
    /// as produced by `HEADER`) into a leaked `&'static SchemaIr` via the
    /// production parse path. Test-only (see `ir_support::compile_to_static_ir`).
    fn compile(schema_yaml: &str) -> &'static SchemaIr {
        let yaml = format!("{HEADER}{schema_yaml}{FOOTER}");
        crate::ir_support::compile_to_static_ir(&yaml).unwrap_or_else(|e| panic!("test schema failed to compile: {e:#}\n---\n{yaml}"))
    }

    // ── apply_defaults ──

    #[test]
    fn apply_defaults_inserts_and_preserves() {
        let schema = compile(
            r#"
  schema:
    fields:
      - name: color
        type: string
        default: blue
"#,
        );

        // Missing field → default inserted.
        let mut absent = RawResource { kind: "test".into(), data: json!({}) };
        apply_defaults(&mut absent, schema);
        assert_eq!(absent.data.get("color"), Some(&json!("blue")));

        // Present field → existing value preserved (default must not overwrite).
        let mut present = RawResource { kind: "test".into(), data: json!({"color": "red"}) };
        apply_defaults(&mut present, schema);
        assert_eq!(present.data.get("color"), Some(&json!("red")));
    }

    // ── nested schema recursion (required / type / enum / item_type / defaults) ──

    /// Schema: `target` object with nested required `target_type` (enum) and
    /// optional `count` (integer, default 1); `thresholds` array of objects with
    /// nested `value`; `tags` array<string>.
    fn nested_schema() -> &'static SchemaIr {
        compile(
            r#"
  schema:
    fields:
      - name: target
        type: object
        schema:
          fields:
            - name: target_type
              type: string
              required: true
              allowed_values: [subscription, instance]
            - name: count
              type: integer
              default: 1
      - name: thresholds
        type: array
        item_type: object
        schema:
          fields:
            - name: value
              type: float
      - name: tags
        type: array
        item_type: string
"#,
        )
    }

    #[test]
    fn nested_schema_accept_paths() {
        let schema = nested_schema();
        let ok = json!({"target": {"target_type": "subscription", "count": 2}, "thresholds": [{"value": 0.8}], "tags": ["a", "b"]});
        assert!(validate_schema(&RawResource { kind: "test".into(), data: ok }, schema).is_ok());

        // Integer literals are valid floats (JSON-Schema `number`): `value: 1`
        // parses as i64, and nested enforcement must not reject it (live-hit:
        // pa_dimension `weight: 1` failed the plan gate once nested validation
        // became enforcing; TM1 accepts the integer on the wire).
        let int_for_float = json!({"thresholds": [{"value": 1}]});
        assert!(validate_schema(&RawResource { kind: "test".into(), data: int_for_float }, schema).is_ok());

        // Non-numeric values still fail the float check.
        let bad_float = json!({"thresholds": [{"value": "high"}]});
        assert!(validate_schema(&RawResource { kind: "test".into(), data: bad_float }, schema).is_err());

        // Template strings skip nested shape checks (they resolve after validation).
        let templated = json!({"target": {"target_type": "${env:TARGET_TYPE}"}, "tags": ["${model.m}"]});
        assert!(validate_schema(&RawResource { kind: "test".into(), data: templated }, schema).is_ok());

        // Undeclared nested keys warn (V401) but do not fail — nested schemas are partial.
        let extra = json!({"target": {"target_type": "instance", "extra_key": 1}});
        assert!(validate_schema(&RawResource { kind: "test".into(), data: extra }, schema).is_ok());
    }

    #[test]
    fn nested_schema_reject_paths() {
        let schema = nested_schema();
        // Each row: (data, expected error fragment, why).
        let cases: &[(Value, &str, &str)] = &[
            (json!({"target": {"count": 2}}), "target.target_type", "missing nested required"),
            (json!({"target": {"target_type": "bogus"}}), "target.target_type", "nested enum violation"),
            (json!({"target": {"target_type": "instance", "count": "two"}}), "target.count", "nested type mismatch"),
            (json!({"thresholds": [{"value": "high"}]}), "thresholds[0].value", "nested type mismatch inside array element"),
            (json!({"tags": [1, 2]}), "tags[0]", "array element type against item_type"),
        ];
        for (data, needle, why) in cases {
            let err = validate_schema(&RawResource { kind: "test".into(), data: data.clone() }, schema).unwrap_err();
            assert!(err.to_string().contains(needle), "{why}: expected '{needle}' in error, got: {err}");
        }
    }

    /// orchestrate_connection.environment is array<string> with element
    /// allowed_values [draft, live]. Phase 1 shipped the element-enforcement branch
    /// build-only; bind its behavior here on a schema mirroring that field shape.
    #[test]
    fn array_element_allowed_values_enforced() {
        let schema = compile(
            r#"
  schema:
    fields:
      - name: environment
        type: array
        item_type: string
        allowed_values: [draft, live]
"#,
        );

        // Valid element sets pass.
        for ok in [json!({"environment": ["draft"]}), json!({"environment": ["draft", "live"]})] {
            assert!(validate_schema(&RawResource { kind: "test".into(), data: ok.clone() }, schema).is_ok(), "{ok} should pass");
        }
        // A disallowed element ([production]) fails with an element-scoped error.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"environment": ["production"]}) }, schema).unwrap_err();
        assert!(err.to_string().contains("environment[0]"), "expected element-scoped error, got: {err}");
    }

    #[test]
    fn apply_defaults_recurses_into_nested_objects_and_array_elements() {
        let schema = nested_schema();

        // Present object → nested default filled; user value preserved.
        let mut r = RawResource { kind: "test".into(), data: json!({"target": {"target_type": "instance"}, "thresholds": [{}]}) };
        apply_defaults(&mut r, schema);
        assert_eq!(r.data["target"]["count"], json!(1), "nested default filled");
        // Absent parent stays absent — defaults never synthesize whole objects.
        let mut r2 = RawResource { kind: "test".into(), data: json!({}) };
        apply_defaults(&mut r2, schema);
        assert!(r2.data.get("target").is_none(), "absent parent must not be synthesized");
    }

    // ── validate_schema: required / from_id / computed ──

    #[test]
    fn validate_schema_required_field_paths() {
        let schema = compile(
            r#"
  schema:
    fields:
      - name: name
        type: string
        required: true
"#,
        );

        // Present → ok.
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"name": "hello"}) }, schema).is_ok());

        // Missing common required → MissingField (logged V001/V003).
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({}) }, schema).unwrap_err();
        assert!(matches!(err, ValidationError::MissingField { field, .. } if field == "name"));

        // `_from_id` dereference skips the required check entirely.
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"_from_id": true}) }, schema).is_ok());
    }

    #[test]
    fn validate_schema_computed_field_set() {
        let schema = compile(
            r#"
  schema:
    fields:
      - name: hash
        type: string
        location: Computed
"#,
        );

        let resource = RawResource { kind: "test".into(), data: json!({"hash": "abc"}) };
        let err = validate_schema(&resource, schema).unwrap_err();
        assert!(matches!(err, ValidationError::ComputedFieldSet { field } if field == "hash"));
    }

    // ── validate_schema: type / range / pattern / allowed-values rejects ──
    //
    // One table over every value-validation reject path: each row drives the
    // same `validate_schema` call to the same `Err`. Grouped by the error
    // variant they must surface so each distinct branch survives folding.

    #[test]
    fn validate_schema_type_mismatch_rejects() {
        // Each row: (field type YAML name, value, expected type named in the message). The
        // top-level check rewraps `TypeMismatch` (which carries no field path) into
        // `InvalidFieldValue` so the error names the offending field — previously
        // these surfaced as field-less TypeMismatch and the pipeline attributed
        // them to the literal field 'schema'.
        let cases: &[(&str, Value, &str)] = &[
            ("integer", json!("not_a_number"), "integer"),              // string where integer wanted
            ("timestamp", json!("not-a-date"), "timestamp (ISO 8601)"), // non-RFC3339 string
            ("array", json!("draft"), "array"),                         // scalar where array wanted (environment-field regression)
        ];
        for (field_type, value, expected) in cases {
            let schema = compile(&format!("\n  schema:\n    fields:\n      - name: f\n        type: {field_type}\n"));
            let resource = RawResource { kind: "test".into(), data: json!({ "f": value }) };
            let err = validate_schema(&resource, schema).unwrap_err();
            assert_eq!(err.field(), "f", "error must name the offending field for {value:?}");
            match err {
                ValidationError::InvalidFieldValue { ref message, .. } => assert!(message.contains(expected), "message {message:?} must name {expected} for {value:?}"),
                other => panic!("expected InvalidFieldValue naming 'f' (must be {expected}) for {value:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn validate_schema_valid_value_types_pass() {
        // Accept cases: well-formed values for type/timestamp must validate clean.
        let valid_ts = compile(
            r#"
  schema:
    fields:
      - name: created_at
        type: timestamp
"#,
        );
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"created_at": "2024-01-15T10:30:00Z"}) }, valid_ts).is_ok());
    }

    #[test]
    fn validate_schema_invalid_field_value_rejects() {
        // Each row: (field, schema YAML, value, why). All must surface
        // `InvalidFieldValue` naming the field — one row per distinct rule branch.
        let cases: Vec<(&str, &str, Value)> = vec![
            // allowed_values reject (hard list)
            (
                "status",
                r#"
  schema:
    fields:
      - name: status
        type: string
        allowed_values: [active, inactive]
"#,
                json!("deleted"),
            ),
            // min_length
            (
                "name",
                r#"
  schema:
    fields:
      - name: name
        type: string
        validation:
          min_length: 3
"#,
                json!("ab"),
            ),
            // max_length
            (
                "name",
                r#"
  schema:
    fields:
      - name: name
        type: string
        validation:
          max_length: 5
"#,
                json!("toolong"),
            ),
            // pattern mismatch
            (
                "code",
                r#"
  schema:
    fields:
      - name: code
        type: string
        validation:
          pattern: "^[A-Z]+$"
"#,
                json!("abc"),
            ),
            // min_value
            (
                "age",
                r#"
  schema:
    fields:
      - name: age
        type: integer
        validation:
          min_value: 0
          max_value: 150
"#,
                json!(-1),
            ),
            // max_value
            (
                "age",
                r#"
  schema:
    fields:
      - name: age
        type: integer
        validation:
          min_value: 0
          max_value: 150
"#,
                json!(200),
            ),
        ];
        for (field_name, schema_yaml, value) in cases {
            let schema = compile(schema_yaml);
            let resource = RawResource { kind: "test".into(), data: json!({ field_name: value }) };
            let err = validate_schema(&resource, schema).unwrap_err();
            assert!(matches!(err, ValidationError::InvalidFieldValue { ref field, .. } if field == field_name), "field {field_name} value {value:?} → {err:?}");
        }
    }

    #[test]
    fn soft_allowed_values_never_fails_validation() {
        // soft_allowed_values only nudges via tracing; in-list and out-of-list
        // both validate OK (unlike the hard `allowed_values` reject above).
        let schema = compile(
            r#"
  schema:
    fields:
      - name: type
        type: string
        validation:
          soft_allowed_values: [db2, mysql]
"#,
        );
        for v in ["db2", "not_a_real_connector"] {
            assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({ "type": v }) }, schema).is_ok(), "soft value {v} must not fail");
        }
    }

    // ── validate_schema: unknown / meta fields ──

    #[test]
    fn validate_schema_unknown_field() {
        let schema = compile(
            r#"
  schema:
    fields:
      - name: name
        type: string
"#,
        );
        let resource = RawResource { kind: "test".into(), data: json!({"name": "ok", "bogus": 42}) };
        let err = validate_schema(&resource, schema).unwrap_err();
        assert!(matches!(err, ValidationError::UnknownField { field } if field == "bogus"));
    }

    #[test]
    fn validate_schema_meta_fields_pass_through() {
        let schema = compile(
            r#"
  schema:
    fields: []
"#,
        );
        // Core meta-fields plus `depends_on` must all pass the unknown-field check.
        let resource = RawResource { kind: "test".into(), data: json!({"kind": "test", "ref_name": "foo", "_from_id": true, "id": "abc"}) };
        assert!(validate_schema(&resource, schema).is_ok());
        let with_depends = RawResource { kind: "test".into(), data: json!({"ref_name": "b", "depends_on": ["catalog.a"]}) };
        assert!(validate_schema(&with_depends, schema).is_ok(), "depends_on must pass the unknown-field check as a meta-field");
    }

    // ── on_destroy meta-field ──

    #[test]
    fn validate_schema_on_destroy_enum() {
        let schema = compile(
            r#"
  schema:
    fields: []
"#,
        );
        // Accept: both valid enum values.
        for v in ["retain", "delete"] {
            assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({ "on_destroy": v }) }, schema).is_ok(), "on_destroy={v}");
        }
        // Reject: a typo'd string must surface V009 (not silently default to Delete).
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"on_destroy": "retains"}) }, schema).unwrap_err();
        match err {
            ValidationError::InvalidFieldValue { field, message } => {
                assert_eq!(field, "on_destroy");
                assert!(message.contains("WXCTL-V009"));
            }
            other => panic!("expected V009 InvalidFieldValue, got {:?}", other),
        }
        // Reject: a non-string value too.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"on_destroy": true}) }, schema).unwrap_err();
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

    // ── variant-aware validation (V401 / V402) ──

    fn variant_schema() -> &'static SchemaIr {
        compile(
            r#"
  schema:
    fields:
      - name: type
        type: string
        required: true
      - name: name
        type: string
        required: true
    discriminator: type
    variants:
      s3_family:
        applies_to: [ibm_cos, aws_s3]
        fields:
          - name: access_key
            type: string
            required: true
          - name: secret_key
            type: string
            required: true
      adls_family:
        applies_to: [adls_gen2]
        fields:
          - name: account_name
            type: string
            required: true
          - name: sas_token
            type: string
"#,
        )
    }

    #[test]
    fn variant_aware_accept_paths() {
        let schema = variant_schema();

        // Required variant field present → ok. (Also covers `variant_fields_are_known_fields`:
        // s3_family's access_key lives in a variant, not common fields, yet must pass
        // the UnknownField check when set under type=ibm_cos.)
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak", "secret_key": "sk"}) }, schema).is_ok());

        // An ADLS-family field set while type=ibm_cos emits V401 warn but validation still succeeds.
        assert!(validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak", "secret_key": "sk", "account_name": "leftover"}) }, schema).is_ok());
    }

    #[test]
    fn variant_aware_reject_paths() {
        let schema = variant_schema();

        // Missing variant-scoped required → V402 naming the field and variant.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak"}) }, schema).unwrap_err();
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
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "access_key": "ak", "secret_key": "sk"}) }, schema).unwrap_err();
        assert!(matches!(err, ValidationError::MissingField { field, .. } if field == "name"));

        // A field declared by no variant is still a hard UnknownField.
        let err = validate_schema(&RawResource { kind: "test".into(), data: json!({"type": "ibm_cos", "name": "c1", "access_key": "ak", "secret_key": "sk", "completely_bogus": 1}) }, schema).unwrap_err();
        assert!(matches!(err, ValidationError::UnknownField { field } if field == "completely_bogus"));
    }

    #[test]
    fn apply_defaults_only_active_variant() {
        // Two variants, each with a field defaulted to a distinct value.
        let schema = compile(
            r#"
  schema:
    fields:
      - name: type
        type: string
        required: true
    discriminator: type
    variants:
      s3_family:
        applies_to: [ibm_cos]
        fields:
          - name: access_key
            type: string
            default: s3-default
      adls_family:
        applies_to: [adls_gen2]
        fields:
          - name: sas_token
            type: string
            default: adls-default
"#,
        );

        let mut resource = RawResource { kind: "test".into(), data: json!({"type": "ibm_cos"}) };
        apply_defaults(&mut resource, schema);

        assert_eq!(resource.data.get("access_key"), Some(&json!("s3-default")));
        assert!(resource.data.get("sas_token").is_none(), "inactive variant default must not leak");
    }

    // ── V403: redundant inline schema on a Python tool (AC3) ──
    #[test]
    fn v403_flags_redundant_python_tool_schema() {
        // Python tool + inline input_schema → flagged.
        let r = RawResource { kind: "tool".into(), data: json!({"ref_name": "t", "input_schema": {"type": "object"}, "binding": {"python": {"function": "t:main"}}}) };
        assert_eq!(redundant_python_tool_schema_fields(&r), vec!["input_schema"]);
        // Both inline schemas → both flagged, in field order.
        let r = RawResource { kind: "tool".into(), data: json!({"ref_name": "t", "input_schema": {"type": "object"}, "output_schema": {"type": "object"}, "binding": {"python": {"function": "t:main"}}}) };
        assert_eq!(redundant_python_tool_schema_fields(&r), vec!["input_schema", "output_schema"]);
        // Clean Python tool (schema.yaml authoritative, no inline) → nothing flagged.
        let r = RawResource { kind: "tool".into(), data: json!({"ref_name": "t", "source_path": "./resources/tool/t", "binding": {"python": {"function": "t:main"}}}) };
        assert!(redundant_python_tool_schema_fields(&r).is_empty());
        // Non-Python (openapi) tool with input_schema → NOT flagged (V403 is Python-only).
        let r = RawResource { kind: "tool".into(), data: json!({"ref_name": "api", "input_schema": {"type": "object"}, "binding": {"openapi": {"tools": ["*"]}}}) };
        assert!(redundant_python_tool_schema_fields(&r).is_empty());
        // Non-tool kind → never flagged.
        let r = RawResource { kind: "agent".into(), data: json!({"ref_name": "a", "input_schema": {"type": "object"}}) };
        assert!(redundant_python_tool_schema_fields(&r).is_empty());
    }
}
