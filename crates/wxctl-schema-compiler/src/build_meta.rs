//! Build-time schema-file metadata: bundles what the codegen emitters need
//! beyond the runtime `ResourceSchema` model (advisories, raw `unsupported_on`
//! strings, the `include_str!` path) plus the cross-service linkage types. Edge
//! collection runs over the typed model in `crate::definition`, not a second raw
//! parse.

use serde::Deserialize;

/// One parsed schema YAML file: the full normalized model plus the build-only
/// metadata the emitters need alongside it.
pub struct ParsedSchema {
    /// Full, normalized, reshaped schema (via `SchemaParser::parse_str`).
    pub schema: crate::definition::ResourceSchema,
    /// Top-level `advisories:` block (sibling to `resource:`).
    pub advisories: Vec<AdvisoryDef>,
    /// Raw `unsupported_on` constraint strings, pre-typing (byte-identical to
    /// the YAML source, for `UNSUPPORTED_ON` emission).
    pub unsupported_on_raw: Vec<String>,
    /// Forward-slashed absolute `include_str!` path for this schema file.
    pub include_path: String,
}

/// One entry of a schema's top-level `advisories:` block.
#[derive(Deserialize)]
pub struct AdvisoryDef {
    pub severity: String, // "info" | "warn"
    pub tier: String,
    pub date: String,
    pub text: String,
}

// ============================================================================
// Linkage parsing structures (moved verbatim from `wxctl-schema/build.rs`)
// ============================================================================

#[derive(Deserialize)]
pub struct LinkagesFile {
    pub bridges: Vec<BridgeDef>,
}

#[derive(Deserialize)]
pub struct BridgeDef {
    pub name: String,
    pub source: String,
    pub target: String,
    pub constraints: std::collections::HashMap<String, std::collections::HashMap<String, serde_norway::Value>>,
    pub field_mapping: Vec<FieldMappingDef>,
    /// Optional deployment scope. Stored as the raw constraint string (or
    /// list-as-comma-string); parsed at runtime via
    /// `DeploymentConstraintList::from_str`. Empty string = always active.
    #[serde(default)]
    pub when: Option<BridgeWhenDef>,
}

#[derive(Deserialize, Clone)]
pub struct BridgeWhenDef {
    #[serde(default)]
    pub deployment: Option<serde_norway::Value>,
}

#[derive(Deserialize)]
pub struct FieldMappingDef {
    pub source: String,
    pub target: String,
}

/// Render a bridge's `when.deployment` value as a display string (comma-joined
/// when a sequence). Empty string when no `when` block, or no `deployment` key.
pub fn bridge_when_string(when: &Option<BridgeWhenDef>) -> String {
    let Some(w) = when else {
        return String::new();
    };
    let Some(d) = &w.deployment else {
        return String::new();
    };
    match d {
        serde_norway::Value::String(s) => s.clone(),
        serde_norway::Value::Sequence(items) => items.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect::<Vec<_>>().join(", "),
        _ => String::new(),
    }
}

/// Tiny raw-parse shape used only to capture the top-level `advisories:` block
/// and the pre-typed `unsupported_on` strings alongside the full typed parse.
#[derive(Deserialize)]
struct RawSchemaFile {
    #[serde(default)]
    advisories: Vec<AdvisoryDef>,
    resource: RawResource,
}

#[derive(Deserialize)]
struct RawResource {
    #[serde(default)]
    unsupported_on: Vec<String>,
}

/// Parse one schema file's YAML into a `ParsedSchema`. `include_path` is
/// passed through unchanged (forward-slashed absolute `include_str!` path).
pub fn parse_schema_file(yaml: &str, include_path: String) -> anyhow::Result<ParsedSchema> {
    let schema = crate::parser::SchemaParser::parse_str(yaml)?;
    let raw: RawSchemaFile = serde_norway::from_str(yaml)?;
    Ok(ParsedSchema { schema, advisories: raw.advisories, unsupported_on_raw: raw.resource.unsupported_on, include_path })
}
