use anyhow::{Context, Result};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::TempDir;
use zip::ZipWriter;

use crate::util::validate_path;

/// Builder for creating tool artifacts
pub struct ArtifactBuilder {
    source_dir: PathBuf,
    temp_dir: TempDir,
}

impl ArtifactBuilder {
    /// Create new artifact builder, validating the path against traversal attacks.
    pub fn new(source_dir: PathBuf) -> Result<Self> {
        let validated_source_dir = validate_path(&source_dir)?;
        let temp_dir = TempDir::new().context("Failed to create temp directory")?;
        Ok(Self { source_dir: validated_source_dir, temp_dir })
    }

    /// Compute BLAKE3 hash of the zipped artifact
    pub fn compute_source_hash(&self) -> Result<String> {
        // Build ZIP in a temporary location
        let temp_zip = self.temp_dir.path().join("hash_artifact.zip");
        self.build_zip(&temp_zip)?;

        crate::util::hash_file_blake3(&temp_zip)
    }

    /// Build ZIP artifact with all required files and return path with hash
    pub fn build(self) -> Result<(PathBuf, String)> {
        let zip_path = self.temp_dir.path().join("tool_artifact.zip");
        self.build_zip(&zip_path)?;

        let hash = crate::util::hash_file_blake3(&zip_path)?;

        // Convert TempDir to PathBuf to persist it beyond this function
        // Caller is responsible for cleanup after upload
        let persisted_path = self.temp_dir.keep();
        let final_zip_path = persisted_path.join("tool_artifact.zip");

        Ok((final_zip_path, hash))
    }

    /// Internal helper to build ZIP at the specified path
    fn build_zip(&self, zip_path: &Path) -> Result<()> {
        let file = File::create(zip_path).context("Failed to create ZIP file")?;
        let mut zip = ZipWriter::new(file);

        let options = crate::util::deterministic_zip_options();

        // Sort Python files for deterministic ordering
        let mut py_files = self.collect_python_files()?;
        py_files.sort();

        // Add all Python files
        for py_file in py_files {
            let relative = py_file.strip_prefix(&self.source_dir).context("Failed to strip prefix")?;

            zip.start_file(relative.to_string_lossy().to_string(), options).context("Failed to start ZIP file entry")?;

            let content = std::fs::read(&py_file).context(format!("Failed to read file: {:?}", py_file))?;

            zip.write_all(&content).context("Failed to write to ZIP")?;
        }

        // Add schema.yaml if exists (included in hash so schema changes trigger updates)
        let schema_path = self.source_dir.join("schema.yaml");
        if schema_path.exists() {
            zip.start_file("schema.yaml", options).context("Failed to start schema.yaml entry")?;

            let content = std::fs::read(&schema_path).context("Failed to read schema.yaml")?;

            zip.write_all(&content).context("Failed to write schema.yaml")?;
        }

        // Add requirements.txt if exists
        let req_path = self.source_dir.join("requirements.txt");
        if req_path.exists() {
            zip.start_file("requirements.txt", options).context("Failed to start requirements.txt entry")?;

            let content = std::fs::read(&req_path).context("Failed to read requirements.txt")?;

            zip.write_all(&content).context("Failed to write requirements.txt")?;
        }

        // Add bundle-format (dynamically generated, not from source)
        zip.start_file("bundle-format", options).context("Failed to start bundle-format entry")?;

        zip.write_all(b"2.0.0\n").context("Failed to write bundle-format")?;

        zip.finish().context("Failed to finish ZIP")?;

        Ok(())
    }

    /// Find all Python files in source directory
    fn collect_python_files(&self) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        for entry in std::fs::read_dir(&self.source_dir).context("Failed to read source directory")? {
            let entry = entry.context("Failed to read directory entry")?;
            let path = entry.path();

            if path.is_file()
                && let Some(ext) = path.extension()
                && ext == "py"
            {
                files.push(path);
            }
        }

        Ok(files)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::lock_cwd;
    use std::path::{Path, PathBuf};

    /// Lay down the in-cwd scaffold shape `scaffold_config_in_cwd` produces for a python
    /// tool — `.wxctl-scaffold/<ref>/{schema.yaml,<module>.py,requirements.txt}` — under
    /// `cwd`, returning the cwd-relative source_dir.
    fn write_scaffold_shape(cwd: &Path, ref_name: &str, module: &str) -> PathBuf {
        let rel = PathBuf::from(".wxctl-scaffold").join(ref_name);
        let abs = cwd.join(&rel);
        std::fs::create_dir_all(&abs).unwrap();
        std::fs::write(abs.join("schema.yaml"), "input_schema:\n  type: object\noutput_schema:\n  type: object\n").unwrap();
        std::fs::write(abs.join(format!("{module}.py")), "def main():\n    return {}\n").unwrap();
        std::fs::write(abs.join("requirements.txt"), "").unwrap();
        rel
    }

    /// AC2: the python artifact builds from the rewritten cwd-relative source_dir with no
    /// path-traversal error, and load_schemas finds schema.yaml there.
    #[test]
    fn ac2_artifact_builds_from_rewritten_in_cwd_source_dir() {
        let _g = lock_cwd();
        let tmp = tempfile::tempdir().unwrap();
        // canonicalize so validate_path's canonical cwd comparison is exact on macOS (/var → /private/var).
        let cwd = std::fs::canonicalize(tmp.path()).unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(&cwd).unwrap();

        let source_dir = write_scaffold_shape(&cwd, "weather", "weather");

        // ArtifactBuilder::new calls validate_path → must NOT report "Path traversal" (dir is in-cwd).
        let hash = ArtifactBuilder::new(source_dir.clone()).and_then(|b| b.compute_source_hash());
        // load_schemas must find schema.yaml in the rewritten dir → no "schema.yaml not found".
        let schemas = super::super::load_schemas(&source_dir);

        std::env::set_current_dir(&prev).unwrap();

        let hash = hash.unwrap_or_else(|e| panic!("AC2: artifact must build from in-cwd source dir, got: {e:#}"));
        assert!(!hash.is_empty(), "AC2: artifact hash computed");
        let schemas = schemas.unwrap_or_else(|e| panic!("AC2: schema.yaml must be found in rewritten dir, got: {e:#}"));
        assert!(schemas.input_schema.is_object());
    }
}
