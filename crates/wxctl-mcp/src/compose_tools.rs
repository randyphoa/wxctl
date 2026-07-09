//! Compose-pipeline MCP tool DTOs + backing logic. Thin wrappers over the
//! `wxctl-compose` core — no logic here beyond input shaping + object-root output
//! wrapping. Pure compute / FS; no profile, no network. `compose_scaffold` is the
//! only FS-writing tool (registered only when not `--read-only`).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use wxctl_compose::scaffold::{config_from_yaml_raw, config_to_multidoc_yaml, rewrite_config_paths_in_cwd, scaffold_config_in_cwd};
use wxctl_compose::{PathsInput, assemble_config_prompt, assemble_implementation_prompt, assemble_test_prompt, resolve_paths, scaffold_config, tool_descriptions_from_yaml};
use wxctl_compose_core::{ComposeRecipe, assemble_recipe};
use wxctl_core::Config;

// ── start (orchestrator) ────────────────────────────────────────────────────────
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComposeStartInput {
    /// Natural-language use-case description.
    pub use_case: String,
    /// Target deployment (`saas`|`software`). Default `saas`. Reserved for future
    /// deployment-scoped recipe variation; accepted + validated today.
    #[serde(default = "default_saas")]
    pub deployment: String,
    /// Optional tier ceiling for the returned `recipe`: `config` returns only the five
    /// pure-compute authoring steps (identify…generate_tests); `deploy` (or omitted)
    /// returns the full nine-step recipe. Lets a config-only caller request just the
    /// authoring slice it can run, without the deploy-tier steps. Omit it for the default
    /// full recipe (the identical recipe every surface returns).
    #[serde(default)]
    pub max_tier: Option<String>,
}

/// MCP-facing mirror of `wxctl_compose_core::RecipeStep` — identical fields + `JsonSchema`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct RecipeStepDto {
    pub n: u32,
    pub name: String,
    pub tier: String,
    pub action: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gate: Option<String>,
}

/// MCP-facing mirror of `wxctl_compose_core::FixLoop` — identical fields + `JsonSchema`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct FixLoopDto {
    pub max_iterations: u32,
    pub policy: String,
}

/// MCP-facing mirror of `wxctl_compose_core::Clarification` — identical fields + `JsonSchema`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct ClarificationDto {
    pub policy: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ComposeStartOutput {
    /// Ordered recipe steps (each tagged `tier`): identify → paths → generate → validate_fix
    /// → generate_tests (config tier) → scaffold → plan → apply → test (deploy tier). When
    /// `max_tier: config` is requested, only the five config-tier steps are returned.
    pub recipe: Vec<RecipeStepDto>,
    /// Ready-to-run Pass-1 identification prompt (contains the use-case text).
    pub identify_prompt: String,
    /// Bounded fix loop policy (`max_iterations: 3`).
    pub fix_loop: FixLoopDto,
    /// Gate strings (the error-free-plan-before-apply precondition, etc.).
    pub gates: Vec<String>,
    /// Clarification policy: how to handle a `kind: clarification_request` from generate.
    pub clarification: ClarificationDto,
}

pub fn compose_start(input: &ComposeStartInput) -> Result<ComposeStartOutput, String> {
    // Validate the deployment string (reserved for future recipe variation; unused beyond validation today).
    crate::tools::parse_deployment(&input.deployment)?;
    if input.use_case.trim().is_empty() {
        return Err("use_case must not be empty".to_string());
    }
    let ComposeRecipe { identify_prompt, steps, fix_loop, gates, clarification } = assemble_recipe(&input.use_case).map_err(|e| format!("{e:#}"))?.with_max_tier(input.max_tier.as_deref()).map_err(|e| format!("{e:#}"))?;
    let recipe = steps.into_iter().map(|s| RecipeStepDto { n: s.n, name: s.name, tier: s.tier, action: s.action, tool: s.tool, gate: s.gate }).collect();
    let fix_loop = FixLoopDto { max_iterations: fix_loop.max_iterations, policy: fix_loop.policy };
    let clarification = ClarificationDto { policy: clarification.policy };
    Ok(ComposeStartOutput { recipe, identify_prompt, fix_loop, gates, clarification })
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PromptOutput {
    /// The assembled LLM prompt text.
    pub prompt: String,
}

// ── paths ─────────────────────────────────────────────────────────────────────
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComposePathsInput {
    /// Resource-list or partial-config YAML (one or more `---` docs).
    pub config: String,
    /// Target deployment for bridge activation: `saas` or `software`. Default `saas`.
    #[serde(default = "default_saas")]
    pub deployment: String,
}

fn default_saas() -> String {
    "saas".to_string()
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct PathsOutputDto {
    /// The `compose/v1` resolved-paths YAML.
    pub paths_yaml: String,
}

pub fn compose_paths(input: &ComposePathsInput) -> Result<PathsOutputDto, String> {
    // `parse_deployment` maps "saas"|"software" → the concrete deployment string
    // `Deployment::from_str` accepts ("saas"|"software-5.3.0"); plain "software" is not concrete.
    let deployment = crate::tools::parse_deployment(&input.deployment)?;
    let paths_yaml = resolve_paths(PathsInput { content: &input.config, deployment }).map_err(|e| format!("{e:#}"))?;
    Ok(PathsOutputDto { paths_yaml })
}

// ── prompt (config / implementation / test) ─────────────────────────────────────
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComposePromptInput {
    /// Use-case text (config + test modes) / implementation context (implementation mode).
    #[serde(default)]
    pub input: Option<String>,
    /// `compose/v1` paths YAML → config-generation prompt (scoped schema reference).
    #[serde(default)]
    pub paths: Option<String>,
    /// Pre-rendered "files already exist" block injected verbatim into the config prompt
    /// (config mode only). The caller renders it (e.g. from a resources directory); this
    /// tool performs no filesystem scan. Default: none (empty block).
    #[serde(default)]
    pub existing_resources: Option<String>,
    /// Scaffold directory → implementation prompt (Pass 4).
    #[serde(default)]
    pub scaffold_dir: Option<String>,
    /// Config YAML → test-generation prompt; or, with `scaffold_dir`, the tool
    /// `ref_name`→`description` join for the implementation prompt.
    #[serde(default)]
    pub config: Option<String>,
    /// Treat `config` as the test-generation config (test mode).
    #[serde(default)]
    pub test_config: bool,
    /// Treat `config` as the data-generation config (data mode): detect data needs and
    /// return the generic data-generation prompt. `input` is the use-case text.
    #[serde(default)]
    pub data_config: bool,
}

pub fn compose_prompt(input: &ComposePromptInput) -> Result<PromptOutput, String> {
    // implementation mode
    if let Some(dir) = &input.scaffold_dir {
        // ref_name→description join for the implementation prompt; shared parse loop in wxctl-compose.
        let descriptions = input.config.as_deref().map(tool_descriptions_from_yaml).unwrap_or_default();
        let ctx = input.input.clone().unwrap_or_default();
        let prompt = assemble_implementation_prompt(dir, &ctx, &descriptions).map_err(|e| format!("{e:#}"))?;
        return Ok(PromptOutput { prompt });
    }
    // test mode
    if input.test_config {
        let config_yaml = input.config.as_deref().ok_or("test mode requires `config` (the config YAML)")?;
        let prompt = assemble_test_prompt(config_yaml, input.input.as_deref().unwrap_or("")).map_err(|e| format!("{e:#}"))?;
        return Ok(PromptOutput { prompt });
    }
    // data mode
    if input.data_config {
        let config_yaml = input.config.as_deref().ok_or("data mode requires `config` (the config YAML)")?;
        let resources = wxctl_compose_core::parse_resources(config_yaml).map_err(|e| format!("{e:#}"))?;
        let needs = wxctl_compose_core::detect_data_needs(&resources);
        let prompt = wxctl_compose_core::assemble_data_prompt(input.input.as_deref().unwrap_or(""), config_yaml, &needs).map_err(|e| format!("{e:#}"))?;
        return Ok(PromptOutput { prompt });
    }
    // config mode (with or without scoped paths)
    let user_input = input.input.as_deref().ok_or("config mode requires `input` (use-case text)")?;
    let paths_yaml = input.paths.as_deref().unwrap_or("");
    let existing_resources = input.existing_resources.as_deref().unwrap_or("");
    let prompt = assemble_config_prompt(user_input, paths_yaml, existing_resources).map_err(|e| format!("{e:#}"))?;
    Ok(PromptOutput { prompt })
}

// ── scaffold ────────────────────────────────────────────────────────────────────
#[derive(Debug, Deserialize, JsonSchema)]
pub struct ComposeScaffoldInput {
    /// Config YAML whose referenced files to materialize.
    pub config: String,
    /// Directory to write the stub files into. **Omit it** (recommended) to write into
    /// the canonical in-cwd dir `<cwd>/.wxctl-scaffold/<ref_name>/` and get back a
    /// `config` whose source-path fields point there (use that returned config for
    /// plan/apply/test). If set, it must resolve **inside** the working directory; the
    /// stub file names are rebased under it (legacy CLI-parity behavior).
    #[serde(default)]
    pub output_dir: Option<String>,
    /// Print the manifest without writing any file (default false).
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct ScaffoldOutputDto {
    /// Human-readable manifest (created / skipped / failed per path).
    pub manifest: String,
    /// True if any entry failed (the CLI exits non-zero on this).
    pub failed: bool,
    /// (created, skipped, failed) counts.
    pub created: usize,
    pub skipped: usize,
    pub failed_count: usize,
    /// The config YAML with source-path fields rewritten to the canonical in-cwd
    /// scaffold locations. Use this for downstream plan/apply/test. Empty string when
    /// an explicit `output_dir` was supplied (legacy rebase path performs no rewrite).
    pub config: String,
    /// True when there is no rewritten `config` to adopt — keep using the config you passed
    /// in. This holds when scaffold materialized no source files (the config references no
    /// source-bearing resources, so the returned `config` is identical to the input), and
    /// always in the explicit-`output_dir` (legacy) mode, which rebases files by name and
    /// returns an empty `config`. False means source paths were rewritten and downstream
    /// steps (plan/apply/test) must use the returned `config`, not the pre-scaffold one.
    pub config_unchanged: bool,
}

pub fn compose_scaffold(input: &ComposeScaffoldInput) -> Result<ScaffoldOutputDto, String> {
    let config = Config::from_yaml(&input.config).map_err(|e| format!("config parse error: {e:#}"))?;
    let cwd = std::env::current_dir().map_err(|e| format!("cannot resolve working directory: {e}"))?;
    // Canonicalize cwd to resolve platform symlinks (e.g. /var → /private/var on macOS).
    let cwd_canon = cwd.canonicalize().unwrap_or(cwd.clone());
    match &input.output_dir {
        // Explicit output_dir: must be inside cwd; preserves the legacy rebase-by-filename behavior.
        Some(dir) => {
            let joined = if Path::new(dir).is_absolute() { PathBuf::from(dir) } else { cwd.join(dir) };
            // Canonicalize the existing prefix of the resolved path, then lexically normalize any
            // trailing non-existent segments. This handles platform symlinks (/var vs /private/var).
            let resolved = canonicalize_prefix(&joined);
            if !resolved.starts_with(&cwd_canon) {
                return Err(format!("output_dir '{dir}' resolves outside the working directory {} — scaffold must write inside cwd", cwd.display()));
            }
            let out = scaffold_config(&config, Some(dir), Path::new(dir), input.dry_run);
            let (created, skipped, failed_count) = out.manifest.counts();
            // Legacy explicit-output_dir mode returns no rewritten config → the caller keeps its own.
            Ok(ScaffoldOutputDto { manifest: out.manifest.render(), failed: out.manifest.any_failed(), config: String::new(), created, skipped, failed_count, config_unchanged: true })
        }
        // Default: canonical in-cwd rewrite; return the path-consistent config.
        None => {
            // On-disk scaffold uses the interpolated `config` (real values where present).
            let (out, _rewritten) = scaffold_config_in_cwd(&config, &cwd, input.dry_run);
            let (created, skipped, failed_count) = out.manifest.counts();
            // The RETURNED config is derived from a NON-interpolated parse so `${env:...}` literals
            // survive round-trip — never echo a resolved secret back to the client (wxctl_validate /
            // wxctl_plan never do). Apply the same canonical path rewrites the on-disk scaffold used.
            let raw = config_from_yaml_raw(&input.config).map_err(|e| format!("config parse error: {e:#}"))?;
            let rewritten_raw = rewrite_config_paths_in_cwd(&raw);
            let config_yaml = config_to_multidoc_yaml(&rewritten_raw).map_err(|e| format!("serialize rewritten config: {e:#}"))?;
            // Empty manifest ⇒ no source-bearing resources ⇒ no path was rewritten ⇒ config is unchanged.
            let config_unchanged = created == 0 && skipped == 0 && failed_count == 0;
            Ok(ScaffoldOutputDto { manifest: out.manifest.render(), failed: out.manifest.any_failed(), config: config_yaml, created, skipped, failed_count, config_unchanged })
        }
    }
}

/// Canonicalize as much of `path` as possible: walk from the full path up to the first ancestor
/// that exists, canonicalize it, then re-append the remaining non-existent tail lexically
/// (collapsing `.` and `..`). This lets us do security prefix-checks on paths that may not
/// exist yet without forfeiting platform-symlink resolution on the parts that do exist.
fn canonicalize_prefix(path: &Path) -> PathBuf {
    // Find the longest existing ancestor.
    let mut existing = path.to_path_buf();
    let mut tail = vec![];
    loop {
        if existing.exists() {
            break;
        }
        match existing.file_name() {
            Some(name) => {
                tail.push(name.to_os_string());
                existing = existing.parent().map(PathBuf::from).unwrap_or_default();
            }
            None => break,
        }
    }
    let base = existing.canonicalize().unwrap_or(existing);
    // Re-append tail in reverse (we pushed from deepest to shallowest).
    let mut result = base;
    for component in tail.into_iter().rev() {
        // Skip `.`; collapse `..` by popping.
        if component == "." {
            continue;
        } else if component == ".." {
            result.pop();
        } else {
            result.push(component);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_returns_recipe_and_identify_prompt() {
        let out = compose_start(&ComposeStartInput { use_case: "HR chatbot".to_string(), deployment: "saas".to_string(), max_tier: None }).unwrap();
        let names: Vec<&str> = out.recipe.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["identify", "paths", "generate", "validate_fix", "generate_tests", "scaffold", "plan", "apply", "test"]);
        let tiers: Vec<&str> = out.recipe.iter().map(|s| s.tier.as_str()).collect();
        assert_eq!(tiers, ["config", "config", "config", "config", "config", "deploy", "deploy", "deploy", "deploy"]);
        assert_eq!(out.fix_loop.max_iterations, 3);
        assert!(out.gates.iter().any(|g| g.contains("error-free") || g.contains("plan must be error-free")));
        assert!(out.clarification.policy.contains("clarification_request"));
        assert!(out.identify_prompt.contains("HR chatbot"));
    }

    #[test]
    fn start_max_tier_config_returns_only_the_authoring_steps() {
        let out = compose_start(&ComposeStartInput { use_case: "HR chatbot".to_string(), deployment: "saas".to_string(), max_tier: Some("config".to_string()) }).unwrap();
        let names: Vec<&str> = out.recipe.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["identify", "paths", "generate", "validate_fix", "generate_tests"]);
        assert!(out.recipe.iter().all(|s| s.tier == "config"));
        // Cross-cutting policy is still returned in the filtered view.
        assert_eq!(out.fix_loop.max_iterations, 3);
        assert!(!out.gates.is_empty());
        // `deploy` and omitted are the full recipe; a bogus tier errors.
        assert_eq!(compose_start(&ComposeStartInput { use_case: "x".to_string(), deployment: "saas".to_string(), max_tier: Some("deploy".to_string()) }).unwrap().recipe.len(), 9);
        assert!(compose_start(&ComposeStartInput { use_case: "x".to_string(), deployment: "saas".to_string(), max_tier: Some("bogus".to_string()) }).unwrap_err().contains("config, deploy"));
    }

    #[test]
    fn start_rejects_empty_use_case_and_bad_deployment() {
        assert!(compose_start(&ComposeStartInput { use_case: "  ".to_string(), deployment: "saas".to_string(), max_tier: None }).is_err());
        assert!(compose_start(&ComposeStartInput { use_case: "x".to_string(), deployment: "onprem".to_string(), max_tier: None }).unwrap_err().contains("saas"));
    }

    #[test]
    fn paths_rejects_unknown_deployment_and_emits_compose_v1() {
        let cfg = "resources:\n  - kind: agent\n";
        // Bad deployment → error naming the valid values.
        let err = compose_paths(&ComposePathsInput { config: cfg.to_string(), deployment: "onprem".to_string() }).unwrap_err();
        assert!(err.contains("saas") && err.contains("software"));
        // Valid deployment → emits a compose/v1 paths document.
        let out = compose_paths(&ComposePathsInput { config: cfg.to_string(), deployment: "saas".to_string() }).unwrap();
        assert!(out.paths_yaml.contains("format: compose/v1"));
    }

    use crate::test_support::lock_cwd;

    #[test]
    fn scaffold_explicit_output_dir_still_rebases_by_filename() {
        let _g = lock_cwd();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::current_dir().unwrap();
        // Set cwd to tmp so the explicit output_dir (a subdir of tmp) is within cwd.
        std::env::set_current_dir(tmp.path()).unwrap();
        let out_dir = tmp.path().join("out");
        std::fs::create_dir_all(&out_dir).unwrap();
        let result = compose_scaffold(&ComposeScaffoldInput { config: "kind: wml_function\nref_name: f\nsource_path: score.py\n".to_string(), output_dir: Some(out_dir.to_string_lossy().into_owned()), dry_run: false });
        std::env::set_current_dir(&prev).unwrap();
        let out = result.unwrap();
        assert!(!out.failed);
        assert!(out_dir.join("score.py").exists());
        assert!(out.config.is_empty(), "explicit output_dir performs no rewrite");
        assert!(out.config_unchanged, "legacy explicit-output_dir mode returns no config to adopt");
    }

    #[test]
    fn scaffold_code_free_config_signals_config_unchanged() {
        let _g = lock_cwd();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        // An agent has no source-bearing fields → nothing to materialize.
        let result = compose_scaffold(&ComposeScaffoldInput { config: "kind: agent\nref_name: a\nname: A\ndescription: d\ninstructions: hi\n".to_string(), output_dir: None, dry_run: false });
        std::env::set_current_dir(&prev).unwrap();
        let out = result.unwrap();
        assert!(!out.failed, "{}", out.manifest);
        assert_eq!((out.created, out.skipped, out.failed_count), (0, 0, 0), "no files for a code-free config");
        assert!(out.config_unchanged, "code-free config is unchanged — proceed with the original");
    }

    #[test]
    fn scaffold_default_writes_in_cwd_and_returns_rewritten_config() {
        let _g = lock_cwd();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        let result = compose_scaffold(&ComposeScaffoldInput { config: "kind: tool\nref_name: weather\nsource_path: x\ninput_schema:\n  type: object\nbinding:\n  python:\n    function: weather:main\n".to_string(), output_dir: None, dry_run: false });
        std::env::set_current_dir(&prev).unwrap();
        let out = result.unwrap();
        assert!(!out.failed, "{}", out.manifest);
        assert!(out.config.contains(".wxctl-scaffold/weather"), "returned config carries the canonical source_path: {}", out.config);
        assert!(!out.config_unchanged, "a source-bearing config was rewritten — use the returned config");
        assert!(tmp.path().join(".wxctl-scaffold/weather/schema.yaml").exists());
    }

    #[test]
    fn scaffold_returns_env_literals_not_resolved_secrets() {
        // The default (in-cwd) branch must return `${env:...}` literals verbatim, never the
        // resolved secret — even though the env var is set (so it COULD leak). Redaction parity
        // with wxctl_validate / wxctl_plan, which never echo interpolated values.
        let _g = lock_cwd();
        let tmp = tempfile::tempdir().unwrap();
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(tmp.path()).unwrap();
        // SAFETY: serialized by lock_cwd; the var name is unique to this test and removed below.
        unsafe { std::env::set_var("WXCTL_TEST_SCAFFOLD_SECRET", "supersecret-value") };
        let config = "kind: agent\nref_name: a\nname: A\ndescription: uses ${env:WXCTL_TEST_SCAFFOLD_SECRET}\ninstructions: hi\n";
        let result = compose_scaffold(&ComposeScaffoldInput { config: config.to_string(), output_dir: None, dry_run: false });
        unsafe { std::env::remove_var("WXCTL_TEST_SCAFFOLD_SECRET") };
        std::env::set_current_dir(&prev).unwrap();
        let out = result.unwrap();
        assert!(out.config.contains("${env:WXCTL_TEST_SCAFFOLD_SECRET}"), "returned config must preserve the env literal, got: {}", out.config);
        assert!(!out.config.contains("supersecret-value"), "returned config must NOT echo the resolved secret: {}", out.config);
    }

    #[test]
    fn scaffold_output_dir_outside_cwd_errors_and_writes_nothing() {
        let _g = lock_cwd();
        let cwd_tmp = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_path = outside.path().join("outside");
        let prev = std::env::current_dir().unwrap();
        std::env::set_current_dir(cwd_tmp.path()).unwrap();
        let result = compose_scaffold(&ComposeScaffoldInput { config: "kind: wml_function\nref_name: f\nsource_path: score.py\n".to_string(), output_dir: Some(outside_path.to_string_lossy().into_owned()), dry_run: false });
        std::env::set_current_dir(&prev).unwrap();
        let err = result.unwrap_err();
        assert!(err.contains("working directory"), "error names the cwd constraint: {err}");
        assert!(!outside_path.join("score.py").exists(), "nothing written outside cwd");
    }
}
