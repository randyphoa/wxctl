use crate::output::color::Theme;
use crate::output::{CollectorGuard, OutputCollector, RunSinkGuard, install_collector, install_run_sink, set_full_trace};
use anyhow::{Result, bail};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;
use wxctl_core::logging::run_record::{RunCounts, RunManifest, RunSink, generate_run_id, utc_now_string};
use wxctl_core::{ClientFactory, ConcurrencyConfig, Config, ResourceRegistry};
use wxctl_engine::{ExecutionResults, OperationType, SchemaBasedReconciler};

/// Command context containing shared resources for command execution
pub struct CommandContext {
    pub config: Config,
    pub registry: Arc<ResourceRegistry>,
    pub client_factory: Option<Arc<ClientFactory>>,
    pub concurrency_config: ConcurrencyConfig,
    pub operation_id: String,
    pub collector: Arc<Mutex<OutputCollector>>,
    pub start_time: Instant,
    pub(crate) _guard: CollectorGuard,
    command_name: String,
    pub _run_id: String,
    pub _full_trace: bool,
    pub(crate) run_sink: Arc<RunSink>,
    pub(crate) _run_span: tracing::span::EnteredSpan,
    pub(crate) _run_guard: RunSinkGuard,
}

/// First present, non-empty string value among `fields` in `response`, tried in
/// priority order. Shared by the apply URL/id extractors.
pub(crate) fn first_string_field(response: &serde_json::Value, fields: &[&str]) -> Option<String> {
    fields.iter().find_map(|field| response.get(field).and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(str::to_string))
}

impl CommandContext {
    /// Set up command context with registry, output collector, and optional client factory
    pub fn setup(config_paths: &[String], operation_name: &str, profile: Option<&str>, profile_path: Option<&str>, full_trace: bool) -> Result<Self> {
        // Effective full_trace: CLI flag OR WXCTL_FULL_TRACE=1/"true". Must be resolved
        // before the manifest is built so the artifact records the correct value even when
        // the command fails before reaching the flag-application code below.
        let full_trace = full_trace || crate::config::env_bool("WXCTL_FULL_TRACE");
        set_full_trace(full_trace);

        // Run record: installed FIRST so that pre-config errors (e.g. WXCTL-V301 env
        // interpolation) are captured and diagnosable via `wxctl debug`. The same sink
        // is used for the entire command lifetime — no second sink is created.
        let run_id = generate_run_id(operation_name);
        let manifest = RunManifest {
            run_id: run_id.clone(),
            command: operation_name.to_string(),
            args: std::env::args().skip(1).collect(),
            profile: profile.map(str::to_string),
            deployment: None,
            config_paths: config_paths.to_vec(),
            started: utc_now_string(),
            finished: None,
            outcome: None,
            counts: RunCounts::default(),
            errors: Vec::new(),
            full_trace,
            record_incomplete: false,
        };
        let run_sink = Arc::new(RunSink::new(manifest).unwrap_or_else(RunSink::null));
        let _run_guard = install_run_sink(run_sink.clone());

        // Load and merge configuration from all files. For errors that carry a WXCTL
        // error-code prefix (e.g. WXCTL-V301 env interpolation, WXCTL-V302 malformed
        // expression), emit a structured error event so the run record indexes them by
        // their real code (not just the WXCTL-E000 wrapper that main emits). The sink is
        // already installed above; finalize it before returning so the manifest is written
        // (the guard's Drop clears the slot before main's finalize_active_run fires).
        let content = load_configs(config_paths).inspect_err(|_| {
            run_sink.finalize("failed");
        })?;
        let mut config = Config::from_yaml(&content).inspect_err(|e| {
            let msg = e.to_string();
            // Only emit a structured error event when the message carries a real WXCTL-<code>:
            // prefix so the run record indexes it under the correct code. Non-prefixed errors
            // (e.g. plain YAML syntax failures) are reported by main's generic wrapper; emitting
            // a structured event with a guessed code here would produce a misleading artifact.
            if let Some(stripped) = msg.strip_prefix("WXCTL-") {
                let end = stripped.find(':').unwrap_or(0);
                if end > 0 {
                    let code = format!("WXCTL-{}", &stripped[..end]);
                    let fix = if stripped[..end].starts_with('V') { "check that all ${env:VAR} references in the config are set and non-empty" } else { "review the config for schema or expression errors" };
                    tracing::error!(target: "wxctl::error", stage = "config", error_code = %code, message = %msg, fix = fix, "config loading failed");
                }
            }
            run_sink.finalize("failed");
        })?;

        // Filter out `kind: test` resources — they are only used by `wxctl test`
        config.resources.retain(|r| r.kind != "test");

        // Resolve relative file paths in resources relative to the config file directory.
        // This follows industry convention (Terraform, Docker Compose, etc.) where paths
        // in config files are relative to the file's location, not the working directory.
        if let Some(config_dir) = resolve_config_dir(config_paths) {
            resolve_file_paths(&mut config, &config_dir);
        }

        // Set up registry
        let mut registry = ResourceRegistry::new();
        let schemas = wxctl_providers::load_all_schemas()?;
        for schema in schemas {
            let handler = wxctl_providers::get_handler(&schema.resource.name);
            registry.register_from_schema(schema, handler, |_| Arc::new(SchemaBasedReconciler::new()))?;
        }
        let registry = Arc::new(registry);

        // Load concurrency config from environment (shared between client factory and executor)
        let concurrency_config = ConcurrencyConfig::from_env();

        // Create client factory if profile is provided
        let client_factory = if let Some(profile) = profile { Some(Arc::new(ClientFactory::new(profile, profile_path, &concurrency_config)?)) } else { None };

        // Resolve color theme
        let color_pref = wxctl_core::load_color_preference(profile_path);
        let theme = Theme::resolve(color_pref.as_deref());

        // Create output collector
        let operation_id = Uuid::new_v4().to_string();
        let collector = Arc::new(Mutex::new(OutputCollector::new(operation_id.clone(), theme)));

        let guard = install_collector(collector.clone());

        // Determine stage count per command
        let stage_count = match (operation_name, profile.is_some()) {
            ("apply", true) => 4,   // validation, reconciliation, planning, execution
            ("destroy", true) => 4, // validation, reconciliation, planning, execution
            (_, true) => 3,         // validation, reconciliation, planning
            _ => 1,                 // validation only
        };

        // Print header with profile and config context
        {
            let mut c = collector.lock();
            let profile_display = client_factory.as_ref().map(|cf| cf.profile_name());
            c.set_stage_count(stage_count);
            c.set_has_execution_stage(operation_name == "apply" || operation_name == "destroy");
            c.set_run_id(run_id.clone());
            c.set_command(operation_name.to_string(), config_paths.join(", "));
            c.print_header(operation_name, config.resources.len(), profile_display, config_paths);
        }

        // Root `run` span. Opened here rather than at run-sink creation so spans from
        // registry/factory setup (above) are associated with the command root span.
        let run_span = tracing::info_span!(target: "wxctl::stage::run", "run", run_id = %run_id, command = %operation_name, operation_id = %operation_id, profile = profile.unwrap_or("none"), full_trace).entered();

        Ok(Self { config, registry, client_factory, concurrency_config, operation_id, collector, start_time: Instant::now(), _guard: guard, command_name: operation_name.to_string(), _run_id: run_id, _full_trace: full_trace, run_sink, _run_span: run_span, _run_guard })
    }

    /// Finalize the run-record manifest with a definite outcome. Called by each
    /// command's `execute` on both Ok and Err paths before returning.
    pub fn finalize_run(&self, outcome: &str) {
        self.run_sink.finalize(outcome);
    }

    /// Finalize with outcome derived from a command result, emitting the
    /// structured error_chain event into the run record while the sink is live.
    /// For commands that short-circuit via `?` before reaching `finish()` (plan-style
    /// validation bails and execution-path failures alike), this also renders the
    /// `▌ Errors` section + failure footer so the screen is never left without a summary.
    pub fn finalize_run_result<T>(&self, outcome: &Result<T>) {
        if let Err(e) = outcome {
            let chain = wxctl_core::error_chain_vec(e);
            tracing::error!(target: "wxctl::error", stage = "command", error_code = "WXCTL-E000", message = %e, fix = "see error_chain for the full context chain", error_chain = %serde_json::to_string(&chain).unwrap_or_default(), "Command failed");
            let mut c = self.collector.lock();
            c.set_duration(self.start_time.elapsed().as_millis() as u64);
            c.print_summary(&self.command_name);
        }
        self.finalize_run(if outcome.is_ok() { "success" } else { "failed" });
    }

    /// Print summary and clean up
    pub fn finish(&self) -> Result<()> {
        let total_duration = self.start_time.elapsed().as_millis() as u64;
        let mut c = self.collector.lock();
        c.set_duration(total_duration);
        c.print_summary(&self.command_name);
        Ok(())
    }

    /// Lock the collector for printing
    pub fn lock_collector(&self) -> parking_lot::MutexGuard<'_, OutputCollector> {
        self.collector.lock()
    }
}

/// Handle execution results with proper error messages
pub fn handle_execution_results(operation_id: &str, results: &ExecutionResults, action: &str) -> Result<()> {
    if !results.skipped.is_empty() {
        tracing::warn!(
            target: "wxctl::substage::execution",
            operation_id = %operation_id,
            category = "skipped_resources",
            count = results.skipped.len(),
            reason = "failed_dependencies",
            "resources skipped due to failed dependencies"
        );
    }

    if results.cancelled {
        bail!("{} was cancelled", action);
    }

    if !results.failed.is_empty() {
        for result in &results.failed {
            let code = match result.operation {
                OperationType::Create | OperationType::Recreate => wxctl_core::logging::error_codes::E001,
                OperationType::Update { .. } => wxctl_core::logging::error_codes::E002,
                OperationType::Delete => wxctl_core::logging::error_codes::E003,
                OperationType::NoOp | OperationType::Retain | OperationType::Skip { .. } => wxctl_core::logging::error_codes::E001,
            };
            wxctl_core::log_error_resource!(operation_id, "execution", code, &result.key.kind, &result.key.name, result.error.as_deref().unwrap_or("Unknown error"), "Review the error, correct the resource configuration, and retry");
        }

        bail!("{} failed with {} errors", action, results.failed.len());
    }

    Ok(())
}

/// Load and concatenate config sources, joining them with YAML document separators.
///
/// Each path can be:
/// - `-` to read from stdin
/// - A directory (reads all `.yaml` and `.yml` files, sorted)
/// - A file path
pub fn load_configs(paths: &[String]) -> Result<String> {
    let mut parts = Vec::new();
    for path in paths {
        if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).map_err(|e| anyhow::anyhow!("Failed to read from stdin: {}", e))?;
            parts.push(buf);
        } else {
            let meta = std::fs::metadata(path).map_err(|e| anyhow::anyhow!("Failed to access '{}': {}", path, e))?;
            if meta.is_dir() {
                let mut entries: Vec<_> = std::fs::read_dir(path)
                    .map_err(|e| anyhow::anyhow!("Failed to read directory '{}': {}", path, e))?
                    .filter_map(|entry| entry.ok())
                    .filter(|entry| {
                        let name = entry.file_name();
                        let name = name.to_string_lossy();
                        name.ends_with(".yaml") || name.ends_with(".yml")
                    })
                    .collect();
                entries.sort_by_key(|e| e.file_name());
                if entries.is_empty() {
                    bail!("No .yaml or .yml files found in directory '{}'", path);
                }
                for entry in entries {
                    let file_path = entry.path();
                    let content = std::fs::read_to_string(&file_path).map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", file_path.display(), e))?;
                    parts.push(content);
                }
            } else {
                let content = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read config file '{}': {}", path, e))?;
                parts.push(content);
            }
        }
    }
    Ok(parts.join("\n---\n"))
}

/// Determine the config directory from config paths.
///
/// Uses the parent directory of the first non-stdin config path.
/// For directory paths, uses the directory itself.
pub(crate) fn resolve_config_dir(config_paths: &[String]) -> Option<PathBuf> {
    for path in config_paths {
        if path == "-" {
            continue;
        }
        let p = Path::new(path);
        let dir = if p.is_dir() { p } else { p.parent()? };
        // Canonicalize to get absolute path; fall back to the raw path joined with CWD
        return Some(std::fs::canonicalize(dir).unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(dir)));
    }
    // When all inputs are stdin, fall back to the current working directory
    std::env::current_dir().ok()
}

/// Resolve relative file paths in config resources to absolute paths.
///
/// Path fields are schema-declared (`is_path: true`) and surfaced via the
/// build-generated `wxctl_providers::PATH_FIELDS` table — adding a path field
/// needs only `is_path: true` on the schema, never an edit here.
pub(crate) fn resolve_file_paths(config: &mut Config, config_dir: &Path) {
    for resource in &mut config.resources {
        for &(kind, field_name, parent_array) in wxctl_providers::PATH_FIELDS {
            if resource.kind != kind {
                continue;
            }
            match parent_array {
                None => {
                    if let Some(val) = resource.data.get_mut(field_name) {
                        resolve_path_value(val, config_dir);
                    }
                }
                Some(arr_field) => {
                    if let Some(items) = resource.data.get_mut(arr_field).and_then(|v| v.as_array_mut()) {
                        for item in items {
                            if let Some(obj) = item.as_object_mut() {
                                if let Some(val) = obj.get_mut(field_name) {
                                    resolve_path_value(val, config_dir);
                                }
                            } else {
                                // Bare-string array item (e.g. documents: ["file.pdf"]).
                                resolve_path_value(item, config_dir);
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Resolve a single path value relative to config_dir if it's a relative path string.
fn resolve_path_value(value: &mut serde_json::Value, config_dir: &Path) {
    if let Some(path_str) = value.as_str() {
        let p = Path::new(path_str);
        if p.is_relative() {
            let resolved = config_dir.join(p);
            *value = serde_json::Value::String(resolved.to_string_lossy().into_owned());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `resolve_file_paths` rebases a relative scalar path field against the config
    /// dir (not the process CWD) for every `is_path` scalar field — a regression
    /// magnet, since a field missing `is_path` silently canonicalizes against CWD
    /// and fails with `os error 2`/ENOENT before any API call. Covers `server_path`
    /// (toolkit MCP), `spec_path` (OpenAPI tool; `flow_path` shares this path),
    /// `import_file` (business_terms), and `package_extension` `source_path`.
    #[test]
    fn resolve_file_paths_rebases_relative_scalar_fields() {
        let cases: &[(&str, &str, &str)] = &[
            ("kind: toolkit\nref_name: tk\nserver_path: ./servers/python\n", "server_path", "servers/python"),
            ("kind: tool\nref_name: echo\nspec_path: ./tools/echo-api/openapi.yaml\nbinding:\n  openapi:\n    tools: [\"*\"]\n", "spec_path", "tools/echo-api/openapi.yaml"),
            ("kind: business_terms\nref_name: bt\nimport_file: ./terms.csv\n", "import_file", "terms.csv"),
            ("kind: package_extension\nref_name: pe\ntype: conda_yml\nsource_path: env_e2e.yaml\nspace_id: s\n", "source_path", "env_e2e.yaml"),
        ];
        for (yaml, field, tail) in cases {
            let mut config = Config::from_yaml(yaml).unwrap();
            resolve_file_paths(&mut config, Path::new("/tmp/cell"));
            let resolved = config.resources[0].data.get(*field).and_then(|v| v.as_str()).unwrap();
            assert!(Path::new(resolved).is_absolute(), "{field} should be absolute, got {resolved}");
            assert!(resolved.starts_with("/tmp/cell"), "{field} should be under config_dir, got {resolved}");
            assert!(resolved.ends_with(tail), "{field} tail should be preserved, got {resolved}");
        }
    }

    /// An already-absolute scalar path is left untouched (no double-rebase).
    #[test]
    fn resolve_file_paths_leaves_absolute_scalar_fields_untouched() {
        let cases: &[(&str, &str, &str)] =
            &[("kind: toolkit\nref_name: tk\nserver_path: /abs/servers/node\n", "server_path", "/abs/servers/node"), ("kind: tool\nref_name: echo\nspec_path: /abs/tools/echo-api/openapi.yaml\nbinding:\n  openapi:\n    tools: [\"*\"]\n", "spec_path", "/abs/tools/echo-api/openapi.yaml")];
        for (yaml, field, expect) in cases {
            let mut config = Config::from_yaml(yaml).unwrap();
            resolve_file_paths(&mut config, Path::new("/tmp/cell"));
            let resolved = config.resources[0].data.get(*field).and_then(|v| v.as_str()).unwrap();
            assert_eq!(resolved, *expect, "{field}");
        }
    }

    /// Array-of-objects path fields (`knowledge_base.documents[].path`) rebase each
    /// relative entry under the config dir while leaving absolute entries untouched.
    #[test]
    fn resolve_file_paths_resolves_knowledge_base_documents() {
        let mut config = Config::from_yaml("kind: knowledge_base\nref_name: kb\ndocuments:\n  - path: ./docs/a.pdf\n  - path: /abs/b.pdf\n").unwrap();
        resolve_file_paths(&mut config, Path::new("/tmp/cell"));
        let docs = config.resources[0].data.get("documents").and_then(|v| v.as_array()).unwrap();
        let a = docs[0].get("path").and_then(|v| v.as_str()).unwrap();
        assert!(a.starts_with("/tmp/cell") && a.ends_with("docs/a.pdf"), "relative doc path resolved under config_dir, got {a}");
        let b = docs[1].get("path").and_then(|v| v.as_str()).unwrap();
        assert_eq!(b, "/abs/b.pdf", "absolute doc path untouched");
    }
}
