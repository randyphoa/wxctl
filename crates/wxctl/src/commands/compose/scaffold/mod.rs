//! `wxctl compose scaffold` — thin CLI wrapper: read config, run the compose-core
//! scaffolder, print the manifest, exit non-zero on any failure.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use wxctl_compose::scaffold::{apply_impl_file, scaffold_config};
use wxctl_core::Config;

pub fn execute(config_path: &str, output_dir: Option<&str>, apply_implementations: Option<&str>, dry_run: bool) -> Result<()> {
    let content = std::fs::read_to_string(config_path).with_context(|| format!("Failed to read config '{}'", config_path))?;
    let config = Config::from_yaml(&content)?;
    if let Some(impl_file) = apply_implementations {
        return apply_impl_file(&config, impl_file);
    }
    let config_dir = Path::new(config_path).parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
    let out = scaffold_config(&config, output_dir, &config_dir, dry_run);
    eprint!("{}", out.manifest.render());
    if out.manifest.any_failed() {
        anyhow::bail!("scaffold completed with failures");
    }
    Ok(())
}
