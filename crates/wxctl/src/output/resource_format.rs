//! Shared `-o table|json|yaml` rendering for read-only discovery commands
//! (`wxctl resources`, and `wxctl explain` in Phase 2).

use anyhow::Result;
use clap::ValueEnum;
use serde::Serialize;

/// Output format for resource-discovery commands. Distinct from
/// `crate::cli::OutputFormat` (which is Json-only and shared with `validate`).
#[derive(Clone, Copy, Debug, Default, ValueEnum)]
pub enum ResourceFormat {
    /// The grouped panel catalog with full descriptions.
    #[default]
    Table,
    Json,
    Yaml,
    Markdown,
}

/// A single column in a rendered table.
pub struct Column {
    pub header: &'static str,
    pub values: Vec<String>,
}

/// Render rows as a GitHub-flavored Markdown table to stdout. Pipe characters
/// in cell values are escaped so they don't break the column layout.
pub fn print_markdown_table(columns: &[Column]) {
    let row_count = columns.first().map(|c| c.values.len()).unwrap_or(0);

    let header: Vec<&str> = columns.iter().map(|c| c.header).collect();
    println!("| {} |", header.join(" | "));
    println!("| {} |", columns.iter().map(|_| "---").collect::<Vec<_>>().join(" | "));

    for i in 0..row_count {
        let cells: Vec<String> = columns.iter().map(|c| c.values[i].replace('|', "\\|")).collect();
        println!("| {} |", cells.join(" | "));
    }
}

/// Render any serializable value as pretty JSON to stdout.
pub fn print_json<T: Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Render any serializable value as YAML to stdout.
pub fn print_yaml<T: Serialize>(value: &T) -> Result<()> {
    print!("{}", serde_norway::to_string(value)?);
    Ok(())
}

/// Render `value` as JSON/YAML, or invoke `table` for the text formats (`Table`
/// / `Markdown`) — each command supplies its own layout (columnar list vs.
/// key/value detail) and branches on the passed format to pick the renderer.
pub fn render<T: Serialize>(value: &T, format: ResourceFormat, table: impl FnOnce(ResourceFormat)) -> Result<()> {
    match format {
        ResourceFormat::Json => print_json(value),
        ResourceFormat::Yaml => print_yaml(value),
        ResourceFormat::Table | ResourceFormat::Markdown => {
            table(format);
            Ok(())
        }
    }
}
