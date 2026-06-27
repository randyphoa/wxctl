//! Deterministic schema-reference markdown for the LLM generation/fix prompts,
//! rendered from the compiled provider schemas (`load_all_schemas`) — the same
//! single source of truth `wxctl explain` reads.
//!
//! This replaces the hand-authored `compose/schema/reference.md` +
//! `compose/resources/<service>/<kind>.md` (the old "Manual Step 1"), which
//! covered only a subset of kinds and drifted from the schemas. `generate` Pass 3
//! and `validate --fix-prompt` both consume this so config generation always sees
//! the live schema set.

use anyhow::Result;
use std::collections::HashSet;

/// Render markdown reference docs for the requested kinds — or every kind when
/// `only` is `None` or empty — sourced from `load_all_schemas()`. Output is
/// stable (kinds sorted alphabetically) and `---`-separated, matching the layout
/// the prompt templates expect.
pub fn render_kinds_markdown(only: Option<&HashSet<&str>>) -> Result<String> {
    let mut schemas = crate::load_all_schemas()?;
    schemas.sort_by(|a, b| a.resource.kind.cmp(&b.resource.kind));

    let select_all = only.is_none_or(|kinds| kinds.is_empty());
    let mut out = String::new();
    for schema in &schemas {
        if !select_all && !only.unwrap().contains(schema.resource.kind.as_str()) {
            continue;
        }
        // Serialize back to the `{ resource: {...} }` shape the renderer indexes,
        // so we reuse one renderer over the compiled (not disk-read) schema.
        let doc = serde_norway::to_value(schema)?;
        out.push_str(&render_schema_value(&doc));
        out.push_str("\n---\n\n");
    }
    Ok(out)
}

/// Render one schema document (`{ resource: {...} }`) into markdown: header,
/// fields table, validation rules, allowed values, nested structures, prompt
/// notes, and minimal/full examples.
fn render_schema_value(doc: &serde_norway::Value) -> String {
    let resource = &doc["resource"];
    let kind = yaml_str(&resource["kind"]);
    let service = yaml_str(&resource["service"]);
    let description = yaml_str(&resource["description"]);

    let mut out = String::new();

    // Header
    out.push_str(&format!("# {}\n\n", kind));
    out.push_str(&format!("**Service:** {}\n\n", service));
    out.push_str(&format!("**Description:** {}\n\n", description.trim()));
    out.push_str("---\n\n");

    // Fields table
    if let Some(fields) = resource["schema"]["fields"].as_sequence() {
        // Filter out Computed fields for the main table — they're never authored.
        let user_fields: Vec<_> = fields.iter().filter(|f| yaml_str_opt(&f["location"]).as_deref() != Some("Computed")).collect();

        if !user_fields.is_empty() {
            out.push_str("## Fields\n\n");
            out.push_str("| Field | Type | Required | Default | Description |\n");
            out.push_str("|-------|------|----------|---------|-------------|\n");

            for field in &user_fields {
                let name = yaml_str(&field["name"]);
                let ftype = yaml_str(&field["type"]);
                let required = field["required"].as_bool().unwrap_or(false);
                let default_val = format_default(&field["default"]);
                let mut desc = yaml_str_opt(&field["description"]).map(|s| s.trim().replace('\n', " ")).unwrap_or_default();
                // Truncate long descriptions for table readability.
                if desc.len() > 120 {
                    let truncated = &desc[..117];
                    if let Some(pos) = truncated.rfind(' ') {
                        desc = format!("{}...", &desc[..pos]);
                    } else {
                        desc = format!("{}...", truncated);
                    }
                }
                // Flag only non-default locations (Body is the implicit default).
                if let Some(loc) = yaml_str_opt(&field["location"]).filter(|l| l != "Body") {
                    desc = format!("*({})* {}", loc, desc);
                }
                out.push_str(&format!("| `{}` | {} | {} | {} | {} |\n", name, ftype, if required { "Yes" } else { "No" }, default_val, desc));
            }
            out.push_str("\n---\n\n");

            // Validation Rules section
            let mut validation_lines = Vec::new();
            for field in &user_fields {
                if let Some(validation) = field["validation"].as_mapping() {
                    let name = yaml_str(&field["name"]);
                    // Skip null members — serializing ValidationRules emits every key.
                    let rules: Vec<String> = validation.iter().filter(|(_, v)| !v.is_null()).map(|(k, v)| format!("{}: {}", yaml_str(k), format_yaml_value(v))).collect();
                    if !rules.is_empty() {
                        validation_lines.push(format!("- **`{}`**: {}", name, rules.join(", ")));
                    }
                }
            }
            if !validation_lines.is_empty() {
                out.push_str("## Validation Rules\n\n");
                for line in &validation_lines {
                    out.push_str(line);
                    out.push('\n');
                }
                out.push_str("\n---\n\n");
            }

            // Allowed Values section
            let mut allowed_lines = Vec::new();
            for field in &user_fields {
                if let Some(vals) = field["allowed_values"].as_sequence() {
                    let name = yaml_str(&field["name"]);
                    let formatted: Vec<String> = vals.iter().map(|v| format!("`{}`", format_yaml_value(v))).collect();
                    allowed_lines.push(format!("**`{}`:** {}", name, formatted.join(" | ")));
                }
            }
            if !allowed_lines.is_empty() {
                out.push_str("## Allowed Values\n\n");
                for line in &allowed_lines {
                    out.push_str(line);
                    out.push_str("\n\n");
                }
                out.push_str("---\n\n");
            }

            // Nested Structures section
            let mut nested_lines = Vec::new();
            for field in &user_fields {
                if let Some(props) = field["properties"].as_mapping() {
                    let name = yaml_str(&field["name"]);
                    let desc = yaml_str_opt(&field["description"]).map(|s| s.trim().replace('\n', " ")).unwrap_or_default();
                    nested_lines.push(format!("### `{}`\n\n{}\n", name, desc));
                    nested_lines.push("| Property | Type | Description |\n".to_string());
                    nested_lines.push("|----------|------|-------------|\n".to_string());
                    for (k, v) in props {
                        let prop_name = yaml_str(k);
                        let prop_type = yaml_str_opt(&v["type"]).unwrap_or_default();
                        let prop_desc = yaml_str_opt(&v["description"]).map(|s| s.trim().replace('\n', " ")).unwrap_or_default();
                        nested_lines.push(format!("| `{}` | {} | {} |\n", prop_name, prop_type, prop_desc));
                    }
                    nested_lines.push("\n".to_string());
                }
            }
            if !nested_lines.is_empty() {
                out.push_str("## Nested Structures\n\n");
                for line in &nested_lines {
                    out.push_str(line);
                }
                out.push_str("---\n\n");
            }
        }
    }

    // Prompt notes
    if let Some(notes) = resource["prompt"]["notes"].as_sequence()
        && !notes.is_empty()
    {
        out.push_str("## Notes\n\n");
        for note in notes {
            out.push_str(&format!("- {}\n", yaml_str(note)));
        }
        out.push_str("\n---\n\n");
    }

    // Prompt examples
    if let Some(minimal) = resource["prompt"]["examples"]["minimal"].as_str() {
        out.push_str("## Minimal Example\n\n```yaml\n");
        out.push_str(minimal.trim_end());
        out.push_str("\n```\n\n---\n\n");
    }
    if let Some(full) = resource["prompt"]["examples"]["full"].as_str() {
        out.push_str("## Full Example\n\n```yaml\n");
        out.push_str(full.trim_end());
        out.push_str("\n```\n");
    }

    out
}

/// Extract a string from a YAML value, returning an empty string for non-string/null values.
fn yaml_str(value: &serde_norway::Value) -> String {
    match value {
        serde_norway::Value::String(s) => s.clone(),
        serde_norway::Value::Null => String::new(),
        other => serde_norway::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

/// Extract an optional string from a YAML value.
fn yaml_str_opt(value: &serde_norway::Value) -> Option<String> {
    match value {
        serde_norway::Value::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// Format a YAML default value for display in the fields table.
fn format_default(value: &serde_norway::Value) -> String {
    match value {
        serde_norway::Value::Null => "\u{2014}".to_string(), // em dash
        serde_norway::Value::Bool(b) => format!("`{}`", b),
        serde_norway::Value::Number(n) => format!("`{}`", n),
        serde_norway::Value::String(s) => {
            if s.is_empty() {
                "\u{2014}".to_string()
            } else {
                format!("`{}`", s)
            }
        }
        serde_norway::Value::Sequence(seq) => {
            if seq.is_empty() {
                "`[]`".to_string()
            } else {
                format!("`{}`", serde_norway::to_string(value).unwrap_or_default().trim())
            }
        }
        _ => "\u{2014}".to_string(),
    }
}

/// Format a YAML value for inline display.
fn format_yaml_value(value: &serde_norway::Value) -> String {
    match value {
        serde_norway::Value::String(s) => s.clone(),
        serde_norway::Value::Bool(b) => b.to_string(),
        serde_norway::Value::Number(n) => n.to_string(),
        serde_norway::Value::Null => "null".to_string(),
        other => serde_norway::to_string(other).unwrap_or_default().trim().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both "render all" paths — `None` and `Some(&empty_set)` — document every
    /// kind from the live schema set, with no serialization noise leaking through.
    #[test]
    fn renders_all_kinds_without_serialization_noise() {
        let total = crate::load_all_schemas().unwrap().len();
        let empty: HashSet<&str> = HashSet::new();
        // None (no-arg) and Some(&empty) both mean "all kinds".
        for md in [render_kinds_markdown(None).expect("renders"), render_kinds_markdown(Some(&empty)).expect("renders")] {
            assert_eq!(md.matches("\n**Service:**").count(), total, "every kind is documented");
            // Round-trip leaks we explicitly suppress:
            assert!(!md.contains(": null"), "null validation members are filtered");
            assert!(!md.contains("*(Body)*"), "the default Body location is not annotated");
        }
    }

    /// Filtering to specific kinds yields only those — the validate fix-prompt path.
    #[test]
    fn filters_to_requested_kinds() {
        let only: HashSet<&str> = ["s3_bucket"].into_iter().collect();
        let md = render_kinds_markdown(Some(&only)).expect("renders");
        assert!(md.contains("# s3_bucket"));
        assert!(!md.contains("# agent"));
        assert_eq!(md.matches("\n**Service:**").count(), 1);
    }
}
