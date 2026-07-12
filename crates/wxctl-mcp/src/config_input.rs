//! Shared config-input handling for the live tools (`validate`, `plan`, and the
//! Phase 3 mutating tools). Accepts exactly one of `config` (inline YAML) or
//! `config_path` (filesystem path), returns a parsed `Config`, and applies the
//! same relative-`PATH_FIELDS` semantics the CLI uses:
//!
//! - `config_path` → read the file, `Config::from_yaml`, then resolve relative
//!   path fields against the file's directory via the shared
//!   [`wxctl_providers::resolve_file_paths`] (the same function the CLI uses).
//! - `config` (inline) → `Config::from_yaml`, then resolve relative path fields in
//!   `PATH_FIELDS` against the current working directory (`std::env::current_dir()`).
//!   cwd is exactly the base `compose_scaffold` writes its cwd-relative source paths
//!   against, so a scaffolded config flows inline with no path rewrite. Inline
//!   absolute paths pass through unchanged; `validate_path` remains the downstream
//!   traversal guard for upload kinds.
//!
//! Both branches drive the build-generated `wxctl_providers::PATH_FIELDS` table
//! `(kind, field_name, parent_array_field)`, so adding a path field upstream needs
//! no edit here.

use std::path::Path;

use schemars::JsonSchema;
use serde::Deserialize;
use wxctl_core::Config;

/// Inline `config` XOR `config_path`. Shared by every live tool's input.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ConfigInput {
    /// Inline YAML config (one or more `---`-separated documents). Relative paths in
    /// path-bearing fields (e.g. `source_path`, `documents[].path`, `spec_path`,
    /// `server_path`, `import_file`) resolve against the current working directory.
    #[serde(default)]
    pub config: Option<String>,
    /// Filesystem path to a YAML config file. Relative path-bearing fields resolve
    /// against this file's directory, identical to `wxctl -f <file>`.
    #[serde(default)]
    pub config_path: Option<String>,
}

impl ConfigInput {
    /// Config-path list for the run manifest: the file path when loaded from a
    /// file, else a single `"inline"` marker (inline YAML has no path).
    pub fn scope_paths(&self) -> Vec<String> {
        match &self.config_path {
            Some(p) => vec![p.clone()],
            None => vec!["inline".to_string()],
        }
    }

    /// Resolve to a parsed `Config`, applying the path semantics above. `Err(String)`
    /// is an already-formatted, agent-readable message (becomes an `isError` result).
    pub fn load(&self) -> Result<Config, String> {
        match (&self.config, &self.config_path) {
            (Some(_), Some(_)) | (None, None) => Err("provide exactly one of `config` (inline YAML) or `config_path` (file path)".to_string()),
            (Some(inline), None) => {
                let mut config = Config::from_yaml(inline).map_err(|e| format!("config parse error: {e:#}"))?;
                let cwd = std::env::current_dir().map_err(|e| format!("could not determine current directory for inline config path resolution: {e}"))?;
                wxctl_providers::resolve_file_paths(&mut config, &cwd);
                Ok(config)
            }
            (None, Some(path)) => {
                let path = Path::new(path);
                let content = std::fs::read_to_string(path).map_err(|e| format!("could not read config_path '{}': {e}", path.display()))?;
                let mut config = Config::from_yaml(&content).map_err(|e| format!("config parse error: {e:#}"))?;
                let dir = path.parent().unwrap_or_else(|| Path::new("."));
                // Trust the config file's directory for the providers-side traversal
                // guard — resolved paths live under it, not necessarily under the CWD.
                wxctl_core::paths::allow_path_root(dir);
                wxctl_providers::resolve_file_paths(&mut config, dir);
                Ok(config)
            }
        }
    }

    /// Like [`load`](Self::load) but drops `kind: test` documents — the deployment
    /// lifecycle tools (`wxctl_validate` / `wxctl_plan` / `wxctl_apply` / `wxctl_destroy`)
    /// operate on real resources only, matching the CLI, which filters tests once for
    /// every command in `CommandContext::setup` (`commands/common.rs`:
    /// `config.resources.retain(|r| r.kind != "test")`). The compose recipe deliberately
    /// appends the generated `kind: test` suite to the same `config.yaml`
    /// (recipe step 5 `generate_tests`), so a driver naturally feeds a test-bearing config
    /// to these tools; without this filter every call rejected it with
    /// `Unknown resource type: test`. `wxctl_test` keeps calling the unfiltered [`load`](Self::load)
    /// because it consumes that test suite.
    pub fn load_deployable(&self) -> Result<Config, String> {
        let mut config = self.load()?;
        config.resources.retain(|r| r.kind != "test");
        Ok(config)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_not_exactly_one_input() {
        // Both `config` and `config_path` set, or neither set, both violate the XOR rule.
        let both = ConfigInput { config: Some("x: 1".to_string()), config_path: Some("/tmp/x.yaml".to_string()) };
        let neither = ConfigInput { config: None, config_path: None };
        for (label, input) in [("both", both), ("neither", neither)] {
            let err = input.load().unwrap_err();
            assert!(err.contains("exactly one"), "{label} names the XOR rule: {err}");
        }
    }

    #[test]
    fn inline_parses_config_without_path_fields() {
        let input = ConfigInput { config: Some("kind: s3_bucket\nname: b1\n".to_string()), config_path: None };
        let config = input.load().expect("inline parses");
        assert_eq!(config.resources.len(), 1);
        assert_eq!(config.resources[0].kind, "s3_bucket");
    }

    #[test]
    fn inline_path_field_resolution() {
        // `package_extension.source_path` is an `is_path` field (present in PATH_FIELDS,
        // verified via the CLI's resolve_file_paths tests). A relative inline value resolves
        // against the cwd; an absolute value passes through unchanged (no regression).
        // Hold the shared CWD lock: this reads `current_dir()` here and again inside
        // `load()`, so a parallel test mutating the process CWD would clobber it between.
        let _g = crate::test_support::lock_cwd();
        let abs = if cfg!(windows) { "C:\\opt\\local.zip" } else { "/opt/local.zip" };
        let expected_rel = std::env::current_dir().unwrap().join("local.zip").to_string_lossy().into_owned();
        // (label, source_path value, expected resolved value)
        let cases: &[(&str, String, String)] = &[("relative→cwd", "local.zip".to_string(), expected_rel), ("absolute→unchanged", abs.to_string(), abs.to_string())];
        for (label, value, expected) in cases {
            let yaml = format!("kind: package_extension\nname: pe1\nsource_path: {value}\n");
            let input = ConfigInput { config: Some(yaml), config_path: None };
            let config = input.load().expect("inline parses");
            let resolved = config.resources[0].data.get("source_path").and_then(|v| v.as_str()).unwrap();
            assert_eq!(resolved, expected, "{label}: {resolved}");
            assert!(Path::new(resolved).is_absolute(), "{label} resolves to an absolute path: {resolved}");
        }
    }

    #[test]
    fn load_deployable_drops_kind_test_documents() {
        // The compose recipe appends a `kind: test` suite to the same config; the
        // deployment lifecycle tools (validate/plan/apply/destroy) must filter it like
        // the CLI does, or every call fails with `Unknown resource type: test`.
        let yaml = "kind: agent\nref_name: a1\n---\nkind: test\nref_name: t1\nagent: ${agent.a1}\nturns:\n  - message: hi\n    expect_answer: yo\n";
        let input = ConfigInput { config: Some(yaml.to_string()), config_path: None };
        let all = input.load().expect("raw load parses");
        assert_eq!(all.resources.len(), 2, "raw load keeps both docs");
        let deployable = input.load_deployable().expect("load_deployable parses");
        assert_eq!(deployable.resources.len(), 1, "load_deployable drops the kind:test doc");
        assert_eq!(deployable.resources[0].kind, "agent", "the real resource survives");
    }

    #[test]
    fn config_path_resolves_relative_path_field() {
        let dir = std::env::temp_dir().join(format!("wxctl-mcp-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("cfg.yaml");
        std::fs::write(&file, "kind: package_extension\nname: pe1\nsource_path: local.zip\n").unwrap();
        let input = ConfigInput { config: None, config_path: Some(file.to_string_lossy().into_owned()) };
        let config = input.load().expect("file parses");
        let resolved = config.resources[0].data.get("source_path").and_then(|v| v.as_str()).unwrap();
        assert!(Path::new(resolved).is_absolute(), "resolved against file dir: {resolved}");
        assert!(resolved.ends_with("local.zip"), "keeps the basename: {resolved}");
        std::fs::remove_dir_all(&dir).ok();
    }
}
