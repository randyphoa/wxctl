use crate::commands::compose::identify::{read_input, write_output};
use anyhow::{Result, bail};

pub fn execute(input: Option<&str>, paths: Option<&str>, resources_dir: Option<&str>, scaffold_dir: Option<&str>, config: Option<&str>, test_config: Option<&str>, output_path: Option<&str>) -> Result<()> {
    if let Some(scaffold) = scaffold_dir {
        return execute_pass4(scaffold, input, config, output_path);
    }
    if let Some(config_path) = test_config {
        return execute_test_gen(config_path, input, output_path);
    }
    match (input, paths) {
        (Some(input_text), None) => execute_pass1(input_text, output_path),
        (Some(input_text), Some(paths_file)) => execute_pass3(input_text, paths_file, resources_dir, output_path),
        (None, Some(_)) => bail!("--paths requires --input to be specified"),
        (None, None) => bail!(
            "--input is required.\n\
             Use --input to assemble an LLM generation prompt.\n\
             Use --input with --paths for Pass 3 prompt assembly.\n\
             Use --scaffold-dir for Pass 4 implementation prompt assembly.\n\
             Use --test-config for test generation prompt assembly."
        ),
    }
}

fn execute_pass1(input: &str, output_path: Option<&str>) -> Result<()> {
    let prompt = wxctl_compose::assemble_identify_prompt(&read_input(input)?)?;
    write_output(&prompt, output_path)
}

fn execute_pass3(input: &str, paths_file: &str, resources_dir: Option<&str>, output_path: Option<&str>) -> Result<()> {
    let user_input = read_input(input)?;
    let paths_yaml = std::fs::read_to_string(paths_file).map_err(|e| anyhow::anyhow!("Failed to read paths file '{}': {}", paths_file, e))?;
    let files = wxctl_compose::discover_existing_resources(resources_dir)?;
    let existing = wxctl_compose::render_existing_resources(&files);
    let prompt = wxctl_compose::assemble_config_prompt(&user_input, &paths_yaml, &existing)?;
    write_output(&prompt, output_path)
}

fn execute_pass4(scaffold_dir: &str, input: Option<&str>, config: Option<&str>, output_path: Option<&str>) -> Result<()> {
    let descriptions = match config {
        Some(path) => wxctl_compose::prompt::tool_descriptions_from_config(path)?,
        None => std::collections::HashMap::new(),
    };
    let original_input = match input {
        Some(text) => read_input(text)?,
        None => std::fs::read_to_string("input.txt").unwrap_or_default(),
    };
    if original_input.trim().is_empty() {
        eprintln!("warning: no use-case context (pass --input or place input.txt in the working directory); implementation prompt assembled without it");
    }
    let prompt = wxctl_compose::assemble_implementation_prompt(scaffold_dir, &original_input, &descriptions)?;
    write_output(&prompt, output_path)
}

fn execute_test_gen(config_path: &str, input: Option<&str>, output_path: Option<&str>) -> Result<()> {
    let config_yaml = std::fs::read_to_string(config_path).map_err(|e| anyhow::anyhow!("Failed to read config '{}': {}", config_path, e))?;
    let user_input = match input {
        Some(text) => read_input(text)?,
        None => std::fs::read_to_string("input.txt").unwrap_or_default(),
    };
    let prompt = wxctl_compose::assemble_test_prompt(&config_yaml, &user_input)?;
    write_output(&prompt, output_path)
}
