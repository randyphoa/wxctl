//! Process-wide registry of trusted config base directories.
//!
//! Relative `is_path` fields resolve against the config file's directory (the
//! documented contract), so the traversal guard in
//! `wxctl_providers::util::validate_path` must accept resolved paths under any
//! loaded config's directory — not only the process CWD. Config loaders (the
//! CLI's `load_configs_resolved`, the MCP server's `ConfigInput::load`) register
//! each source's base directory here at parse time; `validate_path` treats the
//! registered set (plus the CWD) as the allowed containment roots.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

static ALLOWED_ROOTS: OnceLock<Mutex<Vec<PathBuf>>> = OnceLock::new();

fn roots() -> &'static Mutex<Vec<PathBuf>> {
    ALLOWED_ROOTS.get_or_init(|| Mutex::new(Vec::new()))
}

/// Register a config source's base directory as a trusted containment root for
/// path validation. Canonicalizes the directory; silently ignores directories
/// that cannot be canonicalized (they cannot contain resolvable paths anyway).
pub fn allow_path_root(dir: &Path) {
    if let Ok(canonical) = std::fs::canonicalize(dir) {
        let mut roots = roots().lock().expect("allowed-roots lock poisoned");
        if !roots.contains(&canonical) {
            roots.push(canonical);
        }
    }
}

/// Snapshot of the registered containment roots (canonical, deduped).
pub fn allowed_path_roots() -> Vec<PathBuf> {
    roots().lock().expect("allowed-roots lock poisoned").clone()
}
