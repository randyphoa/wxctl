use super::common::{CommandContext, handle_execution_results};
use super::progress_observer::CliProgressObserver;
use anyhow::Result;
use std::sync::Arc;
use wxctl_engine::{DagExecutor, ExecutorConfig, OperationType, Pipeline, RuntimeIdStore};

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, full_trace: bool) -> Result<()> {
    let mut ctx = CommandContext::setup(config_paths, "apply", Some(profile), profile_path, full_trace)?;
    let outcome = async {
        let client_factory = ctx.client_factory.clone().expect("Client factory required for apply");

        let pipeline = Pipeline::new(ctx.registry.clone(), client_factory.clone());
        // One observer for both the reconciliation stage (live counter, Phase 2) and the
        // execution stage. Built before reconciliation so the reconcile callbacks reach it.
        let observer = Arc::new(CliProgressObserver::new(ctx.collector.clone()));
        let (operation_id, plan, graph) = pipeline.plan_for_apply_with(&mut ctx.config, true, observer.clone()).await?;

        ctx.lock_collector().print_plan();

        // Execution with the same CLI observer
        let executor_config = ExecutorConfig::new(ctx.concurrency_config.global_limit, ctx.concurrency_config.default_timeout_secs);
        let executor = DagExecutor::with_observer(ctx.registry.clone(), client_factory, executor_config, observer);

        let results = executor.execute(&operation_id, plan.operations, &graph).await?;

        // Cache results and extract URLs for created resources
        let runtime_cache = RuntimeIdStore::new();
        for succeeded in &results.succeeded {
            if let Some(response) = &succeeded.response {
                runtime_cache.insert(succeeded.key.clone(), response.clone());

                if matches!(succeeded.operation, OperationType::Create)
                    && let Some(url) = extract_resource_url(response)
                {
                    let resource_name = format!("{}.{}", succeeded.key.kind, succeeded.key.name);
                    ctx.lock_collector().add_resource_url(resource_name, url);
                }
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
