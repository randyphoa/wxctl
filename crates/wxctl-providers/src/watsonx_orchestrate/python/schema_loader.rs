use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;

/// Tool schemas loaded from schema.yaml
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSchemas {
    pub input_schema: Value,
    pub output_schema: Value,
}

/// Load schemas from schema.yaml in source directory
pub fn load_schemas(source_dir: &Path) -> Result<ToolSchemas> {
    let schema_path = source_dir.join("schema.yaml");

    if !schema_path.exists() {
        bail!("schema.yaml not found in source directory: {}", source_dir.display());
    }

    let content = std::fs::read_to_string(&schema_path).context(format!("Failed to read schema.yaml from {}", schema_path.display()))?;

    let schemas: ToolSchemas = serde_norway::from_str(&content).context("Failed to parse schema.yaml")?;

    // Validate that schemas are objects
    if !schemas.input_schema.is_object() {
        bail!("input_schema must be a JSON object");
    }

    if !schemas.output_schema.is_object() {
        bail!("output_schema must be a JSON object");
    }

    Ok(schemas)
}
