use crate::output::color::Theme;
use crate::output::{CollectorGuard, OutputCollector, RunSinkGuard, install_collector, install_run_sink, set_active_run_deployment, set_full_trace};
use anyhow::{Result, bail};
use parking_lot::Mutex;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;
use uuid::Uuid;
use wxctl_core::logging::run_record::{RunCounts, RunManifest, RunSink, generate_run_id, utc_now_string};
use wxctl_core::types::Deployment;
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
/// priority order. Shared by the apply URL/id extractors. Re-export of
/// [`wxctl_core::first_string_field`] — the single home for id/field extraction.
pub(crate) use wxctl_core::first_string_field;

impl CommandContext {
    /// Set up command context with registry, output collector, and optional client factory.
    /// `render: false` puts the collector in
    /// quiet mode before anything prints — for machine-readable output (`-o json`)
    /// where the command's own document must be the only stdout. Run records, the
    /// registry, and error collection behave identically.
    pub fn setup_with_render(config_paths: &[String], operation_name: &str, profile: Option<&str>, profile_path: Option<&str>, full_trace: bool, render: bool) -> Result<Self> {
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

        // Load, parse, and resolve configuration from all sources. `load_configs_resolved`
        // resolves each source's relative `is_path` fields against that source's OWN
        // directory before merging, so repeatable `-f` honors the documented "relative
        // paths resolve against the config file's directory" contract. For errors that
        // carry a WXCTL error-code prefix (e.g. WXCTL-V301 env interpolation, WXCTL-V302
        // malformed expression), emit a structured error event so the run record indexes
        // them by their real code (not just the WXCTL-E000 wrapper that main emits). The
        // sink is already installed above; finalize it before returning so the manifest is
        // written (the guard's Drop clears the slot before main's finalize_active_run fires).
        let mut config = load_configs_resolved(config_paths).inspect_err(|e| {
            let msg = e.to_string();
            // Only emit a structured error event when the message carries a real WXCTL-<code>:
            // prefix so the run record indexes it under the correct code. Non-prefixed errors
            // (e.g. plain YAML syntax / file-read failures) are reported by main's generic
            // wrapper; emitting a structured event with a guessed code here would produce a
            // misleading artifact.
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

        // Set up registry. Errors here (bad schema load / registration) and the client
        // factory below (a missing or malformed profile is a common runtime failure) land
        // after the sink is installed but before the outcome is inspected, so finalize a
        // "failed" manifest on the way out — otherwise `wxctl runs`/`wxctl debug` see no
        // record for exactly the failures they exist to diagnose.
        let mut registry = ResourceRegistry::new();
        for ir in wxctl_schema::ir::RESOURCE_IR.values().copied() {
            let handler = wxctl_providers::get_handler(ir.resource.name);
            // Per-kind custom reconcilers first (e.g. asset_promotion); the generic
            // schema-driven reconciler covers everything else.
            registry.register_from_schema(ir, handler, |ir| wxctl_providers::get_reconciler(ir.resource.name).unwrap_or_else(|| Arc::new(SchemaBasedReconciler::new()))).inspect_err(|_| run_sink.finalize("failed"))?;
        }
        let registry = Arc::new(registry);

        // Load concurrency config from environment (shared between client factory and executor)
        let concurrency_config = ConcurrencyConfig::from_env();

        // Create client factory if profile is provided
        let client_factory = if let Some(profile) = profile { Some(Arc::new(ClientFactory::new(profile, profile_path, &concurrency_config).inspect_err(|_| run_sink.finalize("failed"))?)) } else { None };

        // Record the run's deployment scope now that the profile is resolved (the
        // manifest predates profile load, so this lands late but before finalize).
        // Profile-level effective value, defaulting to `Saas` the same way
        // `ClientFactory::deployment_for_service` does when the profile omits it;
        // per-service overrides (cross-env profiles) are not distinguished here.
        // Credential-free commands construct no factory, so no deployment is recorded.
        if let Some(cf) = &client_factory {
            let deployment = cf.profile_deployment().ok().flatten().unwrap_or(Deployment::Saas);
            set_active_run_deployment(Some(deployment.flavor().to_string()));
        }

        // Resolve color theme. The collector panel draws to stderr, so gate
        // auto-detection on stderr's TTY (keeps a colored panel on screen even
        // when stdout is redirected).
        let color_pref = wxctl_core::load_color_preference(profile_path);
        let theme = Theme::resolve_for_stderr(color_pref.as_deref());

        // Create output collector
        let operation_id = Uuid::new_v4().to_string();
        let collector = Arc::new(Mutex::new(OutputCollector::new(operation_id.clone(), theme)));
        if !render {
            collector.lock().set_quiet();
        }

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
            // The command returned Err — mark it so the footer renders Failed even when the
            // failure surfaced no per-resource error event (e.g. a pre-execution executor
            // abort whose only signal is the dropped WXCTL-E000 rollup).
            c.mark_command_failed();
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

/// Graceful Ctrl-C for the mutating commands. The first SIGINT cancels the DAG
/// executor's token — `execute_with_cancel` then returns the operations that
/// actually completed (results accumulate outside the collect future), so the
/// run record finalizes with real counts instead of vanishing. A second SIGINT
/// finalizes the run record directly and hard-exits 130. Command-scoped on
/// purpose: `wxctl mcp serve`'s stdio lifecycle is owned by the rmcp framework
/// and must not see a competing global signal handler.
pub fn spawn_ctrl_c_cancel() -> (tokio_util::sync::CancellationToken, tokio::task::JoinHandle<()>) {
    let cancel = tokio_util::sync::CancellationToken::new();
    let token = cancel.clone();
    let listener = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n^C — cancelling; in-flight operations finish and the run record is kept (Ctrl-C again to force quit)");
            token.cancel();
            if tokio::signal::ctrl_c().await.is_ok() {
                crate::output::finalize_active_run("aborted");
                std::process::exit(130);
            }
        }
    });
    (cancel, listener)
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

/// Load and parse every config source, resolving each source's relative `is_path`
/// fields against that source's OWN directory before merging into one `Config`.
///
/// This is the path-correct counterpart to `load_configs` + a single `resolve_file_paths`
/// pass: with repeatable `-f`, every file's relative paths resolve against the file's
/// location (the documented "relative paths resolve against the config file's directory"
/// contract), instead of all files resolving against the first path's parent. Each source's
/// base directory:
/// - a file → its parent directory (its own directory when the path is bare);
/// - a directory → the directory itself (all its `.yaml`/`.yml` files share it);
/// - stdin (`-`) → the current working directory (stdin has no location).
///
/// `${env:VAR}` interpolation runs per source in `Config::from_yaml`, identically to the
/// single-string path. `kind: test` filtering is left to the caller (`wxctl test` keeps
/// them; every other command drops them).
pub fn load_configs_resolved(paths: &[String]) -> Result<Config> {
    // Start from an empty Config and append each resolved source's resources — avoids
    // naming `RawResource` here while keeping the merge order stable (source order).
    let mut merged = Config::from_yaml("")?;
    for path in paths {
        let (content, base_dir): (String, Option<PathBuf>) = if path == "-" {
            use std::io::Read;
            let mut buf = String::new();
            std::io::stdin().read_to_string(&mut buf).map_err(|e| anyhow::anyhow!("Failed to read from stdin: {}", e))?;
            (buf, std::env::current_dir().ok())
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
                let mut parts = Vec::new();
                for entry in entries {
                    let file_path = entry.path();
                    let content = std::fs::read_to_string(&file_path).map_err(|e| anyhow::anyhow!("Failed to read '{}': {}", file_path.display(), e))?;
                    parts.push(content);
                }
                (parts.join("\n---\n"), Some(canonical_dir(Path::new(path))))
            } else {
                let content = std::fs::read_to_string(path).map_err(|e| anyhow::anyhow!("Failed to read config file '{}': {}", path, e))?;
                (content, Path::new(path).parent().map(canonical_dir))
            }
        };
        let mut config = Config::from_yaml(&content)?;
        if let Some(dir) = &base_dir {
            // Trust this source's directory for the providers-side traversal guard —
            // the resolved paths below live under it, not necessarily under the CWD.
            wxctl_core::paths::allow_path_root(dir);
            resolve_file_paths(&mut config, dir);
        }
        merged.resources.append(&mut config.resources);
    }
    Ok(merged)
}

/// Canonicalize a config source's base directory to an absolute path, falling back to
/// the raw path joined with the CWD when canonicalization fails (e.g. a bare filename
/// whose parent is `""` → the CWD).
fn canonical_dir(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap_or_else(|_| std::env::current_dir().unwrap_or_default().join(dir))
}

/// Resolve relative file paths in config resources to absolute paths. Re-export
/// of [`wxctl_providers::resolve_file_paths`] — the single home shared with the
/// local MCP server. The `allow_path_root` registration stays at the call site.
pub(crate) use wxctl_providers::resolve_file_paths;

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
        // Use a platform-absolute base: a Unix-style "/tmp/cell" is not absolute on
        // Windows (no drive prefix), which would break the is_absolute() check.
        let config_dir = std::env::temp_dir().join("cell");
        for (yaml, field, tail) in cases {
            let mut config = Config::from_yaml(yaml).unwrap();
            resolve_file_paths(&mut config, &config_dir);
            let resolved = config.resources[0].data.get(*field).and_then(|v| v.as_str()).unwrap();
            let resolved_path = Path::new(resolved);
            assert!(resolved_path.is_absolute(), "{field} should be absolute, got {resolved}");
            assert!(resolved_path.starts_with(&config_dir), "{field} should be under config_dir, got {resolved}");
            // Component-based check is separator-agnostic (tail uses '/').
            assert!(resolved_path.ends_with(tail), "{field} tail should be preserved, got {resolved}");
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

    /// With repeatable `-f`, each file's relative `is_path` fields resolve against that
    /// file's OWN directory — not the first path's parent (the multi-`-f` contract fix).
    #[test]
    fn load_configs_resolved_rebases_each_file_against_its_own_dir() {
        let base = std::env::temp_dir().join(format!("wxctl-loadcfg-{}", std::process::id()));
        let dir_a = base.join("alpha");
        let dir_b = base.join("beta");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();
        let file_a = dir_a.join("tk_a.yaml");
        let file_b = dir_b.join("tk_b.yaml");
        std::fs::write(&file_a, "kind: toolkit\nref_name: tka\nserver_path: ./servers/a\n").unwrap();
        std::fs::write(&file_b, "kind: toolkit\nref_name: tkb\nserver_path: ./servers/b\n").unwrap();

        let paths = vec![file_a.to_string_lossy().into_owned(), file_b.to_string_lossy().into_owned()];
        let config = load_configs_resolved(&paths).unwrap();
        // Source order is preserved: file_a's resource first, file_b's second.
        let a = config.resources[0].data.get("server_path").and_then(|v| v.as_str()).unwrap();
        let b = config.resources[1].data.get("server_path").and_then(|v| v.as_str()).unwrap();
        // Component-based ends_with is separator-agnostic (Windows resolves with '\').
        assert!(Path::new(a).starts_with(canonical_dir(&dir_a)) && Path::new(a).ends_with("servers/a"), "file A resolves against dir alpha, got {a}");
        assert!(Path::new(b).starts_with(canonical_dir(&dir_b)) && Path::new(b).ends_with("servers/b"), "file B resolves against its own dir beta (not alpha), got {b}");
        let _ = std::fs::remove_dir_all(&base);
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
