//! Structured, wasm-safe projections over the compiled schema set:
//! - `explain_kind(kind)` → the `ExplainView` the CLI serializes for `explain -o json`.
//! - `list_kinds(filter)` → `KindSummary` rows the CLI renders for `resources`.
//!
//! The CLI's table renderers (color/Theme) stay in `wxctl`; this module owns the
//! data model so the CLI and the wasm core emit byte-identical JSON.

use crate::dependency_graph::{deployment_support, get_edges, get_resource_by_index, resource_catalog, resource_prompt_notes};
use crate::descriptor::ResourceDescriptor;
use crate::ir::{FieldIr, FieldLocationIr, FieldTypeIr, HttpMethodIr, RESOURCE_IR, ValidationIr};
use anyhow::Result;
use serde::Serialize;

/// Serializable detail view for one resource kind.
#[derive(Serialize)]
pub struct ExplainView {
    pub kind: String,
    pub service: String,
    pub id_field: String,
    pub authoring: Authoring,
    pub endpoints: ExplainEndpoints,
    pub fields: Vec<ExplainField>,
    pub dependencies: Vec<ExplainDependency>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub consumers: Vec<ExplainConsumer>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub advisories: Vec<ExplainAdvisory>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<ExplainVariant>,
    pub prompt_notes: Vec<String>,
}

/// How a kind's fields assemble into a config document. The fields above are a
/// per-kind schema; this block carries the cross-kind conventions an agent
/// otherwise can't derive — the envelope shape, the `ref_name` handle (a meta
/// key, never a schema field), and the `${...}` reference grammar.
#[derive(Serialize)]
pub struct Authoring {
    pub envelope: &'static str,
    pub ref_name: &'static str,
    pub reference_syntax: &'static str,
}

impl Authoring {
    fn new() -> Self {
        Authoring {
            envelope: "A config is one or more YAML documents separated by `---`. Each has top-level `kind` and `ref_name`, then the fields below at the top level (not nested under `spec`).",
            ref_name: "Unique handle for this resource within the config. Used to reference it from other resources, then stripped before the API call (not a schema field).",
            reference_syntax: "Reference another resource by its ref_name: `${<kind>.<ref_name>}` resolves to its id, `${<kind>.<ref_name>.<field>}` to a specific field. Values resolve late, at plan/apply time.",
        }
    }
}

/// The cross-kind authoring conventions (envelope / `ref_name` / reference
/// syntax), independent of any single kind — the payload `wxctl explain` renders
/// when called with no kind argument.
pub fn authoring_overview() -> Authoring {
    Authoring::new()
}

#[derive(Serialize)]
pub struct ExplainEndpoints {
    pub base_path: String,
    pub list: Option<String>,
    pub get: String,
    pub create: String,
    pub update: Option<String>,
    pub update_method: Option<String>,
    pub delete: String,
}

#[derive(Serialize)]
pub struct ExplainField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    /// Element type for `array` fields (e.g. `array<string>`); absent otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_type: Option<String>,
    pub required: bool,
    pub immutable: bool,
    pub location: String,
    /// Value is a local filesystem path resolved against the config-file dir.
    #[serde(skip_serializing_if = "is_false")]
    pub is_path: bool,
    /// Value is redacted in plan diffs / logs (credentials, keys).
    #[serde(skip_serializing_if = "is_false")]
    pub sensitive: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_json::Value>,
    /// Closed enum — only these values are accepted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed_values: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<ExplainValidation>,
    /// Literal value to author for a reference field — e.g. `${storage_connection.<ref_name>}`.
    /// Present iff the field points at another resource kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reference: Option<String>,
    /// Sub-fields of an `object` (or array-of-object) field — the nested shape an
    /// agent must author. Absent for scalars and untyped containers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<ExplainField>>,
}

/// Project a schema's field list onto the explain view, recursing into nested
/// `object`/array-of-object sub-fields so the full authorable shape is exposed.
fn build_fields(defs: &[FieldIr]) -> Vec<ExplainField> {
    defs.iter()
        .map(|f| ExplainField {
            name: f.name.to_string(),
            field_type: field_type_str(&f.field_type).to_string(),
            item_type: f.item_type.as_ref().map(|t| field_type_str(t).to_string()),
            required: f.required,
            immutable: f.immutable,
            location: location_str(&f.location).to_string(),
            is_path: f.is_path,
            sensitive: f.sensitive,
            default: f.default.map(|s| serde_json::from_str::<serde_json::Value>(s).expect("canonical json default")),
            allowed_values: f.allowed_values.map(|v| v.iter().map(|s| s.to_string()).collect()),
            description: f.description.map(str::to_string),
            validation: f.validation.as_ref().and_then(ExplainValidation::from_rules),
            reference: f.references.as_ref().map(|r| format!("${{{}.<ref_name>}}", r.resource)),
            fields: f.schema.map(|s| build_fields(s.fields)).filter(|v| !v.is_empty()),
        })
        .collect()
}

/// Field constraints, emitted only when at least one is set (each member is
/// `skip`ped when absent, and [`ExplainValidation::from_rules`] returns `None`
/// for an all-empty rule set so the whole block disappears).
#[derive(Serialize)]
pub struct ExplainValidation {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_length_bytes: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_value: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    /// Soft allowlist: values outside this list warn (`WXCTL-V401`) but pass.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub soft_allowed_values: Option<Vec<String>>,
    /// Mutual-exclusivity groups — exactly one field per inner list may be set.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub one_of: Option<Vec<Vec<String>>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_rules: Option<Vec<String>>,
}

impl ExplainValidation {
    /// Project the compiled [`ValidationIr`] onto the explain view, returning
    /// `None` when no constraint is set so the field omits `validation` entirely.
    pub fn from_rules(v: &ValidationIr) -> Option<Self> {
        let ev = ExplainValidation {
            pattern: v.pattern.map(str::to_string),
            min_length: v.min_length,
            max_length: v.max_length,
            max_length_bytes: v.max_length_bytes,
            min_value: v.min_value,
            max_value: v.max_value,
            max_items: v.max_items,
            soft_allowed_values: v.soft_allowed_values.map(|vals| vals.iter().map(|s| s.to_string()).collect()),
            one_of: v.one_of.map(|groups| groups.iter().map(|g| g.iter().map(|s| s.to_string()).collect()).collect()),
            extra_rules: v.extra_rules.map(|rules| rules.iter().map(|s| s.to_string()).collect()),
        };
        let empty = ev.pattern.is_none() && ev.min_length.is_none() && ev.max_length.is_none() && ev.max_length_bytes.is_none() && ev.min_value.is_none() && ev.max_value.is_none() && ev.max_items.is_none() && ev.soft_allowed_values.is_none() && ev.one_of.is_none() && ev.extra_rules.is_none();
        (!empty).then_some(ev)
    }
}

/// `skip_serializing_if` predicate for boolean flags that default to `false`.
fn is_false(b: &bool) -> bool {
    !*b
}

#[derive(Serialize)]
pub struct ExplainDependency {
    pub field: String,
    pub target_kind: String,
    pub required: bool,
}

/// A kind that consumes this one via a `${<this kind>.<ref_name>}` reference —
/// the reverse of [`ExplainDependency`]. Sourced from the compile-time edge
/// table so it stays in sync with the forward `dependencies` view.
#[derive(Serialize)]
pub struct ExplainConsumer {
    pub kind: String,
    pub field: String,
    pub required: bool,
}

/// A published advisory for this kind — a known limitation, gotcha, or
/// deprecation notice an agent should see before authoring against it.
/// Sourced from the schema's top-level `advisories:` block.
#[derive(Serialize)]
pub struct ExplainAdvisory {
    pub severity: String,
    pub tier: String,
    pub date: String,
    pub text: String,
}

/// One discriminator-scoped field group from a variant schema (e.g. one
/// datasource type's fields on a connection kind).
#[derive(Serialize)]
pub struct ExplainVariant {
    pub name: String,
    pub applies_to: Vec<String>,
    pub fields: Vec<ExplainField>,
}

fn location_str(loc: &FieldLocationIr) -> &'static str {
    match loc {
        FieldLocationIr::Body => "Body",
        FieldLocationIr::Query => "Query",
        FieldLocationIr::Header => "Header",
        FieldLocationIr::Path => "Path",
        FieldLocationIr::Computed => "Computed",
        FieldLocationIr::LocalOnly => "LocalOnly",
    }
}

fn field_type_str(t: &FieldTypeIr) -> &'static str {
    match t {
        FieldTypeIr::String => "string",
        FieldTypeIr::Integer => "integer",
        FieldTypeIr::Float => "float",
        FieldTypeIr::Boolean => "boolean",
        FieldTypeIr::Object => "object",
        FieldTypeIr::Array => "array",
        FieldTypeIr::Timestamp => "timestamp",
    }
}

/// Compact type label for the table — `array<string>` for typed arrays, else the bare type.
pub fn type_label(f: &ExplainField) -> String {
    match &f.item_type {
        Some(it) => format!("{}<{}>", f.field_type, it),
        None => f.field_type.clone(),
    }
}

fn method_str(m: &HttpMethodIr) -> &'static str {
    match m {
        HttpMethodIr::Get => "GET",
        HttpMethodIr::Post => "POST",
        HttpMethodIr::Put => "PUT",
        HttpMethodIr::Patch => "PATCH",
        HttpMethodIr::Delete => "DELETE",
    }
}

pub fn build_view(desc: &ResourceDescriptor) -> ExplainView {
    let endpoints = ExplainEndpoints {
        base_path: desc.endpoints.base_path.clone(),
        list: desc.endpoints.list.clone(),
        get: desc.endpoints.get.clone(),
        create: desc.endpoints.create.clone(),
        update: desc.endpoints.update.clone(),
        update_method: desc.endpoints.update_method.as_ref().map(|m| method_str(m).to_string()),
        delete: desc.endpoints.delete.clone(),
    };

    // Source from the full schema (retained on the descriptor) rather than the
    // narrow `desc.fields` projection, so type / description / default / enum /
    // validation / nested sub-fields reach the structured output an LLM agent
    // authors against.
    let fields = build_fields(desc.schema.resource.schema.fields);

    // Field-level dependencies from the compile-time edge table. `name == kind`
    // for all schemas, so the graph (keyed by resource name) is reachable by kind.
    let dependencies = get_edges(&desc.name).unwrap_or(&[]).iter().map(|&(field_name, target_index, required, ..)| ExplainDependency { field: field_name.to_string(), target_kind: get_resource_by_index(target_index).name.to_string(), required }).collect();

    // Reverse edges: kinds that reference this one, the mirror of `dependencies`.
    let consumers = crate::dependency_graph::consumers(&desc.name).into_iter().map(|(kind, field, required)| ExplainConsumer { kind: kind.to_string(), field: field.to_string(), required }).collect();

    let advisories = crate::dependency_graph::resource_advisories(&desc.kind).iter().map(|&(severity, tier, date, text)| ExplainAdvisory { severity: severity.to_string(), tier: tier.to_string(), date: date.to_string(), text: text.to_string() }).collect();

    // Discriminator-scoped field groups (e.g. per-datasource-type fields on a
    // connection kind); the IR's `variants` pair slice is already sorted by
    // variant key at build time, so we just iterate it.
    let variants = match desc.schema.resource.schema.variants {
        Some(pairs) => pairs.iter().map(|(name, v)| ExplainVariant { name: name.to_string(), applies_to: v.applies_to.iter().map(|s| s.to_string()).collect(), fields: build_fields(v.fields) }).collect(),
        None => Vec::new(),
    };

    let prompt_notes = resource_prompt_notes(&desc.kind).iter().map(|s| s.to_string()).collect();

    ExplainView { kind: desc.kind.clone(), service: desc.service.clone(), id_field: desc.id_field.clone(), authoring: Authoring::new(), endpoints, fields, dependencies, consumers, advisories, variants, prompt_notes }
}

/// One row of the resource catalog, filtered by service and/or deployment flavor.
#[derive(Serialize, Debug, Clone)]
pub struct KindSummary {
    pub kind: String,
    pub service: String,
    pub deployment_support: Vec<String>,
    pub summary: String,
}

/// Full structured schema for one kind — the exact value `wxctl explain -o json`
/// serializes. Returns an error (listing valid kinds) for an unknown kind.
pub fn explain_kind(kind: &str) -> Result<ExplainView> {
    let ir = RESOURCE_IR.get(kind).copied().ok_or_else(|| {
        let mut ks: Vec<&str> = RESOURCE_IR.keys().copied().collect();
        ks.sort_unstable();
        anyhow::anyhow!("unknown kind '{kind}'. Valid kinds: {}.", ks.join(", "))
    })?;
    Ok(build_view(&ResourceDescriptor::from_ir(ir)))
}

/// The resource catalog as `KindSummary` rows, optionally narrowed to one service
/// and/or one deployment flavor (`saas`/`software`). Sorted by the catalog's order.
pub fn list_kinds(service: Option<&str>, deployment: Option<&str>) -> Vec<KindSummary> {
    resource_catalog()
        .iter()
        .filter(|(_, svc, _)| service.is_none_or(|s| *svc == s))
        .filter_map(|&(kind, svc, desc)| {
            let support = deployment_support(kind);
            if deployment.is_some_and(|d| !support.contains(&d)) {
                return None;
            }
            Some(KindSummary { kind: kind.to_string(), service: svc.to_string(), deployment_support: support.iter().map(|s| s.to_string()).collect(), summary: desc.to_string() })
        })
        .collect()
}
