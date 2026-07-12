use anyhow::{Context, Result};
use std::path::Path;

/// `wxctl compose identify` — read use case, assemble the Pass-1 prompt, write it.
pub fn execute(input: &str, output_path: Option<&str>) -> Result<()> {
    let user_input = read_input(input)?;
    let prompt = wxctl_compose::assemble_identify_prompt(&user_input)?;
    write_output(&prompt, output_path)
}

pub(crate) fn read_input(input: &str) -> Result<String> {
    if Path::new(input).is_file() { std::fs::read_to_string(input).with_context(|| format!("Failed to read input file '{}'", input)) } else { Ok(input.to_string()) }
}

pub(crate) fn write_output(content: &str, output_path: Option<&str>) -> Result<()> {
    if let Some(path) = output_path {
        std::fs::write(path, content).with_context(|| format!("Failed to write to '{}'", path))?;
        eprintln!("Prompt written to: {}", path);
    } else {
        print!("{}", content);
    }
    Ok(())
}
