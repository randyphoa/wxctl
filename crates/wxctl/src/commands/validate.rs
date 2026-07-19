use super::common::{CommandContext, load_configs};
use crate::cli::{ComposeDeployment, OutputFormat};
use crate::output::sections::AdvisoryBlock;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::str::FromStr;
use wxctl_compose::extract_prompt_body;
use wxctl_engine::{Advisory, AnnotatedValidationError, ValidationError, ValidationPipeline, bridge_advisories};
use wxctl_schema::deployment::Deployment;

pub async fn execute(config_paths: &[String], fix_prompt: Option<&str>, output: Option<&OutputFormat>, skip_post_validate: bool, deployment: Option<ComposeDeployment>) -> Result<()> {
    // `-o json` owns stdout: quiet the collector so the header/stage panel can't
    // corrupt the machine-readable document.
    let render = !matches!(output, Some(OutputFormat::Json));
    let mut ctx = CommandContext::setup_with_render(config_paths, "validate", None, None, false, render)?;

    // Wrap the body so the run-record manifest is finalized on every exit path (JSON-bail,
    // fix-prompt, table) — the guard's Drop clears the sink slot before main can finalize,
    // so an unfinalized `validate` failure would otherwise leave `wxctl runs`/`wxctl debug`
    // without a record. `finalize_run` (not `finalize_run_result`) is used deliberately:
    // validate already owns its stdout in each mode (raw JSON, the fix prompt, or the table
    // summary rendered by `ctx.finish()`), so appending a styled failure footer would
    // corrupt JSON output and double-print the table summary.
    let outcome = async {
        let validator = ValidationPipeline::new(ctx.registry.clone(), ctx.client_factory.clone());
        let result = validator.validate(&ctx.operation_id, &mut ctx.config.resources, skip_post_validate).await?;

        // Attach bridge advisories (validate-surface only, per spec I2 — never inside
        // the shared ValidationPipeline, so plan/apply/destroy never see them). `dep`
        // absent (no `--deployment`) falls back to the conservative default: only
        // bridges active on every deployment flavor.
        let dep = deployment.and_then(|d| Deployment::from_str(d.as_deployment_str()).ok());
        let advisories = bridge_advisories(&result, dep.as_ref());
        let result = result.with_advisories(advisories);

        if let Some(OutputFormat::Json) = output {
            // On failure, assemble the same fix.md-template prompt the MCP `wxctl_validate`
            // tool returns (no original prompt); `.ok()` degrades a schema-render failure to
            // `fix_prompt: None` so the JSON document is always emitted. A valid config
            // passes `None`, so a valid result never carries a prompt.
            let fix_prompt = if result.is_valid() { None } else { assemble_fix_prompt(config_paths, result.errors(), result.advisories(), None).ok() };
            let out = wxctl_sdk::json::validate_output(&result, fix_prompt);
            println!("{}", serde_json::to_string_pretty(&out)?);
            if !result.is_valid() {
                anyhow::bail!("Validation failed");
            }
            return Ok(());
        }

        // If --fix-prompt and validation failed, output a fix prompt instead of the normal
        // summary. The prompt is the product here, so this path always exits 0.
        if fix_prompt.is_some() && !result.is_valid() {
            let original_prompt_path = fix_prompt.filter(|p| !p.is_empty());
            let prompt = assemble_fix_prompt(config_paths, result.errors(), result.advisories(), original_prompt_path)?;
            print!("{}", prompt);
            return Ok(());
        }

        // Table summary. `ctx.finish()` renders it (including the `▌ Errors` section and a
        // Failed footer when invalid); the trailing bail then makes the process exit
        // non-zero so CI gating on the exit code catches validation failures — previously
        // this path fell through to a 0 exit and reported a false pass.
        //
        // Feed advisories into the panel so `ctx.finish()` renders the `▌ Advisories`
        // section (populated above by the bridge-advisory scan; empty when no
        // orphaned bridge endpoint is present).
        let advisory_blocks: Vec<AdvisoryBlock> = result.advisories().iter().map(|a| AdvisoryBlock { code: a.code.clone(), resource: a.resource.clone(), message: a.message.clone(), suggestion: a.suggestion.clone() }).collect();
        ctx.lock_collector().set_advisories(advisory_blocks);
        ctx.finish()?;
        if !result.is_valid() {
            anyhow::bail!("Validation failed with {} error(s)", result.errors().len());
        }
        Ok(())
    }
    .await;

    ctx.finalize_run(if outcome.is_ok() { "success" } else { "failed" });
    outcome
}

fn format_error_list(errors: &[AnnotatedValidationError]) -> String {
    errors.iter().enumerate().map(|(i, e)| format!("{}. [{}] {}: {}. {}", i + 1, e.resource, e.error.field(), e.error, e.error.suggestion())).collect::<Vec<_>>().join("\n")
}

fn assemble_fix_prompt(config_paths: &[String], errors: &[AnnotatedValidationError], advisories: &[Advisory], original_prompt_path: Option<&str>) -> Result<String> {
    if let Some(path) = original_prompt_path {
        let original_prompt = std::fs::read_to_string(path).with_context(|| format!("Failed to read original prompt: {}", path))?;
        let config_content = load_configs(config_paths)?;
        let error_list = format_error_list(errors);

        return Ok(format!("{}\n\n---\n\nYour previous output had validation errors. Here was your output:\n\n```\n{}\n```\n\nErrors:\n{}\n\nFix these errors and output the corrected YAML.", original_prompt, config_content, error_list));
    }

    let template = wxctl_compose::templates::FIX;

    let config_content = load_configs(config_paths)?;
    let errors_text = format_error_list(errors);

    // Determine which resource kinds had errors (parse "kind/name" from e.resource),
    // plus every kind an add-resource suggestion names: the unresolved-reference
    // kind and each transitively-required chain kind. This widens the schema-doc
    // scope so the LLM sees the schema + References table of what it is about to add.
    let mut failing_kinds: HashSet<&str> = errors
        .iter()
        .filter_map(|e| {
            if e.resource.is_empty() {
                None
            } else {
                // resource format is "kind/name"
                e.resource.split('/').next()
            }
        })
        .collect();
    for e in errors {
        if let ValidationError::UnresolvedReference { ref_kind, required_chain, .. } = &e.error {
            failing_kinds.insert(ref_kind.as_str());
            for (kind, _, _) in required_chain {
                failing_kinds.insert(kind.as_str());
            }
        }
    }

    // Render schema docs for the failing kinds from the compiled schemas (an empty
    // set renders all kinds). Single source of truth — see `super::schema_doc`.
    let schema_ref = super::schema_doc::render_kinds_markdown(Some(&failing_kinds))?;

    // Extract prompt body (skip markdown documentation header) and replace placeholders
    let body = extract_prompt_body(template);
    let mut prompt = body.replace("<CONFIG>", &config_content).replace("<ERRORS>", &errors_text).replace("<SCHEMA_REFERENCE>", &schema_ref);

    if !advisories.is_empty() {
        prompt.push_str("\n\n## Advisories (non-blocking)\n\n");
        for a in advisories {
            prompt.push_str(&format!("- [{}] {}: {}. {}\n", a.code, a.resource, a.message, a.suggestion));
        }
    }

    Ok(prompt)
}
