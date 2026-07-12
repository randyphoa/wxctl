use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use zip::ZipWriter;

use crate::util::validate_path;

/// Builder for OpenAPI tool artifacts.
/// Creates a ZIP containing the spec file + bundle-format marker.
/// Matches ADK tools_controller.py lines 1065-1072.
pub struct OpenApiArtifactBuilder {
    source_file: PathBuf,
    temp_dir: TempDir,
}

impl OpenApiArtifactBuilder {
    pub fn new(source_file: PathBuf) -> Result<Self> {
        let validated = validate_path(&source_file)?;
        let temp_dir = TempDir::new().context("Failed to create temp directory")?;
        Ok(Self { source_file: validated, temp_dir })
    }

    /// Compute BLAKE3 hash of the ZIP artifact for change detection.
    /// Builds a temporary ZIP and hashes it, ensuring the hash matches what `build()` produces.
    pub fn compute_source_hash(&self) -> Result<String> {
        let temp_zip = self.temp_dir.path().join("hash_artifact.zip");
        self.build_zip(&temp_zip)?;
        crate::util::hash_file_blake3(&temp_zip)
    }

    /// Build ZIP artifact and return (path, hash).
    /// ZIP contains: spec file (original filename) + bundle-format.
    pub fn build(self) -> Result<(PathBuf, String)> {
        let zip_path = self.temp_dir.path().join("tool_artifact.zip");
        self.build_zip(&zip_path)?;

        let hash = crate::util::hash_file_blake3(&zip_path)?;

        let persisted_path = self.temp_dir.keep();
        let final_zip_path = persisted_path.join("tool_artifact.zip");

        Ok((final_zip_path, hash))
    }

    fn build_zip(&self, zip_path: &Path) -> Result<()> {
        let file = std::fs::File::create(zip_path).context("Failed to create ZIP file")?;
        let mut zip = ZipWriter::new(file);

        let options = crate::util::deterministic_zip_options();

        let filename = self.source_file.file_name().context("Spec file has no filename")?.to_string_lossy().to_string();

        zip.start_file(&filename, options).context("Failed to start spec file entry")?;
        let content = std::fs::read(&self.source_file).context("Failed to read spec file")?;
        zip.write_all(&content).context("Failed to write spec file")?;

        zip.start_file("bundle-format", options).context("Failed to start bundle-format entry")?;
        zip.write_all(b"2.0.0\n").context("Failed to write bundle-format")?;

        zip.finish().context("Failed to finish ZIP")?;
        Ok(())
    }
}
