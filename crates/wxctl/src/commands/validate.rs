use super::common::{CommandContext, load_configs};
use crate::cli::OutputFormat;
use anyhow::{Context, Result};
use std::collections::HashSet;
use wxctl_compose::extract_prompt_body;
use wxctl_engine::{AnnotatedValidationError, ValidationPipeline};

pub async fn execute(config_paths: &[String], fix_prompt: Option<&str>, output: Option<&OutputFormat>, skip_post_validate: bool) -> Result<()> {
    let mut ctx = CommandContext::setup(config_paths, "validate", None, None, false)?;

    let validator = ValidationPipeline::new(ctx.registry.clone(), ctx.client_factory.clone());
    let result = validator.validate(&ctx.operation_id, &mut ctx.config.resources, skip_post_validate).await?;

    if let Some(OutputFormat::Json) = output {
        let json_output = serde_json::json!({
            "valid": result.is_valid(),
            "errors": result.errors()
        });
        println!("{}", serde_json::to_string_pretty(&json_output)?);
        if !result.is_valid() {
            anyhow::bail!("Validation failed");
        }
        return Ok(());
    }

    // If --fix-prompt and validation failed, output a fix prompt instead of the normal summary
    if fix_prompt.is_some() && !result.is_valid() {
        let original_prompt_path = fix_prompt.filter(|p| !p.is_empty());
        let prompt = assemble_fix_prompt(config_paths, result.errors(), original_prompt_path)?;
        print!("{}", prompt);
        return Ok(());
    }

    ctx.finish()
}

fn format_error_list(errors: &[AnnotatedValidationError]) -> String {
    errors.iter().enumerate().map(|(i, e)| format!("{}. [{}] {}: {}. {}", i + 1, e.resource, e.error.field(), e.error, e.error.suggestion())).collect::<Vec<_>>().join("\n")
}

fn assemble_fix_prompt(config_paths: &[String], errors: &[AnnotatedValidationError], original_prompt_path: Option<&str>) -> Result<String> {
    if let Some(path) = original_prompt_path {
        let original_prompt = std::fs::read_to_string(path).with_context(|| format!("Failed to read original prompt: {}", path))?;
        let config_content = load_configs(config_paths)?;
        let error_list = format_error_list(errors);

        return Ok(format!("{}\n\n---\n\nYour previous output had validation errors. Here was your output:\n\n```\n{}\n```\n\nErrors:\n{}\n\nFix these errors and output the corrected YAML.", original_prompt, config_content, error_list));
    }

    let template = wxctl_compose::templates::FIX;

    let config_content = load_configs(config_paths)?;
    let errors_text = format_error_list(errors);

    // Determine which resource kinds had errors (parse "kind/name" from e.resource)
    let failing_kinds: HashSet<&str> = errors
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

    // Render schema docs for the failing kinds from the compiled schemas (an empty
    // set renders all kinds). Single source of truth — see `super::schema_doc`.
    let schema_ref = super::schema_doc::render_kinds_markdown(Some(&failing_kinds))?;

    // Extract prompt body (skip markdown documentation header) and replace placeholders
    let body = extract_prompt_body(template);
    let prompt = body.replace("<CONFIG>", &config_content).replace("<ERRORS>", &errors_text).replace("<SCHEMA_REFERENCE>", &schema_ref);

    Ok(prompt)
}
