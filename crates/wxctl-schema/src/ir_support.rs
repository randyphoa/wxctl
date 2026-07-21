//! Test-support helper: compile a schema-file YAML string into a leaked
//! `&'static` IR tree by driving the *production* parse path
//! (`wxctl_schema_compiler::SchemaParser::parse_str`) and then converting the owned
//! `ResourceSchema` into the `crate::ir::*` static type family.
//!
//! This is the runtime dual of the build-time codegen emitter in
//! `wxctl-schema-compiler/src/codegen/ir.rs`: that crate turns an owned
//! `ResourceSchema` into Rust *source text* naming `crate::ir::…` literals;
//! this module turns the same owned `ResourceSchema` into actual `crate::ir::…`
//! values at test time, `Box::leak`ing every owned piece into a `'static`
//! reference. The two must produce byte-identical `serde_json::to_value`
//! output for the same YAML input — that equivalence is what the Phase 3 E2E
//! gate checks.
//!
//! Conventions mirrored from the emitter (binding design decisions D2/D3):
//! - D2 (untyped fields): `default` / `list_body` / `absent_when.equals` are
//!   `serde_json::Value` in the owned model — leaked as their canonical JSON
//!   string (`serde_json::to_string`). `prompt` / deployment-overlay bodies are
//!   `serde_norway::Value` — converted via `serde_json::to_value` first, then
//!   the same canonical-JSON-string treatment; an overlay body that is YAML
//!   `null` leaks as the literal `"null"` string (mirroring the emitter's
//!   `Value::is_null` skip_serializing_if short-circuit), not `"null"`'s own
//!   round-tripped JSON encoding (which happens to be the same string, but for
//!   the same reason the emitter special-cases it rather than routing null
//!   through `serde_json::to_value`).
//! - D3 (map fields): `HashMap<String, V>` fields (`schema.variants`,
//!   `resource.deployments`) become `&'static [(&'static str, V)]` sorted by
//!   key, so serializing the pair slice as a map (`ser_opt_pairs_as_map` in
//!   `ir.rs`) reproduces the original map's *contents* deterministically
//!   (order does not affect map equality/serialization).
//!
//! Test-only: every conversion here leaks memory (`Box::leak`) for the
//! lifetime of the process. Never call `compile_to_static_ir` from library
//! (non-test) code.

use crate::ir::{AbsentWhenIr, ApiIr, DiscoveryIr, FieldIr, FieldReferencesIr, HookIr, IdentityHashIr, IdentityMatchIr, ListFilterIr, OverlayIr, ReadinessIr, ReconIr, SchemaBodyIr, SchemaIr, ValidationIr, VariantIr};
use std::collections::HashMap;
use wxctl_schema_compiler::definition::{
    AbsentWhen, ApiDefinition, DeploymentOverlay, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldLocation, FieldReferences, FieldType, HashStorage, HookDefinition, HttpMethod, IdentityHash, IdentityMatch, ListFilter, ReadinessDefinition, ReconciliationDefinition, ResourceDefinition,
    ResourceSchema, SchemaDefinition, UpdateStrategy, ValidationRules, VariantDefinition,
};

/// Compile a schema-file YAML string to a leaked `&'static SchemaIr`, going through the
/// production parse+normalize+reshape path (`wxctl_schema_compiler::SchemaParser::parse_str`).
/// Leaks (test-only); never call in library code.
pub fn compile_to_static_ir(yaml: &str) -> anyhow::Result<&'static crate::ir::SchemaIr> {
    let owned = wxctl_schema_compiler::SchemaParser::parse_str(yaml)?;
    Ok(leak_schema_ir(&owned))
}

// ---------------------------------------------------------------------------
// Leak primitives.
// ---------------------------------------------------------------------------

fn leak_str(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

fn leak_slice<T: 'static>(v: Vec<T>) -> &'static [T] {
    Box::leak(v.into_boxed_slice())
}

/// `&[String]` -> `&'static [&'static str]`.
fn leak_str_slice(v: &[String]) -> &'static [&'static str] {
    leak_slice(v.iter().map(|s| leak_str(s)).collect())
}

/// `serde_json::Value` (D2 untyped field) -> canonical-JSON `&'static str`.
fn leak_json(v: &serde_json::Value) -> &'static str {
    leak_str(&serde_json::to_string(v).expect("canonical json"))
}

/// `serde_norway::Value` (D2 untyped field: `prompt`) -> canonical-JSON `&'static str`,
/// via `serde_json::to_value` first per D2.
fn leak_yaml_json(v: &serde_norway::Value) -> &'static str {
    let j: serde_json::Value = serde_json::to_value(v).expect("yaml→json");
    leak_json(&j)
}

/// A `DeploymentOverlay` sub-block (D2): the literal `"null"` when the YAML value is
/// null (mirrors the emitter's `Value::is_null` skip_serializing_if), else its
/// canonical JSON literal.
fn leak_yaml_value_or_null(v: &serde_norway::Value) -> &'static str {
    if v.is_null() { "null" } else { leak_yaml_json(v) }
}

/// `HashMap<String, V>` (D3) -> `&'static [(&'static str, IrV)]` sorted by key.
fn leak_sorted_pairs<V, IrV: 'static>(map: &HashMap<String, V>, f: impl Fn(&V) -> IrV) -> &'static [(&'static str, IrV)] {
    let mut keys: Vec<&String> = map.keys().collect();
    keys.sort();
    let pairs: Vec<(&'static str, IrV)> = keys.into_iter().map(|k| (leak_str(k), f(&map[k.as_str()]))).collect();
    leak_slice(pairs)
}

// ---------------------------------------------------------------------------
// Enum owned -> IR conversions.
// ---------------------------------------------------------------------------

fn field_type_ir(t: &FieldType) -> crate::ir::FieldTypeIr {
    match t {
        FieldType::String => crate::ir::FieldTypeIr::String,
        FieldType::Integer => crate::ir::FieldTypeIr::Integer,
        FieldType::Float => crate::ir::FieldTypeIr::Float,
        FieldType::Boolean => crate::ir::FieldTypeIr::Boolean,
        FieldType::Object => crate::ir::FieldTypeIr::Object,
        FieldType::Array => crate::ir::FieldTypeIr::Array,
        FieldType::Timestamp => crate::ir::FieldTypeIr::Timestamp,
    }
}

fn field_location_ir(l: &FieldLocation) -> crate::ir::FieldLocationIr {
    match l {
        FieldLocation::Body => crate::ir::FieldLocationIr::Body,
        FieldLocation::Query => crate::ir::FieldLocationIr::Query,
        FieldLocation::Header => crate::ir::FieldLocationIr::Header,
        FieldLocation::Path => crate::ir::FieldLocationIr::Path,
        FieldLocation::Computed => crate::ir::FieldLocationIr::Computed,
        FieldLocation::LocalOnly => crate::ir::FieldLocationIr::LocalOnly,
    }
}

fn http_method_ir(m: &HttpMethod) -> crate::ir::HttpMethodIr {
    match m {
        HttpMethod::Get => crate::ir::HttpMethodIr::Get,
        HttpMethod::Post => crate::ir::HttpMethodIr::Post,
        HttpMethod::Put => crate::ir::HttpMethodIr::Put,
        HttpMethod::Patch => crate::ir::HttpMethodIr::Patch,
        HttpMethod::Delete => crate::ir::HttpMethodIr::Delete,
    }
}

fn discovery_method_ir(m: &DiscoveryMethod) -> crate::ir::DiscoveryMethodIr {
    match m {
        DiscoveryMethod::ListAndGet => crate::ir::DiscoveryMethodIr::ListAndGet,
        DiscoveryMethod::GetById => crate::ir::DiscoveryMethodIr::GetById,
        DiscoveryMethod::Skip => crate::ir::DiscoveryMethodIr::Skip,
        DiscoveryMethod::Singleton => crate::ir::DiscoveryMethodIr::Singleton,
    }
}

fn update_strategy_ir(u: &UpdateStrategy) -> crate::ir::UpdateStrategyIr {
    match u {
        UpdateStrategy::Patch => crate::ir::UpdateStrategyIr::Patch,
        UpdateStrategy::Replace => crate::ir::UpdateStrategyIr::Replace,
        UpdateStrategy::Recreate => crate::ir::UpdateStrategyIr::Recreate,
    }
}

fn hash_storage_ir(h: &HashStorage) -> crate::ir::HashStorageIr {
    match h {
        HashStorage::NameSuffix => crate::ir::HashStorageIr::NameSuffix,
        HashStorage::Tag => crate::ir::HashStorageIr::Tag,
        HashStorage::EnvMarker => crate::ir::HashStorageIr::EnvMarker,
        HashStorage::ServerSide => crate::ir::HashStorageIr::ServerSide,
        HashStorage::Local => crate::ir::HashStorageIr::Local,
    }
}

// ---------------------------------------------------------------------------
// Recursive owned -> leaked-IR converters (mirrors codegen/ir.rs's emit_* family).
// ---------------------------------------------------------------------------

fn leak_schema_ir(schema: &ResourceSchema) -> &'static SchemaIr {
    Box::leak(Box::new(SchemaIr { resource: leak_resource_def_ir(&schema.resource) }))
}

fn leak_resource_def_ir(def: &ResourceDefinition) -> crate::ir::ResourceDefIr {
    crate::ir::ResourceDefIr {
        name: leak_str(&def.name),
        service: leak_str(&def.service),
        kind: leak_str(&def.kind),
        version: leak_str(&def.version),
        api: leak_api_ir(&def.api),
        schema: leak_schema_body_ir(&def.schema),
        reconciliation: leak_recon_ir(&def.reconciliation),
        hooks: leak_hook_ir(&def.hooks),
        deployments: def.deployments.as_ref().map(|m| leak_sorted_pairs(m, leak_overlay_ir)),
        unsupported_on: leak_slice(def.unsupported_on.iter().map(|c| leak_str(&c.to_string())).collect()),
        description: def.description.as_deref().map(leak_str),
        prompt: def.prompt.as_ref().map(leak_yaml_json),
    }
}

fn leak_overlay_ir(o: &DeploymentOverlay) -> OverlayIr {
    OverlayIr { api: leak_yaml_value_or_null(&o.api), schema: leak_yaml_value_or_null(&o.schema), reconciliation: leak_yaml_value_or_null(&o.reconciliation), hooks: leak_yaml_value_or_null(&o.hooks) }
}

fn leak_api_ir(api: &ApiDefinition) -> ApiIr {
    ApiIr {
        base_path: leak_str(&api.base_path),
        id_field: leak_str(&api.id_field),
        list_endpoint: api.list_endpoint.as_deref().map(leak_str),
        get_endpoint: leak_str(&api.get_endpoint),
        create_endpoint: api.create_endpoint.as_deref().map(leak_str),
        create_method: http_method_ir(&api.create_method),
        update_endpoint: api.update_endpoint.as_deref().map(leak_str),
        update_method: api.update_method.as_ref().map(http_method_ir),
        delete_endpoint: api.delete_endpoint.as_deref().map(leak_str),
        delete_method: http_method_ir(&api.delete_method),
        readiness: api.readiness.as_ref().map(leak_readiness_ir),
    }
}

fn leak_readiness_ir(r: &ReadinessDefinition) -> ReadinessIr {
    ReadinessIr { state_path: leak_str(&r.state_path), ready: leak_str_slice(&r.ready), failed: leak_str_slice(&r.failed), timeout_env: r.timeout_env.as_deref().map(leak_str), timeout_default: r.timeout_default, interval_secs: r.interval_secs }
}

fn leak_schema_body_ir(s: &SchemaDefinition) -> SchemaBodyIr {
    SchemaBodyIr { fields: leak_slice(s.fields.iter().map(leak_field_ir).collect()), discriminator: s.discriminator.as_deref().map(leak_str), variants: s.variants.as_ref().map(|m| leak_sorted_pairs(m, leak_variant_ir)) }
}

fn leak_box_schema_body_ir(s: &SchemaDefinition) -> &'static SchemaBodyIr {
    Box::leak(Box::new(leak_schema_body_ir(s)))
}

fn leak_variant_ir(v: &VariantDefinition) -> VariantIr {
    VariantIr { applies_to: leak_str_slice(&v.applies_to), fields: leak_slice(v.fields.iter().map(leak_field_ir).collect()) }
}

fn leak_field_ir(f: &FieldDefinition) -> FieldIr {
    FieldIr {
        name: leak_str(&f.name),
        field_type: field_type_ir(&f.field_type),
        required: f.required,
        immutable: f.immutable,
        location: field_location_ir(&f.location),
        description: f.description.as_deref().map(leak_str),
        validation: f.validation.as_ref().map(leak_validation_ir),
        schema: f.schema.as_deref().map(leak_box_schema_body_ir),
        item_type: f.item_type.as_deref().map(field_type_ir),
        default: f.default.as_ref().map(leak_json),
        allowed_values: f.allowed_values.as_ref().map(|v| leak_str_slice(v)),
        references: f.references.as_ref().map(leak_field_references_ir),
        api_field: f.api_field.as_deref().map(leak_str),
        sensitive: f.sensitive,
        also_query: f.also_query,
        is_path: f.is_path,
        synthesize: f.synthesize,
        synth_shape: f.synth_shape.as_deref().map(leak_str),
    }
}

fn leak_validation_ir(v: &ValidationRules) -> ValidationIr {
    ValidationIr {
        min_length: v.min_length,
        max_length: v.max_length,
        max_length_bytes: v.max_length_bytes,
        pattern: v.pattern.as_deref().map(leak_str),
        min_value: v.min_value,
        max_value: v.max_value,
        max_items: v.max_items,
        soft_allowed_values: v.soft_allowed_values.as_ref().map(|vv| leak_str_slice(vv)),
        one_of: v.one_of.as_ref().map(|vv| leak_slice(vv.iter().map(|inner| leak_str_slice(inner)).collect())),
        extra_rules: v.extra_rules.as_ref().map(|vv| leak_str_slice(vv)),
    }
}

fn leak_field_references_ir(r: &FieldReferences) -> FieldReferencesIr {
    FieldReferencesIr { resource: leak_str(&r.resource), field: leak_str(&r.field), also_allows: leak_str_slice(&r.also_allows), optional: r.optional, require_ready: r.require_ready, relationship: r.relationship.as_deref().map(leak_str) }
}

fn leak_recon_ir(r: &ReconciliationDefinition) -> ReconIr {
    ReconIr {
        discovery: leak_discovery_ir(&r.discovery),
        identity_hash: r.identity_hash.as_ref().map(leak_identity_hash_ir),
        state_fields: r.state_fields.as_ref().map(|v| leak_str_slice(v)),
        update_strategy: update_strategy_ir(&r.update_strategy),
        immutable_fields: leak_str_slice(&r.immutable_fields),
        reject_on_immutable_drift: r.reject_on_immutable_drift,
        use_json_patch: r.use_json_patch,
        json_patch_path_prefix: r.json_patch_path_prefix.as_deref().map(leak_str),
    }
}

fn leak_identity_hash_ir(h: &IdentityHash) -> IdentityHashIr {
    IdentityHashIr { fields: leak_str_slice(&h.fields), nonce_field: h.nonce_field.as_deref().map(leak_str), storage: hash_storage_ir(&h.storage), length: h.length }
}

fn leak_discovery_ir(d: &DiscoveryDefinition) -> DiscoveryIr {
    DiscoveryIr {
        method: discovery_method_ir(&d.method),
        list_field: d.list_field.as_deref().map(leak_str),
        id_source: leak_str(&d.id_source),
        name_field: d.name_field.as_deref().map(leak_str),
        identity_match: d.identity_match.as_ref().map(leak_identity_match_ir),
        absent_when: d.absent_when.as_ref().map(leak_absent_when_ir),
        list_method: d.list_method.as_deref().map(leak_str),
        list_body: d.list_body.as_ref().map(leak_json),
        list_map: d.list_map,
        list_filter: d.list_filter.as_ref().map(leak_list_filter_ir),
    }
}

fn leak_absent_when_ir(a: &AbsentWhen) -> AbsentWhenIr {
    AbsentWhenIr { field: leak_str(&a.field), equals: leak_json(&a.equals) }
}

fn leak_identity_match_ir(m: &IdentityMatch) -> IdentityMatchIr {
    IdentityMatchIr { local_path: leak_str(&m.local_path), remote_path: leak_str(&m.remote_path) }
}

fn leak_list_filter_ir(f: &ListFilter) -> ListFilterIr {
    ListFilterIr { field: leak_str(&f.field), equals: leak_str(&f.equals) }
}

fn leak_hook_ir(h: &HookDefinition) -> HookIr {
    HookIr {
        pre_create: h.pre_create.as_deref().map(leak_str),
        post_create: h.post_create.as_deref().map(leak_str),
        pre_update: h.pre_update.as_deref().map(leak_str),
        post_update: h.post_update.as_deref().map(leak_str),
        pre_delete: h.pre_delete.as_deref().map(leak_str),
        post_delete: h.post_delete.as_deref().map(leak_str),
    }
}
