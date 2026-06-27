use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use zip::ZipWriter;
use zip::write::FileOptions;

use crate::util::validate_path;

/// Builder for MCP toolkit artifacts.
/// Creates a ZIP of the entire server_path directory.
/// Matches ADK toolkit_controller.py _populate_zip().
pub struct McpArtifactBuilder {
    source_dir: PathBuf,
    temp_dir: TempDir,
}

impl McpArtifactBuilder {
    pub fn new(source_dir: PathBuf) -> Result<Self> {
        let validated = validate_path(&source_dir)?;
        let temp_dir = TempDir::new().context("Failed to create temp directory")?;
        Ok(Self { source_dir: validated, temp_dir })
    }

    #[cfg(test)]
    pub fn compute_source_hash(&self) -> Result<String> {
        let temp_zip = self.temp_dir.path().join("hash_artifact.zip");
        self.build_zip(&temp_zip)?;
        crate::util::hash_file_blake3(&temp_zip)
    }

    pub fn build(self) -> Result<(PathBuf, String)> {
        let zip_path = self.temp_dir.path().join("toolkit_artifact.zip");
        self.build_zip(&zip_path)?;
        let hash = crate::util::hash_file_blake3(&zip_path)?;
        let persisted_path = self.temp_dir.keep();
        Ok((persisted_path.join("toolkit_artifact.zip"), hash))
    }

    fn build_zip(&self, zip_path: &Path) -> Result<()> {
        let file = std::fs::File::create(zip_path).context("Failed to create ZIP")?;
        let mut zip = ZipWriter::new(file);
        let options = crate::util::deterministic_zip_options();

        self.walk_dir(&self.source_dir, &self.source_dir, &mut zip, options)?;
        zip.finish().context("Failed to finish ZIP")?;
        Ok(())
    }

    fn walk_dir(&self, dir: &Path, base: &Path, zip: &mut ZipWriter<std::fs::File>, options: FileOptions<'_, ()>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir).with_context(|| format!("Failed to read directory: {}", dir.display()))?.filter_map(|e| e.ok()).collect();
        entries.sort_by_key(|e| e.file_name());

        for entry in entries {
            let path = entry.path();
            let relative = path.strip_prefix(base).context("Failed to strip prefix")?;
            let name = relative.to_string_lossy().to_string();

            // Skip node_modules (matching ADK behavior)
            if name.contains("node_modules") {
                continue;
            }

            if path.is_dir() {
                self.walk_dir(&path, base, zip, options)?;
            } else {
                zip.start_file(&name, options).with_context(|| format!("Failed to add file: {}", name))?;
                let content = std::fs::read(&path).with_context(|| format!("Failed to read: {}", path.display()))?;
                zip.write_all(&content).context("Failed to write to ZIP")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    /// Create a test MCP server directory within the CWD (required by validate_path).
    fn create_test_mcp_dir_in_cwd(dir_name: &str) -> PathBuf {
        let cwd = std::env::current_dir().unwrap();
        let server_dir = cwd.join(dir_name);
        std::fs::create_dir_all(&server_dir).unwrap();
        std::fs::write(server_dir.join("index.js"), "// MCP server entry point\n").unwrap();
        std::fs::write(server_dir.join("package.json"), r#"{"name":"test-mcp","version":"1.0.0"}"#).unwrap();
        server_dir
    }

    #[test]
    fn test_build_artifact() {
        let _g = crate::test_support::lock_cwd();
        let server_dir = create_test_mcp_dir_in_cwd("test_mcp_server");

        let builder = McpArtifactBuilder::new(server_dir.clone()).unwrap();
        let (zip_path, hash) = builder.build().unwrap();

        assert!(zip_path.exists());
        assert!(!hash.is_empty());

        let file = std::fs::File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().to_string()).collect();
        assert!(names.contains(&"index.js".to_string()));
        assert!(names.contains(&"package.json".to_string()));

        // Archived bytes round-trip back to the on-disk source.
        let mut entry = archive.by_name("index.js").unwrap();
        let mut content = String::new();
        entry.read_to_string(&mut content).unwrap();
        assert_eq!(content, "// MCP server entry point\n");
        drop(entry);

        if let Some(parent) = zip_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&server_dir);
    }

    #[test]
    fn test_skips_node_modules() {
        let _g = crate::test_support::lock_cwd();
        let cwd = std::env::current_dir().unwrap();
        let server_dir = cwd.join("test_mcp_server_node_modules");
        std::fs::create_dir_all(&server_dir).unwrap();
        std::fs::write(server_dir.join("index.js"), "// entry\n").unwrap();
        let node_modules = server_dir.join("node_modules");
        std::fs::create_dir_all(&node_modules).unwrap();
        std::fs::write(node_modules.join("some_dep.js"), "// dep\n").unwrap();

        let builder = McpArtifactBuilder::new(server_dir.clone()).unwrap();
        let (zip_path, _hash) = builder.build().unwrap();

        let file = std::fs::File::open(&zip_path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let names: Vec<String> = (0..archive.len()).map(|i| archive.by_index(i).unwrap().name().to_string()).collect();
        assert!(names.contains(&"index.js".to_string()));
        assert!(!names.iter().any(|n| n.contains("node_modules")), "node_modules should be excluded");

        if let Some(parent) = zip_path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
        let _ = std::fs::remove_dir_all(&server_dir);
    }

    // Shared-CWD fixture — holds the crate-wide CWD lock so a concurrent
    // `set_current_dir` (python artifact tests) can't break `validate_path`.
    #[test]
    fn test_compute_source_hash_deterministic_and_content_sensitive() {
        let _g = crate::test_support::lock_cwd();
        let server_dir = create_test_mcp_dir_in_cwd("test_mcp_server_hash");

        // Deterministic: two builders over the same source produce the same hash.
        let hash1 = McpArtifactBuilder::new(server_dir.clone()).unwrap().compute_source_hash().unwrap();
        let hash2 = McpArtifactBuilder::new(server_dir.clone()).unwrap().compute_source_hash().unwrap();
        assert_eq!(hash1, hash2, "deterministic for unchanged source");

        // Content-sensitive: editing a file changes the hash.
        std::fs::write(server_dir.join("index.js"), "// Modified MCP server entry point\n").unwrap();
        let hash3 = McpArtifactBuilder::new(server_dir.clone()).unwrap().compute_source_hash().unwrap();
        assert_ne!(hash1, hash3, "hash should change when content changes");

        let _ = std::fs::remove_dir_all(&server_dir);
    }
}
