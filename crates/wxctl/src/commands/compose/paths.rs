use crate::commands::common::load_configs;
use anyhow::{Context, Result};
use wxctl_compose::{PathsInput, resolve_paths};

/// `wxctl compose paths` — join `-f` files, resolve the recommended path(s), write YAML.
pub fn execute(config_paths: &[String], deployment: &str, output_path: Option<&str>) -> Result<()> {
    let content = load_configs(config_paths)?;
    let yaml = resolve_paths(PathsInput { content: &content, deployment })?;
    if let Some(path) = output_path {
        std::fs::write(path, &yaml).with_context(|| format!("Failed to write output to '{}'", path))?;
        eprintln!("Resolved paths written to: {}", path);
    } else {
        print!("{}", yaml);
    }
    Ok(())
}
