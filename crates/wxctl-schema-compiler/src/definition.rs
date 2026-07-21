use crate::deployment::DeploymentConstraint;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResourceSchema {
    pub resource: ResourceDefinition,
}

// NOTE: does not derive `Default`. Adding a non-defaulted field here breaks every
// full-field `ResourceDefinition { .. }` literal across the workspace (engine/sdk
// `#[cfg(test)]` helpers) with `E0063 missing field` — `#[serde(default)]` covers
// deserialization, not Rust literals. Gate such changes on `cargo build --all-targets`
// (or workspace `cargo clippy --all-targets`), not a crate-scoped `-p <crate>` build.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceDefinition {
    pub name: String,
    pub service: String,
    pub kind: String,
    pub version: String,
    pub api: ApiDefinition,
    pub schema: SchemaDefinition,
    pub reconciliation: ReconciliationDefinition,
    #[serde(default)]
    pub hooks: HookDefinition,
    /// Per-deployment overlays. Keys are constraint strings (`"saas"`,
    /// `"software"`, `"software-5.3"`). At runtime the most-specific
    /// matching key wins; its overlay is deep-merged onto this base.
    /// Absent or empty map means the base block applies to all deployments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployments: Option<HashMap<String, DeploymentOverlay>>,
    /// Constraints under which this resource kind is not supported.
    /// Planning a resource matching any constraint errors with `R004`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported_on: Vec<DeploymentConstraint>,

    /// Human-readable resource description (first sentence is baked into the
    /// build graph's RESOURCE_CATALOG). Parsed but otherwise unused at runtime.
    #[serde(default)]
    pub description: Option<String>,
    /// Optional prompt-authoring block. Parsed but unused at runtime (build.rs
    /// bakes `prompt.notes` separately). Declared so `deny_unknown_fields` accepts it.
    #[serde(default)]
    pub prompt: Option<serde_norway::Value>,
}

/// Partial overlay of a `ResourceDefinition`, applied via deep merge.
/// Every field is optional — overlay only what differs from the base.
/// Stored as `serde_norway::Value` so the merge is structural and supports
/// arbitrarily nested overrides without enumerating every field type.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct DeploymentOverlay {
    #[serde(default, skip_serializing_if = "serde_norway::Value::is_null")]
    pub api: serde_norway::Value,
    #[serde(default, skip_serializing_if = "serde_norway::Value::is_null")]
    pub schema: serde_norway::Value,
    #[serde(default, skip_serializing_if = "serde_norway::Value::is_null")]
    pub reconciliation: serde_norway::Value,
    #[serde(default, skip_serializing_if = "serde_norway::Value::is_null")]
    pub hooks: serde_norway::Value,
}

impl DeploymentOverlay {
    pub fn is_empty(&self) -> bool {
        self.api.is_null() && self.schema.is_null() && self.reconciliation.is_null() && self.hooks.is_null()
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ApiDefinition {
    pub base_path: String,
    pub id_field: String,
    #[serde(default)]
    pub list_endpoint: Option<String>,
    pub get_endpoint: String,

    /// Custom create endpoint (default: base_path)
    #[serde(default)]
    pub create_endpoint: Option<String>,
    pub create_method: HttpMethod,

    /// Custom update endpoint (default: get_endpoint)
    #[serde(default)]
    pub update_endpoint: Option<String>,
    #[serde(default)]
    pub update_method: Option<HttpMethod>,

    /// Custom delete endpoint (default: get_endpoint)
    #[serde(default)]
    pub delete_endpoint: Option<String>,
    pub delete_method: HttpMethod,

    /// Optional readiness contract: how to tell a resource of this kind is
    /// fully provisioned. Consumed by the engine reference-readiness gate
    /// (Phase 2). Absent for every kind that provisions synchronously.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<ReadinessDefinition>,
}

/// Declares how to tell that a resource of this kind is fully provisioned
/// ("ready") by polling its GET response. Consumed by the engine's
/// reference-readiness gate (Phase 2): a reference marked `require_ready`
/// waits until the target's `state_path` holds a `ready` value before the
/// consumer's create POST. Pure data — no I/O; the poll lives in
/// `wxctl-engine`, keeping this crate wasm-safe. Deliberately no `Default`
/// derive (see the hazard note at the top of this file).
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ReadinessDefinition {
    /// Dot path into the GET response holding the status value
    /// (e.g. `entity.status.state`).
    pub state_path: String,
    /// Status values that mean "ready" (e.g. `[active]`).
    pub ready: Vec<String>,
    /// Status values that mean "failed"; the gate bails immediately
    /// (e.g. `[error, disabled]`).
    #[serde(default)]
    pub failed: Vec<String>,
    /// Optional env var overriding the poll budget
    /// (e.g. `WXCTL_DATA_MART_READY_TIMEOUT`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_env: Option<String>,
    /// Poll budget in seconds when `timeout_env` is unset/invalid (default 300).
    #[serde(default = "default_readiness_timeout")]
    pub timeout_default: u32,
    /// Seconds between polls (default 5).
    #[serde(default = "default_readiness_interval")]
    pub interval_secs: u32,
}

fn default_readiness_timeout() -> u32 {
    300
}

fn default_readiness_interval() -> u32 {
    5
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SchemaDefinition {
    pub fields: Vec<FieldDefinition>,
    /// Name of the field that discriminates between variants (e.g. `type`).
    /// When set, `variants` MUST be populated; the discriminator field itself
    /// must appear in `fields` with `allowed_values` or `soft_allowed_values`
    /// listing every key under `variants`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discriminator: Option<String>,
    /// Variant-scoped field groups. Each entry's `applies_to` lists the
    /// discriminator values that activate its `fields`. Validators merge the
    /// active variant's fields with `common_fields` (top-level `fields`) for
    /// per-resource validation, `sensitive_paths`, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<HashMap<String, VariantDefinition>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct VariantDefinition {
    pub applies_to: Vec<String>,
    #[serde(default)]
    pub fields: Vec<FieldDefinition>,
}

/// Specifies where a field should be placed in HTTP requests
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "PascalCase")]
pub enum FieldLocation {
    /// Field is sent in request body JSON
    #[default]
    Body,

    /// Field is sent as URL query parameter (?key=value)
    Query,

    /// Field is sent as HTTP header
    Header,

    /// Field is interpolated into path template {placeholder}
    Path,

    /// Field is computed from other fields, never sent to API
    Computed,

    /// Field is configuration-only, never sent to API
    LocalOnly,
}

// NOTE: does not derive `Default` — same workspace-wide literal hazard as
// `ResourceDefinition` above: adding a non-defaulted field breaks full-field
// `FieldDefinition { .. }` literals in sibling crates' test helpers. Gate on
// `cargo build --all-targets`, not a `-p <crate>` build.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FieldDefinition {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: FieldType,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub immutable: bool,

    /// Field location in HTTP requests
    #[serde(default)]
    pub location: FieldLocation,

    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub validation: Option<ValidationRules>,
    #[serde(default)]
    pub schema: Option<Box<SchemaDefinition>>,
    #[serde(default)]
    pub item_type: Option<Box<FieldType>>,
    #[serde(default)]
    pub default: Option<serde_json::Value>,
    #[serde(default)]
    pub allowed_values: Option<Vec<String>>,
    #[serde(default)]
    pub references: Option<FieldReferences>,

    /// Optional API field path for nested field mapping.
    /// When specified, the user-facing field name maps to this nested path in API requests/responses.
    /// Example: "additional_properties.icon" means the field is nested under additional_properties.
    #[serde(default)]
    pub api_field: Option<String>,

    /// When true, the field's value is masked (`***`) in plan diffs, structured
    /// logs, and OTel attributes. State comparison and reconciliation still
    /// operate on real values — redaction is output-only. Layers on top of
    /// keyword-based redaction in `logging::redaction` for precision.
    #[serde(default)]
    pub sensitive: bool,

    /// When true, the field is also added to the query string on list/get/delete
    /// (bodyless) calls and contributes to discovery scoping params, in addition
    /// to its declared `location`. Lets a single field straddle the WML
    /// convention where `space_id`/`project_id` are sent in the body on POST
    /// but as query params on every other operation.
    #[serde(default)]
    pub also_query: bool,

    /// Nested object sub-fields keyed by name. Mirrors `build.rs FieldDef.properties`
    /// (the build-graph spelling for object/array item sub-fields). Parsed but not
    /// consumed at runtime — the runtime model uses `schema:` for nesting. Declared
    /// so `deny_unknown_fields` accepts the schemas that use `properties:`.
    /// Always `None` post-parse (`normalize_properties` rewrites `properties:` into
    /// `schema.fields` before deserialization), so it's skipped on serialize: the
    /// owned model's serialization then matches the IR (`wxctl-schema/src/ir.rs`
    /// `FieldIr`), which omits this field entirely (see tests/ir_parse_equivalence.rs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<HashMap<String, FieldDefinition>>,

    /// When true, the field holds a local filesystem path that `resolve_file_paths`
    /// resolves against the config dir. The build-time guard enforces `LocalOnly`.
    #[serde(default)]
    pub is_path: bool,

    /// Data-provisioning marker: `Some(true)` opts this field into synthesized-data
    /// detection, `Some(false)` suppresses inference, `None` leaves it to `is_path`.
    /// Parsed so `deny_unknown_fields` accepts annotated schemas; consumed only by
    /// `build.rs` → `SYNTH_FIELDS` (inert at runtime, like `properties`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synthesize: Option<bool>,
    /// Optional shape hint (e.g. "csv") paired with `synthesize`. Same build-only role.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub synth_shape: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldType {
    String,
    Integer,
    Float,
    Boolean,
    Object,
    Array,
    Timestamp,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ValidationRules {
    #[serde(default)]
    pub min_length: Option<usize>,
    #[serde(default)]
    pub max_length: Option<usize>,
    /// UTF-8 byte-length limit (S3 object keys, etc). Distinct from
    /// `max_length` which counts chars.
    #[serde(default)]
    pub max_length_bytes: Option<usize>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub min_value: Option<i64>,
    #[serde(default)]
    pub max_value: Option<i64>,
    /// Max array length (e.g. S3 bucket tag cap of 10).
    #[serde(default)]
    pub max_items: Option<usize>,
    /// Soft allowlist: values outside this list emit `WXCTL-V401` at warn
    /// level but do not fail validation. Used when the canonical list can
    /// grow faster than we can mirror it (e.g. database connector types).
    #[serde(default)]
    pub soft_allowed_values: Option<Vec<String>>,
    /// Cross-field mutual-exclusivity groups. Each inner list names
    /// sibling fields of which exactly one must be set. Violations emit
    /// `WXCTL-V501`. Only evaluated on object-typed fields whose parent
    /// owns the named siblings.
    #[serde(default)]
    pub one_of: Option<Vec<Vec<String>>>,
    /// Extra named validation rules that can't be expressed with the primitives
    /// above. Each rule is an opaque string that the engine's validator knows
    /// how to interpret. Used for S3 bucket name rules (`no_consecutive_dots`,
    /// `not_ip_address`, `no_reserved_prefix`, `no_reserved_suffix`).
    #[serde(default)]
    pub extra_rules: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct FieldReferences {
    /// Target resource kind (e.g., "orchestrate_connection", "model")
    pub resource: String,
    /// Target field name on the referenced resource (e.g., "asset_id", "id")
    pub field: String,
    /// Additional resource kinds that are also valid for this field.
    /// Used when a field can reference multiple resource types (e.g., asset
    /// can reference either ai_service or wml_function).
    #[serde(default)]
    pub also_allows: Vec<String>,
    /// When true, the reference is advisory: a required field carrying it is
    /// relaxed (a literal value is accepted in place of a `${kind.name}`
    /// template, and the field's dependency edge is not force-required). It
    /// does NOT legitimize a dangling `${kind.name}` template: a template
    /// whose referent is absent still fails with the `WXCTL-V005` hard error.
    /// Default `false` preserves the existing strict behavior for all
    /// pre-existing schemas. Already recognised at build time in
    /// `wxctl-providers/build.rs`.
    #[serde(default)]
    pub optional: bool,
    /// When true, the engine gates the consumer's create on this reference's
    /// target reaching its readiness state; the target kind must declare an
    /// `api.readiness` block (enforced by `validation::readiness`). Default
    /// `false` preserves the existing behavior for all pre-existing schemas.
    #[serde(default)]
    pub require_ready: bool,
    /// Taxonomy marker (OQ1): `containment` marks a parent-child lifecycle
    /// reference, exported as `mechanism: containment`. Metadata only — no
    /// engine behavior. Default `None` = ordinary reference.
    #[serde(default)]
    pub relationship: Option<String>,
}

/// How a job kind's identity hash is stored on the remote object.
/// Declared on `reconciliation.identity_hash`.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum HashStorage {
    /// Created object name = `<name>-<hash>`; discovery matches the suffixed name.
    #[default]
    NameSuffix,
    /// `run-hash:<hex>` in the API `tags` array; discovery matches the tagged item.
    Tag,
    /// `WXCTL_IDENTITY=<hex>` entry injected into the kind's `env_variables`
    /// (folded into `configuration.env_variables` on the wire); discovery matches
    /// the marker inside the round-tripped configuration, ignoring the name
    /// entirely. For kinds whose server clobbers the submitted name — job_run:
    /// both CPDaaS and CP4D store every run as `"Notebook Job"` (live-pinned
    /// 2026-07-05), while `entity.job_run.configuration` round-trips verbatim.
    EnvMarker,
    /// Server mints its own id; completed runs can't be listed — idempotency rides
    /// `recover_from_create_error` already-exists adoption.
    ServerSide,
    /// Non-discoverable API (no name/id/tag carrier at all — e.g. `sal_*`): the hash
    /// is persisted in a local, env-scoped record file in the run-records dir
    /// (`runs_root()/local-hashes.json`) by the kind's handler after a successful run;
    /// the reconciler's Skip-discovery arm consults it (recorded → NoChange, else
    /// Create). The documented Q2 local-hash idempotency exception, scoped strictly
    /// to kinds that declare it; fresh machine / cleared record ⇒ one re-run, then
    /// idempotent.
    Local,
}

/// Declares that a kind's identity is a deterministic hash over its real input
/// fields plus an optional nonce, rather than its `name`. When present, the
/// generic reconciler machinery drives identity by hash: `name` and the hashed
/// `fields` drop out of the default `state_fields` and a synthetic `identity_hash`
/// state field is injected.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityHash {
    /// The real input fields folded into the hash (dot notation not required — the
    /// hash reads each as a top-level field).
    pub fields: Vec<String>,
    /// Optional nonce field (`generation`) folded into the hash; `location: LocalOnly`
    /// on the kind's schema so it is never sent to the API body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nonce_field: Option<String>,
    /// Where the hash is stored on the remote object.
    #[serde(default)]
    pub storage: HashStorage,
    /// Number of hex chars of the truncated BLAKE3 hash (default 8 = 32 bits).
    #[serde(default = "default_hash_length")]
    pub length: usize,
}

fn default_hash_length() -> usize {
    8
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconciliationDefinition {
    pub discovery: DiscoveryDefinition,
    /// Opt-in identity-by-hash for job-style kinds. Absent for every kind that
    /// keeps name/id identity (the common case) — `skip_serializing_if` keeps the
    /// serialized schema byte-identical, so no golden/snapshot drift.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_hash: Option<IdentityHash>,
    #[serde(default)]
    pub state_fields: Option<Vec<String>>,
    pub update_strategy: UpdateStrategy,
    #[serde(default)]
    pub immutable_fields: Vec<String>,
    /// When true, an immutable-field drift during reconciliation produces a
    /// hard error instead of proposing Recreate. Use for kinds where silent
    /// destroy+create would clobber externally-created state (e.g. watsonx.data
    /// registrations whose server-side delete cascades to the associated catalog).
    #[serde(default)]
    pub reject_on_immutable_drift: bool,
    /// Whether to use JSON Patch (RFC 6902) format for PATCH requests
    /// Defaults to true for backward compatibility with CP4D/watsonx.data
    #[serde(default = "default_use_json_patch")]
    pub use_json_patch: bool,
    /// Path prefix for JSON Patch operations (must be explicitly set when use_json_patch is true)
    /// Use "/entity" for CP4D compatibility
    /// Use "" for standard RFC 6902 paths (e.g., Watsonx Data /v2/connections API)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub json_patch_path_prefix: Option<String>,
}

fn default_use_json_patch() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct DiscoveryDefinition {
    pub method: DiscoveryMethod,
    #[serde(default)]
    pub list_field: Option<String>,
    #[serde(default)]
    pub id_source: String,
    /// Field name used to match local resource against items in the list response.
    /// Defaults to `name` for backwards compatibility. Set to e.g. `display_name`
    /// when the API uses a different identifier (watsonx.data engines).
    /// Ignored when `identity_match` is declared.
    #[serde(default)]
    pub name_field: Option<String>,
    /// Primary identity match using separate local / remote dot paths. When
    /// declared, this takes precedence over `name_field` — use it when the
    /// stable identity lives at different paths on the local YAML vs. the
    /// remote API response (e.g. singular `associated_catalog.catalog_name`
    /// locally vs. plural `associated_catalogs[0].catalog_name` remotely).
    #[serde(default)]
    pub identity_match: Option<IdentityMatch>,
    /// A sentinel field/value pair that means "not actually present" even though
    /// discovery found a body. Applies to two discovery shapes:
    /// - `Singleton`: a 200 body is normally treated as "the instance exists".
    ///   Some singletons instead return 200 with a sentinel body when absent,
    ///   e.g. SAL's `GET /v3/sal_integration` returns `{"status":"missing"}`
    ///   until it is enabled. Declare `absent_when: {field: status, equals:
    ///   missing}` so such a body is treated as absent (plan Create / "enable")
    ///   rather than Update.
    /// - `ListAndGet`: the identity-matched list item is normally treated as
    ///   "the resource exists". Some APIs leave the identity record in place
    ///   after delete/undeploy with a state field nulled out instead of removing
    ///   the record, e.g. watsonx Orchestrate's undeploy leaves the agent's
    ///   `Environment` record with `current_version: null`. Declare
    ///   `absent_when: {field: current_version, equals: null}` so a matched item
    ///   with that sentinel is treated as absent (plan Create) rather than
    ///   Update/NoChange. A missing field is treated the same as an explicit
    ///   `null` field, so `equals: null` also matches records that omit the
    ///   field entirely.
    #[serde(default)]
    pub absent_when: Option<AbsentWhen>,
    /// For `ListAndGet` only: HTTP method used for the list call. Defaults to GET.
    /// Set to `post` for APIs whose only enumeration endpoint is a search POST
    /// (e.g. CAMS `POST /v2/asset_types/<type>/search`), which has no GET list.
    /// The scoping query params are still applied; `list_body` is the POST body.
    #[serde(default)]
    pub list_method: Option<String>,
    /// For `ListAndGet` with `list_method: post`: the JSON request body sent to the
    /// search endpoint (e.g. `{query: "asset.asset_type:data_asset"}`). Defaults to
    /// `{}` when omitted.
    #[serde(default)]
    pub list_body: Option<serde_json::Value>,
    /// For `ListAndGet` only: the list endpoint returns a JSON *object map* keyed by
    /// the resource identity rather than an array of items. Discovery reads the
    /// top-level `data` object and treats each of its keys (with a trailing `/`
    /// stripped) as a bare-string item to match against. Use for HashiCorp Vault's
    /// `sys/mounts` / `sys/auth` / `sys/audit`, which return
    /// `{"data": {"<path>/": {...config...}}}` and have no per-path GET usable for
    /// discovery (absent → 400 for mounts/auth; audit has no GET at all → 405).
    #[serde(default)]
    pub list_map: bool,
    /// For `ListAndGet` only: an opt-in type predicate that filters a
    /// heterogeneous list response to only this kind's items. See [`ListFilter`].
    /// `skip_serializing_if` keeps `null` out of any schema serialization for the
    /// ~100 kinds that do not declare it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub list_filter: Option<ListFilter>,
}

/// Sentinel that marks a discovered body (`Singleton` 200 response, or a
/// `ListAndGet` identity-matched item) as "absent". See
/// `DiscoveryDefinition::absent_when`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AbsentWhen {
    /// Dot path into the discovered body (top-level field name in the common
    /// case, e.g. `status`, `current_version`).
    pub field: String,
    /// Value at `field` that means the resource is absent (e.g. the string
    /// `missing`, or `null`). A field missing from the body is treated as
    /// `null` for this comparison, so `equals: null` matches both an explicit
    /// null and an omitted field.
    pub equals: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityMatch {
    /// Dot path into the local resource's data (supports numeric segments as
    /// array indices, e.g. `foo.0.bar`).
    pub local_path: String,
    /// Dot path into each remote list item (same segment rules as `local_path`).
    pub remote_path: String,
}

/// An opt-in discovery predicate that filters a heterogeneous list response to
/// only the items belonging to this kind. Used where a kind's `list_endpoint`
/// returns a mixed collection (e.g. Planning Analytics `Assets(...)?$expand=Assets`
/// returns folders, dashboards, and every other type together), so a plain
/// name match could adopt an item of a different type. When declared, discovery
/// keeps only items whose `field` (dot path into each list item) equals `equals`
/// in addition to the name match. Both keys are required: serde rejects a
/// `list_filter` block missing either (a schema parse error at build time).
/// Declares no `references`, so it adds no dependency-graph edges.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ListFilter {
    /// Dot path into each remote list item (same segment rules as `IdentityMatch`).
    pub field: String,
    /// The string value at `field` that marks an item as belonging to this kind.
    pub equals: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoveryMethod {
    ListAndGet,
    GetById,
    Skip,
    /// Per-instance singleton: get/create/delete share one id-less endpoint
    /// (e.g. `/sal_integrations`). Discovery GETs `get_endpoint`; a non-empty
    /// 200 is the one existing instance, an empty body or 404 means absent.
    Singleton,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateStrategy {
    Patch,
    Replace,
    Recreate,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct HookDefinition {
    #[serde(default)]
    pub pre_create: Option<String>,
    #[serde(default)]
    pub post_create: Option<String>,
    #[serde(default)]
    pub pre_update: Option<String>,
    #[serde(default)]
    pub post_update: Option<String>,
    #[serde(default)]
    pub pre_delete: Option<String>,
    #[serde(default)]
    pub post_delete: Option<String>,
}

impl ResourceDefinition {
    /// Sensitive dotted paths for LOGGED API bodies, request AND response: the
    /// field-derived `schema.sensitive_paths()` superset plus CAMS response-envelope
    /// variants of each path — `entity.<name>.<path>` (create/GET response envelope)
    /// and `<list_field>.entity.<name>.<path>` (LIST envelope; `redact_by_schema`
    /// skips array indices, so one dotted path matches every list item). Responses
    /// echo submitted sensitive fields back inside these envelopes (live-pinned
    /// 2026-07-05: a job_run runs-LIST response carried a plaintext env-variable
    /// apikey into the `WXCTL_LOG_PATH` sink — `results.entity.job_run.configuration.env_variables`,
    /// unreachable from the bare field paths). Variants that don't occur in a given
    /// body never match — the superset redacts harmlessly.
    pub fn sensitive_paths(&self) -> Vec<String> {
        let base = self.schema.sensitive_paths();
        let list_field = self.reconciliation.discovery.list_field.as_deref().unwrap_or("results");
        let mut out = base.clone();
        for path in &base {
            let enveloped = format!("entity.{}.{path}", self.name);
            out.push(format!("{list_field}.{enveloped}"));
            out.push(enveloped);
        }
        out
    }
}

impl SchemaDefinition {
    /// Variant groups in sorted-key order — `variants` is a HashMap, so iterating
    /// `values()` directly is nondeterministic; every merged-field surface below
    /// goes through this so field order is stable across runs.
    fn sorted_variants(&self) -> Vec<&VariantDefinition> {
        let Some(variants) = &self.variants else { return Vec::new() };
        let mut keys: Vec<&String> = variants.keys().collect();
        keys.sort_unstable();
        keys.into_iter().map(|k| &variants[k]).collect()
    }

    /// Iterator over common fields + all variant fields (deduped by name).
    /// Used by helpers that need the full field surface regardless of active variant.
    pub fn all_fields(&self) -> Vec<&FieldDefinition> {
        let mut out: Vec<&FieldDefinition> = self.fields.iter().collect();
        let mut seen: std::collections::HashSet<&str> = self.fields.iter().map(|f| f.name.as_str()).collect();
        for variant in self.sorted_variants() {
            for field in &variant.fields {
                if seen.insert(field.name.as_str()) {
                    out.push(field);
                }
            }
        }
        out
    }

    /// Return the fields active under a particular discriminator value, merged with
    /// common fields. Used by variant-aware validation.
    pub fn fields_for_variant(&self, discriminator_value: &str) -> Vec<&FieldDefinition> {
        let mut out: Vec<&FieldDefinition> = self.fields.iter().collect();
        for variant in self.sorted_variants() {
            if variant.applies_to.iter().any(|v| v == discriminator_value) {
                for field in &variant.fields {
                    out.push(field);
                }
            }
        }
        out
    }

    /// Compute state fields by including all fields except Computed and LocalOnly.
    /// Used as a default when `state_fields` is not explicitly set in the schema YAML.
    /// Includes fields from all variants so that discriminator-driven schemas produce
    /// a complete drift-detection surface.
    pub fn compute_state_fields(&self) -> Vec<String> {
        self.all_fields().into_iter().filter(|f| f.location != FieldLocation::Computed && f.location != FieldLocation::LocalOnly).map(|f| f.name.clone()).collect()
    }

    /// Build mapping of referenced resource kinds to API field names
    ///
    /// Scopes: Top-level fields + variant fields. Nested schemas are not traversed.
    ///
    /// A kind referenced by more than one field has no unambiguous alias and is
    /// omitted entirely (previously last-writer-wins mapped e.g. `s3_bucket:` on
    /// `s3_object` to whichever of `bucket`/`region` came last) — users of such
    /// kinds must write the real field name.
    ///
    /// Returns HashMap<resource_kind, api_field_name>
    /// Example: {"orchestrate_connection": "connection_id"}
    pub fn build_field_mapping(&self) -> HashMap<String, String> {
        let mut mapping: HashMap<String, String> = HashMap::new();
        let mut ambiguous: std::collections::HashSet<String> = std::collections::HashSet::new();

        for field in self.all_fields() {
            if let Some(refs) = &field.references {
                for kind in std::iter::once(&refs.resource).chain(refs.also_allows.iter()) {
                    match mapping.get(kind) {
                        Some(existing) if existing != &field.name => {
                            ambiguous.insert(kind.clone());
                        }
                        Some(_) => {}
                        None => {
                            mapping.insert(kind.clone(), field.name.clone());
                        }
                    }
                }
            }
        }

        for kind in &ambiguous {
            mapping.remove(kind);
        }
        mapping
    }

    /// Collect dotted field paths marked `sensitive: true` in this schema
    /// (recursively traversing nested object schemas and variant field groups).
    /// Used by the plan renderer and log emitter to mask values at output time.
    ///
    /// Emits BOTH the wxctl field-name path and the `api_field`-based path for
    /// each sensitive field: request bodies are keyed by `api_field` while local
    /// data and diffs use the field name (the pa_user `password`/`Password` split
    /// documents the trap). The superset is intentional — every sink redacts
    /// whichever spelling it sees.
    pub fn sensitive_paths(&self) -> Vec<String> {
        let root = [String::new()];
        let mut paths = Vec::new();
        collect_sensitive_paths(&self.fields, &root, &mut paths);
        for variant in self.sorted_variants() {
            collect_sensitive_paths(&variant.fields, &root, &mut paths);
        }
        paths
    }
}

fn collect_sensitive_paths(fields: &[FieldDefinition], prefixes: &[String], out: &mut Vec<String>) {
    for field in fields {
        // Both spellings of this field: the wxctl name and (when it differs) the
        // api_field the request body actually carries. `api_field` may itself be a
        // dotted path (e.g. `additional_properties.icon`) — emitted verbatim.
        let mut names: Vec<&str> = vec![field.name.as_str()];
        if let Some(api) = field.api_field.as_deref()
            && api != field.name
        {
            names.push(api);
        }
        let mut paths: Vec<String> = Vec::new();
        for prefix in prefixes {
            for name in &names {
                let path = if prefix.is_empty() { (*name).to_string() } else { format!("{prefix}.{name}") };
                if !paths.contains(&path) {
                    paths.push(path);
                }
            }
        }
        if field.sensitive {
            for path in &paths {
                if !out.contains(path) {
                    out.push(path.clone());
                }
            }
        }
        if let Some(inner) = &field.schema {
            collect_sensitive_paths(&inner.fields, &paths, out);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_field(name: &str, location: FieldLocation) -> FieldDefinition {
        FieldDefinition {
            name: name.to_string(),
            field_type: FieldType::String,
            required: false,
            immutable: false,
            location,
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
            properties: None,
            is_path: false,
            synthesize: None,
            synth_shape: None,
        }
    }

    fn make_field_with_ref(name: &str, resource: &str, field: &str) -> FieldDefinition {
        let mut f = make_field(name, FieldLocation::Body);
        f.references = Some(FieldReferences { resource: resource.to_string(), field: field.to_string(), also_allows: vec![], optional: false, require_ready: false, relationship: None });
        f
    }

    #[test]
    fn test_compute_state_fields_excludes_computed_local() {
        let schema =
            SchemaDefinition { fields: vec![make_field("name", FieldLocation::Body), make_field("version", FieldLocation::Query), make_field("status", FieldLocation::Computed), make_field("source_path", FieldLocation::LocalOnly), make_field("id", FieldLocation::Path)], ..Default::default() };

        let state_fields = schema.compute_state_fields();

        assert!(state_fields.contains(&"name".to_string()));
        assert!(state_fields.contains(&"version".to_string()));
        assert!(state_fields.contains(&"id".to_string()));
        assert!(!state_fields.contains(&"status".to_string()));
        assert!(!state_fields.contains(&"source_path".to_string()));
        assert_eq!(state_fields.len(), 3);
    }

    #[test]
    fn test_build_field_mapping() {
        let schema = SchemaDefinition { fields: vec![make_field_with_ref("connection_id", "orchestrate_connection", "asset_id"), make_field("name", FieldLocation::Body)], ..Default::default() };

        let mapping = schema.build_field_mapping();

        assert_eq!(mapping.get("orchestrate_connection").unwrap(), "connection_id");
        assert_eq!(mapping.len(), 1);
    }

    #[test]
    fn test_build_field_mapping_drops_ambiguous_kind_alias() {
        // s3_object shape: `bucket` and `region` both reference s3_bucket — the
        // `s3_bucket:` alias is ambiguous and must be absent (not last-writer-wins).
        let schema = SchemaDefinition { fields: vec![make_field_with_ref("bucket", "s3_bucket", "name"), make_field_with_ref("region", "s3_bucket", "region"), make_field_with_ref("conn", "connection", "id")], ..Default::default() };
        let mapping = schema.build_field_mapping();
        assert!(!mapping.contains_key("s3_bucket"), "ambiguous alias must be removed, got {mapping:?}");
        assert_eq!(mapping.get("connection").unwrap(), "conn", "unambiguous aliases stay");
        assert_eq!(mapping.len(), 1);
    }

    #[test]
    fn test_readiness_block_deserializes_with_defaults() {
        // Minimal ApiDefinition carrying a readiness block with only the two
        // required keys; failed/timeout_env/timeout_default/interval_secs default.
        let yaml = r#"
base_path: /v2/data_marts
id_field: metadata.id
get_endpoint: /v2/data_marts/{id}
create_method: POST
delete_method: DELETE
readiness:
  state_path: entity.status.state
  ready: [active]
"#;
        let api: ApiDefinition = serde_norway::from_str(yaml).expect("ApiDefinition with readiness parses");
        let r = api.readiness.expect("readiness present");
        assert_eq!(r.state_path, "entity.status.state");
        assert_eq!(r.ready, vec!["active".to_string()]);
        assert!(r.failed.is_empty(), "failed defaults empty");
        assert_eq!(r.timeout_env, None);
        assert_eq!(r.timeout_default, 300, "timeout_default serde-default is 300");
        assert_eq!(r.interval_secs, 5, "interval_secs serde-default is 5");
    }

    #[test]
    fn test_readiness_block_full() {
        let yaml = r#"
base_path: /v2/data_marts
id_field: metadata.id
get_endpoint: /v2/data_marts/{id}
create_method: POST
delete_method: DELETE
readiness:
  state_path: entity.status.state
  ready: [active]
  failed: [error, disabled]
  timeout_env: WXCTL_DATA_MART_READY_TIMEOUT
  timeout_default: 600
  interval_secs: 10
"#;
        let api: ApiDefinition = serde_norway::from_str(yaml).unwrap();
        let r = api.readiness.unwrap();
        assert_eq!(r.failed, vec!["error".to_string(), "disabled".to_string()]);
        assert_eq!(r.timeout_env.as_deref(), Some("WXCTL_DATA_MART_READY_TIMEOUT"));
        assert_eq!(r.timeout_default, 600);
        assert_eq!(r.interval_secs, 10);
    }

    #[test]
    fn test_api_definition_without_readiness_is_none() {
        let yaml = r#"
base_path: /v2/x
id_field: id
get_endpoint: /v2/x/{id}
create_method: POST
delete_method: DELETE
"#;
        let api: ApiDefinition = serde_norway::from_str(yaml).unwrap();
        assert!(api.readiness.is_none(), "readiness defaults to None when absent");
    }

    #[test]
    fn test_require_ready_deserializes_and_defaults_false() {
        // Explicit true.
        let with = r#"
resource: data_mart
field: id
require_ready: true
"#;
        let r: FieldReferences = serde_norway::from_str(with).unwrap();
        assert!(r.require_ready);

        // Absent -> false (spec AC 6 default).
        let without = r#"
resource: data_mart
field: id
"#;
        let r: FieldReferences = serde_norway::from_str(without).unwrap();
        assert!(!r.require_ready, "require_ready defaults to false when absent");
    }

    #[test]
    fn test_sensitive_defaults_false_and_deserialises() {
        let yaml = r#"
name: password
type: string
sensitive: true
"#;
        let field: FieldDefinition = serde_norway::from_str(yaml).unwrap();
        assert!(field.sensitive);

        let yaml_no_flag = "name: username\ntype: string";
        let field_default: FieldDefinition = serde_norway::from_str(yaml_no_flag).unwrap();
        assert!(!field_default.sensitive);
    }

    #[test]
    fn discovery_method_singleton_parses_from_snake_case() {
        let d: DiscoveryDefinition = serde_norway::from_str("method: singleton").unwrap();
        assert!(matches!(d.method, DiscoveryMethod::Singleton));
    }

    #[test]
    fn test_soft_allowed_values_deserialises() {
        let yaml = r#"
name: kind
type: string
validation:
  soft_allowed_values: [a, b, c]
"#;
        let field: FieldDefinition = serde_norway::from_str(yaml).unwrap();
        let soft = field.validation.unwrap().soft_allowed_values.unwrap();
        assert_eq!(soft, vec!["a", "b", "c"]);
    }

    #[test]
    fn test_sensitive_paths_walks_nested_schemas() {
        let mut pwd = make_field("password", FieldLocation::Body);
        pwd.sensitive = true;

        let mut conn = make_field("connection", FieldLocation::Body);
        conn.field_type = FieldType::Object;
        conn.schema = Some(Box::new(SchemaDefinition { fields: vec![make_field("host", FieldLocation::Body), pwd], ..Default::default() }));

        let mut top_secret = make_field("api_key", FieldLocation::Body);
        top_secret.sensitive = true;

        let schema = SchemaDefinition { fields: vec![make_field("name", FieldLocation::Body), top_secret, conn], ..Default::default() };
        let paths = schema.sensitive_paths();
        assert!(paths.contains(&"api_key".to_string()));
        assert!(paths.contains(&"connection.password".to_string()));
        assert_eq!(paths.len(), 2);
    }

    #[test]
    fn test_sensitive_paths_emits_api_field_spelling_too() {
        // pa_user shape: `password` with `api_field: Password` — the request body is
        // keyed by the api_field, so both spellings must be emitted.
        let mut pwd = make_field("password", FieldLocation::Body);
        pwd.sensitive = true;
        pwd.api_field = Some("Password".to_string());

        let mut nested_secret = make_field("secret", FieldLocation::Body);
        nested_secret.sensitive = true;
        let mut creds = make_field("credentials", FieldLocation::Body);
        creds.field_type = FieldType::Object;
        creds.api_field = Some("Credentials".to_string());
        creds.schema = Some(Box::new(SchemaDefinition { fields: vec![nested_secret], ..Default::default() }));

        let schema = SchemaDefinition { fields: vec![pwd, creds], ..Default::default() };
        let paths = schema.sensitive_paths();
        for expected in ["password", "Password", "credentials.secret", "Credentials.secret"] {
            assert!(paths.contains(&expected.to_string()), "missing '{expected}' in {paths:?}");
        }
    }

    #[test]
    fn deny_unknown_fields_rejects_typod_field_attribute() {
        // `loation` for `location` must fail to parse, not be silently dropped —
        // the bug slim-F exists to prevent.
        let res: Result<FieldDefinition, _> = serde_norway::from_str("name: foo\ntype: string\nloation: Body\n");
        assert!(res.is_err(), "typo'd field attribute must be rejected by deny_unknown_fields");
    }
}
