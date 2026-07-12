use anyhow::Result;
use std::path::Path;

/// Parse requirements.txt into Vec<String>
pub fn parse_requirements_file(source_dir: &Path) -> Result<Vec<String>> {
    let req_path = source_dir.join("requirements.txt");

    if !req_path.exists() {
        return Ok(vec![]);
    }

    let content = std::fs::read_to_string(&req_path)?;

    Ok(content.lines().map(|line| line.trim()).filter(|line| !line.is_empty() && !line.starts_with('#')).map(String::from).collect())
}
