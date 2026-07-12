//! Pure, wasm-safe offline config validation: schema-shape checks, `${...}`
//! dependency-reference grammar, and cross-resource invariants. The single source
//! of truth shared by the CLI's `ValidationPipeline` (which adds the IO/overlay
//! orchestration) and the remote MCP server's `validate_config` wasm binding.

pub mod config;
pub mod cross_resource;
pub mod dependency;
pub mod readiness;
pub mod schema;
pub mod types;

pub use config::{ValidationReport, validate_config};
pub use types::{AnnotatedValidationError, ValidationError};

/// Validation error-code constants referenced by the validators. Mirrors the
/// canonical set in `wxctl-core::logging::error_codes` (kept in sync; this crate
/// is a wasm-safe leaf and cannot depend on `wxctl-core`).
pub mod error_codes {
    pub const V005: &str = "WXCTL-V005";
    pub const V009: &str = "WXCTL-V009";
    pub const V401: &str = "WXCTL-V401";
    pub const V402: &str = "WXCTL-V402";
    pub const V403: &str = "WXCTL-V403";
    pub const V501: &str = "WXCTL-V501";
    pub const V503: &str = "WXCTL-V503";
    pub const V504: &str = "WXCTL-V504";
}

use crate::schema::ResourceSchema;
use crate::schema::definition::SchemaDefinition;
use anyhow::anyhow;
use serde_json::Value;

/// Normalize user-facing field names (aliases) to API field names. (Moved from the
/// engine's validation pipeline so the offline validator shares one implementation.)
pub fn normalize_raw_resource_fields(data: &mut Value, schema: &SchemaDefinition, kind: &str) -> anyhow::Result<()> {
    let field_mapping = schema.build_field_mapping();

    // Extract ref_name from data for error messages (fallback to "unnamed")
    let ref_name = data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();

    let map = data.as_object_mut().ok_or_else(|| anyhow!("Resource '{}:{}' data is not an object", kind, ref_name))?;

    // Step 1: Detect conflicts (both API field and alias present)
    for (alias, api_field) in &field_mapping {
        // Skip check when alias and API field are the same (no aliasing needed)
        if alias == api_field {
            continue;
        }
        if map.contains_key(alias) && map.contains_key(api_field) {
            return Err(anyhow!(
                "Field conflict in resource '{}:{}': Cannot specify both '{}' and '{}'. \
                 Use only one.",
                kind,
                ref_name,
                api_field,
                alias
            ));
        }
    }

    // Step 2: Transform alias → API field
    let mut transformations = Vec::new();
    for (alias, api_field) in &field_mapping {
        if let Some(value) = map.remove(alias) {
            transformations.push((api_field.clone(), value));
        }
    }

    // Step 3: Apply transformations
    for (api_field, value) in transformations {
        map.insert(api_field, value);
    }

    Ok(())
}

/// Dereference generic 'id' field to schema-specific id_source field. (Moved from the
/// engine's validation pipeline.)
pub fn dereference_id_field(data: &mut Value, schema: &ResourceSchema, kind: &str) -> anyhow::Result<()> {
    // Check if generic 'id' field is present
    let has_id = data.get("id").is_some();

    if !has_id {
        return Ok(()); // No ID dereferencing needed
    }

    let def = &schema.resource;
    let id_source_field = &def.reconciliation.discovery.id_source;

    // Extract ref_name for error messages
    let ref_name = data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();

    let map = data.as_object_mut().ok_or_else(|| anyhow!("Resource '{}:{}' data is not an object", kind, ref_name))?;

    // Special case: When id_source is already "id", no dereferencing needed
    // Just add metadata flag and return
    if id_source_field == "id" {
        // Validate 'id' field is a string
        let id_value = map.get("id").ok_or_else(|| anyhow!("Resource '{}:{}': 'id' field is missing", kind, ref_name))?;

        if !id_value.is_string() {
            return Err(anyhow!("Resource '{}:{}': 'id' field must be a string, got {}", kind, ref_name, id_value));
        }

        // Add metadata flag (no dereferencing needed)
        map.insert("_from_id".to_string(), Value::Bool(true));
        return Ok(());
    }

    // Step 1: Detect conflicts (both 'id' and id_source field present)
    // Note: This check is skipped when id_source == "id" (handled above)
    if map.contains_key("id") && map.contains_key(id_source_field) {
        return Err(anyhow!(
            "Field conflict in resource '{}:{}': Cannot specify both 'id' and '{}'. \
             Use 'id' for existing resource references, or '{}' for new resources.",
            kind,
            ref_name,
            id_source_field,
            id_source_field
        ));
    }

    // Step 2: Extract and validate 'id' value
    let id_value = map.remove("id").ok_or_else(|| anyhow!("Resource '{}:{}': 'id' field disappeared during dereferencing", kind, ref_name))?;

    let id_str = id_value.as_str().ok_or_else(|| anyhow!("Resource '{}:{}': 'id' field must be a string, got {}", kind, ref_name, id_value))?;

    // Step 3: Dereference id → id_source field
    map.insert(id_source_field.clone(), Value::String(id_str.to_string()));

    // Step 4: Add metadata flag to track ID dereferencing
    map.insert("_from_id".to_string(), Value::Bool(true));

    Ok(())
}
