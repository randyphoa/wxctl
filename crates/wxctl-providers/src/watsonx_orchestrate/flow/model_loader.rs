//! Flow model loading from source files

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use std::path::Path;

/// Flow model loaded from a source file
pub struct FlowModel {
    /// The name of the flow (from spec.name)
    pub name: String,
    /// The description of the flow (from spec.description)
    pub description: String,
    /// Input schema for the flow (from spec.input_schema)
    pub input_schema: Option<Value>,
    /// Output schema for the flow (from spec.output_schema)
    pub output_schema: Option<Value>,
    /// The complete flow model
    pub model: Value,
}

/// Load a flow model from a JSON or YAML file
///
/// The file must contain a flow model with the following structure:
/// ```yaml
/// spec:
///   name: flow_name
///   description: Flow description
///   input_schema: {...}  # optional
///   output_schema: {...}  # optional
/// nodes: {...}
/// edges: [...]
/// ```
pub fn load_flow_model(source_path: &Path) -> Result<FlowModel> {
    // Validate file exists
    if !source_path.exists() {
        bail!("Flow source file not found: {}", source_path.display());
    }

    // Validate it's a file, not a directory
    if !source_path.is_file() {
        bail!("Flow source path must be a file, not a directory: {}", source_path.display());
    }

    // Read file content
    let content = std::fs::read_to_string(source_path).context(format!("Failed to read flow source file: {}", source_path.display()))?;

    // Parse based on file extension
    let model: Value = match source_path.extension().and_then(|e| e.to_str()) {
        Some("json") => serde_json::from_str(&content).context(format!("Failed to parse JSON flow file: {}", source_path.display()))?,
        Some("yaml") | Some("yml") => serde_norway::from_str(&content).context(format!("Failed to parse YAML flow file: {}", source_path.display()))?,
        Some(ext) => bail!("Unsupported flow file format '{}'. Use .json, .yaml, or .yml", ext),
        None => bail!("Flow file must have an extension (.json, .yaml, or .yml): {}", source_path.display()),
    };

    // Extract and validate required fields
    let spec = model.get("spec").ok_or_else(|| anyhow!("Missing 'spec' in flow model"))?;

    let name = spec.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("Missing or invalid 'spec.name' in flow model"))?;

    let description = spec.get("description").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("Missing or invalid 'spec.description' in flow model"))?;

    // Extract optional schemas
    let input_schema = spec.get("input_schema").cloned();
    let output_schema = spec.get("output_schema").cloned();

    Ok(FlowModel { name: name.to_string(), description: description.to_string(), input_schema, output_schema, model })
}
