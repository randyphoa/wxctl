use crate::deployment::DeploymentConstraint;
use anyhow::{Result, anyhow};
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
    #[serde(default)]
    pub properties: Option<HashMap<String, FieldDefinition>>,

    /// When true, the field holds a local filesystem path that `resolve_file_paths`
    /// resolves against the config dir. The build-time guard enforces `LocalOnly`.
    #[serde(default)]
    pub is_path: bool,
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
    /// When true, the reference is advisory — users may supply either a
    /// `${kind.name}` template (which must resolve inside the current
    /// apply) or a hard-coded literal pointing at externally-managed
    /// infrastructure. Missing referents emit `WXCTL-V502` at warn level
    /// instead of the `WXCTL-V005` hard error. Default `false` preserves
    /// the existing strict behavior for all pre-existing schemas. Already
    /// recognised at build time in `wxctl-providers/build.rs`.
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ReconciliationDefinition {
    pub discovery: DiscoveryDefinition,
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
    /// For `Singleton` discovery only: a 200 body is normally treated as "the
    /// instance exists". Some singletons instead return 200 with a sentinel body
    /// when absent — e.g. SAL's `GET /v3/sal_integration` returns
    /// `{"status":"missing"}` until it is enabled. Declare
    /// `absent_when: {field: status, equals: missing}` so such a body is treated
    /// as absent (plan Create / "enable") rather than Update.
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
}

/// Sentinel that marks a `Singleton` 200 response as "absent". See
/// `DiscoveryDefinition::absent_when`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AbsentWhen {
    /// Dot path into the singleton GET response body (top-level field name in
    /// the common case, e.g. `status`).
    pub field: String,
    /// String value at `field` that means the singleton is absent (e.g. `missing`).
    pub equals: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdentityMatch {
    /// Dot path into the local resource's data (supports numeric segments as
    /// array indices, e.g. `foo.0.bar`).
    pub local_path: String,
    /// Dot path into each remote list item (same segment rules as `local_path`).
    pub remote_path: String,
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

impl ResourceSchema {
    /// Validate schema consistency and alias rules
    pub fn validate(&self) -> Result<()> {
        validate_aliases(&self.resource.schema)?;
        crate::validation::schema::validate_reconciliation_patch_prefix(&self.resource.reconciliation, &self.resource.kind).map_err(|e| anyhow!("Schema validation failed: {}", e))?;
        Ok(())
    }
}

/// Validate schema consistency
/// Currently validates that references are properly structured.
fn validate_aliases(schema: &SchemaDefinition) -> Result<()> {
    // Validate that fields with references have the expected structure
    for field in &schema.fields {
        if let Some(refs) = &field.references {
            if refs.resource.is_empty() {
                return Err(anyhow!("Schema validation failed: Field '{}' has references with empty resource.", field.name));
            }
            if refs.field.is_empty() {
                return Err(anyhow!("Schema validation failed: Field '{}' has references with empty field.", field.name));
            }
        }
    }
    Ok(())
}

impl SchemaDefinition {
    /// Iterator over common fields + all variant fields (deduped by name).
    /// Used by helpers that need the full field surface regardless of active variant.
    pub fn all_fields(&self) -> Vec<&FieldDefinition> {
        let Some(variants) = &self.variants else {
            return self.fields.iter().collect();
        };
        let mut out: Vec<&FieldDefinition> = self.fields.iter().collect();
        let mut seen: std::collections::HashSet<&str> = self.fields.iter().map(|f| f.name.as_str()).collect();
        for variant in variants.values() {
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
        if let Some(variants) = &self.variants {
            for variant in variants.values() {
                if variant.applies_to.iter().any(|v| v == discriminator_value) {
                    for field in &variant.fields {
                        out.push(field);
                    }
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
    /// Returns HashMap<resource_kind, api_field_name>
    /// Example: {"orchestrate_connection": "connection_id"}
    pub fn build_field_mapping(&self) -> HashMap<String, String> {
        let mut mapping = HashMap::new();

        for field in self.all_fields() {
            if let Some(refs) = &field.references {
                mapping.insert(refs.resource.clone(), field.name.clone());
                for also in &refs.also_allows {
                    mapping.insert(also.clone(), field.name.clone());
                }
            }
        }

        mapping
    }

    /// Collect dotted field paths marked `sensitive: true` in this schema
    /// (recursively traversing nested object schemas and variant field groups).
    /// Used by the plan renderer and log emitter to mask values at output time.
    pub fn sensitive_paths(&self) -> Vec<String> {
        let mut paths = Vec::new();
        collect_sensitive_paths(&self.fields, "", &mut paths);
        if let Some(variants) = &self.variants {
            for variant in variants.values() {
                collect_sensitive_paths(&variant.fields, "", &mut paths);
            }
        }
        paths
    }
}

fn collect_sensitive_paths(fields: &[FieldDefinition], prefix: &str, out: &mut Vec<String>) {
    for field in fields {
        let path = if prefix.is_empty() { field.name.clone() } else { format!("{prefix}.{}", field.name) };
        if field.sensitive {
            out.push(path.clone());
        }
        if let Some(inner) = &field.schema {
            collect_sensitive_paths(&inner.fields, &path, out);
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
        }
    }

    fn make_field_with_ref(name: &str, resource: &str, field: &str) -> FieldDefinition {
        let mut f = make_field(name, FieldLocation::Body);
        f.references = Some(FieldReferences { resource: resource.to_string(), field: field.to_string(), also_allows: vec![], optional: false });
        f
    }

    fn make_test_schema(fields: Vec<FieldDefinition>) -> ResourceSchema {
        ResourceSchema {
            resource: ResourceDefinition {
                name: "test".to_string(),
                service: "test".to_string(),
                kind: "test".to_string(),
                version: "v1".to_string(),
                api: ApiDefinition {
                    base_path: "/test".to_string(),
                    id_field: "id".to_string(),
                    list_endpoint: None,
                    get_endpoint: "/test/{id}".to_string(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: None,
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                },
                schema: SchemaDefinition { fields, ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, id_source: "id".to_string() },
                    state_fields: None,
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec![],
                    reject_on_immutable_drift: false,
                    use_json_patch: true,
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
    fn test_validate_rejects_empty_reference_parts() {
        // Each row: (reference resource, reference field, expected error substring).
        // A reference must name both a non-empty resource and a non-empty field.
        let cases: &[(&str, &str, &str)] = &[
            ("", "asset_id", "empty resource"),            // empty resource
            ("orchestrate_connection", "", "empty field"), // empty field
        ];
        for (res, field, needle) in cases {
            let schema = make_test_schema(vec![make_field_with_ref("conn_id", res, field)]);
            let err = schema.validate().unwrap_err();
            assert!(err.to_string().contains(needle), "ref({res:?},{field:?}) should error '{needle}', got: {err}");
        }
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
    fn deny_unknown_fields_rejects_typod_field_attribute() {
        // `loation` for `location` must fail to parse, not be silently dropped —
        // the bug slim-F exists to prevent.
        let res: Result<FieldDefinition, _> = serde_norway::from_str("name: foo\ntype: string\nloation: Body\n");
        assert!(res.is_err(), "typo'd field attribute must be rejected by deny_unknown_fields");
    }
}
