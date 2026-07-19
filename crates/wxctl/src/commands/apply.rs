use super::common::{CommandContext, handle_execution_results};
use super::progress_observer::CliProgressObserver;
use crate::cli::OutputFormat;
use anyhow::Result;
use std::sync::Arc;
use wxctl_engine::{DagExecutor, ExecutorConfig, OperationType, Pipeline, RuntimeIdStore};

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, full_trace: bool, output: Option<&OutputFormat>) -> Result<()> {
    let json = matches!(output, Some(OutputFormat::Json));
    let mut ctx = CommandContext::setup_with_render(config_paths, "apply", Some(profile), profile_path, full_trace, !json)?;
    let outcome = async {
        let client_factory = ctx.client_factory.clone().expect("Client factory required for apply");

        let pipeline = Pipeline::new(ctx.registry.clone(), client_factory.clone());
        // One observer for both the reconciliation stage (live counter, Phase 2) and the
        // execution stage. Built before reconciliation so the reconcile callbacks reach it.
        let observer = Arc::new(CliProgressObserver::new(ctx.collector.clone()));
        let (operation_id, plan, graph) = pipeline.plan_for_apply_with(&mut ctx.config, true, observer.clone()).await?;

        let advisories = plan.advisories.clone();
        ctx.lock_collector().print_plan();
        let advisory_blocks: Vec<crate::output::sections::AdvisoryBlock> = advisories.iter().map(|a| crate::output::sections::AdvisoryBlock { code: a.code.clone(), resource: a.resource.clone(), message: a.message.clone(), suggestion: a.suggestion.clone() }).collect();
        ctx.lock_collector().set_advisories(advisory_blocks);

        // Execution with the same CLI observer
        let executor_config = ExecutorConfig::new(ctx.concurrency_config.global_limit, ctx.concurrency_config.default_timeout_secs);
        let executor = DagExecutor::with_observer(ctx.registry.clone(), client_factory, executor_config, observer);

        // Ctrl-C cancels the executor token instead of killing the process, so the
        // run record lists what actually completed (A8b).
        let (cancel, ctrl_c_listener) = super::common::spawn_ctrl_c_cancel();
        let results = executor.execute_with_cancel(&operation_id, plan.operations, &graph, cancel, false, RuntimeIdStore::new()).await?;
        ctrl_c_listener.abort();

        if json {
            // Emit the DTO to stdout FIRST, then run the standard result handler which
            // logs per-resource error events into the run record and returns Err on any
            // failure — so an agent always gets structured `failed[]` detail before the
            // nonzero exit. `ctx._run_id` is the run-record id (matches wxctl-mcp's
            // `scope.run_id`), so the emitted `run_id` feeds `wxctl debug` / `run_diagnose`.
            let mut out = wxctl_sdk::json::execute_output(ctx._run_id.clone(), &results, false);
            out.advisories = advisories;
            println!("{}", serde_json::to_string_pretty(&out)?);
            handle_execution_results(&ctx.operation_id, &results, "Execution")?;
            return Ok(());
        }

        // Extract URLs for created resources so the summary can surface them.
        for succeeded in &results.succeeded {
            if let Some(response) = &succeeded.response
                && matches!(succeeded.operation, OperationType::Create)
                && let Some(url) = extract_resource_url(response)
            {
                let resource_name = format!("{}.{}", succeeded.key.kind, succeeded.key.name);
                ctx.lock_collector().add_resource_url(resource_name, url);
            }
        }

        handle_execution_results(&ctx.operation_id, &results, "Execution")?;
        ctx.finish()
    }
    .await;
    ctx.finalize_run_result(&outcome);
    outcome
}

/// Extract resource URL from API response
fn extract_resource_url(response: &serde_json::Value) -> Option<String> {
    if let Some(url) = super::common::first_string_field(response, &["url", "endpoint", "uri", "href", "link", "self", "web_url", "html_url"]) {
        return Some(url);
    }

    // Try nested _links.self.href (HATEOAS pattern)
    if let Some(links) = response.get("_links")
        && let Some(self_link) = links.get("self")
        && let Some(href) = self_link.get("href").and_then(|v| v.as_str())
    {
        return Some(href.to_string());
    }

    None
}
