//! Static (`&'static`) intermediate-representation (IR) type family mirroring
//! the owned schema model in `schema::definition` and the descriptor model in
//! `descriptor`. These types exist so `build.rs` (a later task) can emit fully
//! baked-in-the-binary schema data with `include!`, replacing runtime YAML
//! parsing, while keeping the *serialized* shape byte-identical to today's
//! `serde_norway::to_value(&ResourceSchema)` / descriptor projections that
//! `render.rs` and `explain.rs` depend on.
//!
//! Pure data + `Serialize` impls only — no dependency on `wxctl-schema-compiler`
//! (Invariant I2: this crate stays wasm-safe, no new runtime deps). Unconsumed
//! this phase: nothing in `wxctl-schema` yet builds or reads these types; they
//! compile as dead (but not `dead_code`-linted, since they're `pub` in a
//! library crate) scaffolding for the codegen emitter landing in a later task.

use serde::{Serialize, Serializer};

// ---------------------------------------------------------------------------
// Enums — own copies of schema::definition's fieldless enums, identical serde
// shape. Not re-exports: these must survive Phase 2's deletion of
// `src/schema/`.
// ---------------------------------------------------------------------------

/// ↔ `schema::definition::FieldType` (definition.rs:277).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldTypeIr {
    String,
    Integer,
    Float,
    Boolean,
    Object,
    Array,
    Timestamp,
}

/// ↔ `schema::definition::FieldLocation` (definition.rs:176).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "PascalCase")]
pub enum FieldLocationIr {
    Body,
    Query,
    Header,
    Path,
    Computed,
    LocalOnly,
}

/// ↔ `schema::definition::HttpMethod` (definition.rs:140).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethodIr {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

/// ↔ `schema::definition::DiscoveryMethod` (definition.rs:560).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMethodIr {
    ListAndGet,
    GetById,
    Skip,
    Singleton,
}

/// ↔ `schema::definition::UpdateStrategy` (definition.rs:572).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStrategyIr {
    Patch,
    Replace,
    Recreate,
}

/// ↔ `schema::definition::HashStorage` (definition.rs:363).
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HashStorageIr {
    NameSuffix,
    Tag,
    EnvMarker,
    ServerSide,
    Local,
}

// ---------------------------------------------------------------------------
// serialize_with helpers (D2 untyped-JSON fields, D3 sorted-map fields).
// ---------------------------------------------------------------------------

/// Parse a canonical-JSON string literal and serialize the resulting value, so an
/// untyped `&'static str` IR field serializes identically to the owned `serde_json::Value`.
fn ser_raw_json<S: Serializer>(s: &&'static str, ser: S) -> Result<S::Ok, S::Error> {
    let v: serde_json::Value = serde_json::from_str(s).map_err(serde::ser::Error::custom)?;
    v.serialize(ser)
}
fn ser_opt_raw_json<S: Serializer>(s: &Option<&'static str>, ser: S) -> Result<S::Ok, S::Error> {
    match s {
        Some(x) => ser_raw_json(x, ser),
        None => ser.serialize_none(),
    }
}
/// Serialize a sorted pair slice as a map (owned HashMap fields serialize as maps; D3).
fn ser_opt_pairs_as_map<K: Serialize, V: Serialize, S: Serializer>(pairs: &Option<&'static [(K, V)]>, ser: S) -> Result<S::Ok, S::Error> {
    match pairs {
        Some(ps) => ser.collect_map(ps.iter().map(|(k, v)| (k, v))),
        None => ser.serialize_none(),
    }
}
/// True when a `DeploymentOverlay`-body JSON literal is the canonical "absent" value,
/// mirroring the owned `Value::is_null` skip_serializing_if (definition.rs:55-62).
fn is_json_null(s: &&'static str) -> bool {
    *s == "null"
}

// ---------------------------------------------------------------------------
// Struct family — one IR struct per owned type in schema::definition, serde
// attrs replicated so `serde_norway::to_value(&SchemaIr)` matches
// `serde_norway::to_value(&ResourceSchema)`.
// ---------------------------------------------------------------------------

/// ↔ `ResourceSchema` (definition.rs:6).
#[derive(Debug, Serialize)]
pub struct SchemaIr {
    pub resource: ResourceDefIr,
}

/// ↔ `ResourceDefinition` (definition.rs:16).
#[derive(Debug, Serialize)]
pub struct ResourceDefIr {
    pub name: &'static str,
    pub service: &'static str,
    pub kind: &'static str,
    pub version: &'static str,
    pub api: ApiIr,
    pub schema: SchemaBodyIr,
    pub reconciliation: ReconIr,
    #[serde(default)]
    pub hooks: HookIr,
    /// ↔ `Option<HashMap<String, DeploymentOverlay>>` (D3): sorted pairs, serialized as a map.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_pairs_as_map")]
    pub deployments: Option<&'static [(&'static str, OverlayIr)]>,
    #[serde(default, skip_serializing_if = "<[_]>::is_empty")]
    pub unsupported_on: &'static [&'static str],
    #[serde(default)]
    pub description: Option<&'static str>,
    /// ↔ `Option<serde_norway::Value>` (D2): canonical JSON string, parsed back at serialize time.
    #[serde(default, serialize_with = "ser_opt_raw_json")]
    pub prompt: Option<&'static str>,
}

/// ↔ `DeploymentOverlay` (definition.rs:53-63).
#[derive(Debug, Serialize)]
pub struct OverlayIr {
    #[serde(skip_serializing_if = "is_json_null", serialize_with = "ser_raw_json")]
    pub api: &'static str,
    #[serde(skip_serializing_if = "is_json_null", serialize_with = "ser_raw_json")]
    pub schema: &'static str,
    #[serde(skip_serializing_if = "is_json_null", serialize_with = "ser_raw_json")]
    pub reconciliation: &'static str,
    #[serde(skip_serializing_if = "is_json_null", serialize_with = "ser_raw_json")]
    pub hooks: &'static str,
}

/// ↔ `ApiDefinition` (definition.rs:71).
#[derive(Debug, Serialize)]
pub struct ApiIr {
    pub base_path: &'static str,
    pub id_field: &'static str,
    #[serde(default)]
    pub list_endpoint: Option<&'static str>,
    pub get_endpoint: &'static str,
    #[serde(default)]
    pub create_endpoint: Option<&'static str>,
    pub create_method: HttpMethodIr,
    #[serde(default)]
    pub update_endpoint: Option<&'static str>,
    #[serde(default)]
    pub update_method: Option<HttpMethodIr>,
    #[serde(default)]
    pub delete_endpoint: Option<&'static str>,
    pub delete_method: HttpMethodIr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ReadinessIr>,
}

/// ↔ `ReadinessDefinition` (definition.rs:109).
#[derive(Debug, Serialize)]
pub struct ReadinessIr {
    pub state_path: &'static str,
    pub ready: &'static [&'static str],
    #[serde(default)]
    pub failed: &'static [&'static str],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_env: Option<&'static str>,
    pub timeout_default: u32,
    pub interval_secs: u32,
}

/// ↔ `SchemaDefinition` (definition.rs:150).
#[derive(Debug, Serialize)]
pub struct SchemaBodyIr {
    pub fields: &'static [FieldIr],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<&'static str>,
    /// ↔ `Option<HashMap<String, VariantDefinition>>` (D3): sorted pairs, serialized as a map.
    #[serde(default, skip_serializing_if = "Option::is_none", serialize_with = "ser_opt_pairs_as_map")]
    pub variants: Option<&'static [(&'static str, VariantIr)]>,
}

/// ↔ `VariantDefinition` (definition.rs:168).
#[derive(Debug, Serialize)]
pub struct VariantIr {
    pub applies_to: &'static [&'static str],
    #[serde(default)]
    pub fields: &'static [FieldIr],
}

/// ↔ `FieldDefinition` (definition.rs:203). The `properties` field is
/// deliberately omitted: it is dead post-parse (definition.rs:254-259).
#[derive(Debug, Serialize)]
pub struct FieldIr {
    pub name: &'static str,
    #[serde(rename = "type")]
    pub field_type: FieldTypeIr,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub immutable: bool,
    #[serde(default)]
    pub location: FieldLocationIr,
    #[serde(default)]
    pub description: Option<&'static str>,
    #[serde(default)]
    pub validation: Option<ValidationIr>,
    #[serde(default)]
    pub schema: Option<&'static SchemaBodyIr>,
    #[serde(default)]
    pub item_type: Option<FieldTypeIr>,
    /// ↔ `Option<serde_json::Value>` (D2): canonical JSON string, parsed back at serialize time.
    #[serde(default, serialize_with = "ser_opt_raw_json")]
    pub default: Option<&'static str>,
    #[serde(default)]
    pub allowed_values: Option<&'static [&'static str]>,
    #[serde(default)]
    pub references: Option<FieldReferencesIr>,
    #[serde(default)]
    pub api_field: Option<&'static str>,
    #[serde(default)]
    pub sensitive: bool,
    #[serde(default)]
    pub also_query: bool,
    #[serde(default)]
    pub is_path: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesize: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synth_shape: Option<&'static str>,
}

/// ↔ `ValidationRules` (definition.rs:289).
#[derive(Debug, Serialize)]
pub struct ValidationIr {
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub max_length: Option<usize>,
    #[serde(default)]
    pub max_length_bytes: Option<usize>,
    #[serde(default)]
    pub pattern: Option<&'static str>,
    #[serde(default)]
    pub min_value: Option<i64>,
    #[serde(default)]
    pub max_value: Option<i64>,
    #[serde(default)]
    pub max_items: Option<usize>,
    #[serde(default)]
    pub soft_allowed_values: Option<&'static [&'static str]>,
    #[serde(default)]
    pub one_of: Option<&'static [&'static [&'static str]]>,
    #[serde(default)]
    pub extra_rules: Option<&'static [&'static str]>,
}

/// ↔ `FieldReferences` (definition.rs:327).
#[derive(Debug, Serialize)]
pub struct FieldReferencesIr {
    pub resource: &'static str,
    pub field: &'static str,
    #[serde(default)]
    pub also_allows: &'static [&'static str],
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub require_ready: bool,
    #[serde(default)]
    pub relationship: Option<&'static str>,
}

/// ↔ `IdentityHash` (definition.rs:396).
#[derive(Debug, Serialize)]
pub struct IdentityHashIr {
    pub fields: &'static [&'static str],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce_field: Option<&'static str>,
    #[serde(default)]
    pub storage: HashStorageIr,
    pub length: usize,
}

/// ↔ `ReconciliationDefinition` (definition.rs:417).
#[derive(Debug, Serialize)]
pub struct ReconIr {
    pub discovery: DiscoveryIr,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_hash: Option<IdentityHashIr>,
    #[serde(default)]
    pub state_fields: Option<&'static [&'static str]>,
    pub update_strategy: UpdateStrategyIr,
    #[serde(default)]
    pub immutable_fields: &'static [&'static str],
    #[serde(default)]
    pub reject_on_immutable_drift: bool,
    pub use_json_patch: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_patch_path_prefix: Option<&'static str>,
}

/// ↔ `DiscoveryDefinition` (definition.rs:451).
#[derive(Debug, Serialize)]
pub struct DiscoveryIr {
    pub method: DiscoveryMethodIr,
    #[serde(default)]
    pub list_field: Option<&'static str>,
    #[serde(default)]
    pub id_source: &'static str,
    #[serde(default)]
    pub name_field: Option<&'static str>,
    #[serde(default)]
    pub identity_match: Option<IdentityMatchIr>,
    #[serde(default)]
    pub absent_when: Option<AbsentWhenIr>,
    #[serde(default)]
    pub list_method: Option<&'static str>,
    /// ↔ `Option<serde_json::Value>` (D2): canonical JSON string, parsed back at serialize time.
    #[serde(default, serialize_with = "ser_opt_raw_json")]
    pub list_body: Option<&'static str>,
    #[serde(default)]
    pub list_map: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_filter: Option<ListFilterIr>,
}

/// ↔ `AbsentWhen` (definition.rs:522).
#[derive(Debug, Serialize)]
pub struct AbsentWhenIr {
    pub field: &'static str,
    /// ↔ `serde_json::Value` (D2): canonical JSON string, parsed back at serialize time.
    #[serde(serialize_with = "ser_raw_json")]
    pub equals: &'static str,
}

/// ↔ `IdentityMatch` (definition.rs:534).
#[derive(Debug, Serialize)]
pub struct IdentityMatchIr {
    pub local_path: &'static str,
    pub remote_path: &'static str,
}

/// ↔ `ListFilter` (definition.rs:552).
#[derive(Debug, Serialize)]
pub struct ListFilterIr {
    pub field: &'static str,
    pub equals: &'static str,
}

/// ↔ `HookDefinition` (definition.rs:580).
#[derive(Debug, Default, Serialize)]
pub struct HookIr {
    #[serde(default)]
    pub pre_create: Option<&'static str>,
    #[serde(default)]
    pub post_create: Option<&'static str>,
    #[serde(default)]
    pub pre_update: Option<&'static str>,
    #[serde(default)]
    pub post_update: Option<&'static str>,
    #[serde(default)]
    pub pre_delete: Option<&'static str>,
    #[serde(default)]
    pub post_delete: Option<&'static str>,
}

// ---------------------------------------------------------------------------
// Descriptor IR — precomputed-descriptor surface, mirrors descriptor.rs:4-32.
// ---------------------------------------------------------------------------

/// ↔ `ResourceDescriptor` (descriptor.rs:4).
#[derive(Debug, Serialize)]
pub struct DescriptorIr {
    pub name: &'static str,
    pub service: &'static str,
    pub kind: &'static str,
    pub id_field: &'static str,
    pub endpoints: EndpointsIr,
    pub fields: &'static [FieldDescriptorIr],
    pub schema: &'static SchemaIr,
}

/// ↔ `Endpoints` (descriptor.rs:15).
#[derive(Debug, Serialize)]
pub struct EndpointsIr {
    pub base_path: &'static str,
    pub list: Option<&'static str>,
    pub get: &'static str,
    pub create: &'static str,
    pub update: Option<&'static str>,
    pub update_method: Option<HttpMethodIr>,
    pub delete: &'static str,
}

/// ↔ `FieldDescriptor` (descriptor.rs:26).
#[derive(Debug, Serialize)]
pub struct FieldDescriptorIr {
    pub name: &'static str,
    pub required: bool,
    pub immutable: bool,
    pub location: FieldLocationIr,
}

// ---------------------------------------------------------------------------
// Generated static IR (schemas, per-deployment variants, descriptors, phf
// lookups) — emitted by `wxctl-schema-compiler::codegen::ir::generate_ir` and
// spliced in here so every `crate::ir::…` path referenced by the generated
// source resolves inside this module.
// ---------------------------------------------------------------------------

include!(concat!(env!("OUT_DIR"), "/schema_ir_generated.rs"));
