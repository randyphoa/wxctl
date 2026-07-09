use anyhow::Result;
use std::path::{Path, PathBuf};

/// The wxctl config directory: `WXCTL_CONFIG_DIR` if set, else `~/.wxctl`.
///
/// The override exists because `dirs::home_dir` ignores `HOME` on Windows (it
/// asks the known-folder API), so a temp-`HOME` sandbox — used by the subprocess
/// E2E tests — cannot redirect the profiles/active-profile lookup without it.
/// Mirrors `WXCTL_UPDATE_CACHE_DIR` (update state) and `WXCTL_RUNS_DIR` (run
/// records). Returns `None` only when neither the override nor a home dir is
/// discoverable.
pub fn wxctl_config_dir() -> Option<PathBuf> {
    std::env::var_os("WXCTL_CONFIG_DIR").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".wxctl")))
}

/// Resolve the active profile name.
///
/// Precedence:
/// 1. Explicit `--profile` flag value (passed as `cli_profile`)
/// 2. `WXCTL_PROFILE` environment variable
/// 3. Contents of `~/.wxctl/active_profile` file
/// 4. `"default"` fallback
pub fn resolve_active_profile(cli_profile: Option<&str>) -> String {
    if let Some(name) = cli_profile {
        return name.to_string();
    }

    if let Ok(name) = std::env::var("WXCTL_PROFILE")
        && !name.trim().is_empty()
    {
        return name.trim().to_string();
    }

    if let Some(dir) = wxctl_config_dir() {
        let active_file = dir.join("active_profile");
        if let Ok(content) = std::fs::read_to_string(&active_file) {
            let name = content.trim().to_string();
            if !name.is_empty() {
                return name;
            }
        }
    }

    "default".to_string()
}

/// Parse a boolean environment variable: `true` when set to `1` or `true`
/// (case-insensitive), `false` when unset or any other value.
pub fn env_bool(name: &str) -> bool {
    std::env::var(name).map(|v| v == "1" || v.eq_ignore_ascii_case("true")).unwrap_or(false)
}

/// Return the path to the active_profile file.
pub fn active_profile_path() -> Result<PathBuf> {
    Ok(wxctl_config_dir().ok_or_else(|| anyhow::anyhow!("Could not determine home directory"))?.join("active_profile"))
}

/// Write `contents` to `path` with owner-only permissions, creating the parent
/// directory if needed.
///
/// The wxctl config file holds plaintext credentials (API keys, CP4D
/// passwords), so it must never be group- or world-readable. On Unix we create
/// the file `0600` from the outset (so it is never momentarily world-readable)
/// and re-tighten afterwards to repair a file written before this safeguard
/// existed; a freshly created parent directory is set `0700`. This mirrors the
/// AWS CLI (`~/.aws/credentials` is `0600`, `~/.aws` is `0700`) and the
/// kubectl/gcloud conventions. On non-Unix platforms the default per-user ACL
/// already scopes the file to its owner, so we just write it.
///
/// A parent directory that already exists is left as-is: callers may point
/// `--profile-path` at an arbitrary location, and silently re-permissioning a
/// pre-existing directory would be surprising. The `0600` file is the credential
/// boundary regardless of the directory mode.
pub fn write_credential_file(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let created = !parent.exists();
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        if created {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700))?;
        }
    }

    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        // `.mode()` only applies when the file is created; re-tighten afterwards
        // so a config written before this fix (typically 0644) gets repaired.
        let mut file = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(path)?;
        file.write_all(contents.as_bytes())?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, contents)?;
    }

    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn credential_file_is_owner_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("profiles.yaml");

        write_credential_file(&path, "{\"secret\":\"shh\"}").unwrap();

        let file_mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(file_mode, 0o600, "credential file must be 0600, got {file_mode:o}");

        let dir_mode = std::fs::metadata(path.parent().unwrap()).unwrap().permissions().mode() & 0o777;
        assert_eq!(dir_mode, 0o700, "freshly created parent dir must be 0700, got {dir_mode:o}");
    }

    #[test]
    fn rewrite_repairs_loose_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("profiles.yaml");

        // Simulate a config written before this safeguard: world-readable.
        std::fs::write(&path, "old").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        write_credential_file(&path, "new").unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "rewrite must tighten an existing loose file, got {mode:o}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }
}
