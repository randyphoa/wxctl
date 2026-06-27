use super::common::{CommandContext, handle_execution_results};
use super::progress_observer::CliProgressObserver;
use anyhow::Result;
use std::sync::Arc;
use wxctl_engine::{DagExecutor, ExecutorConfig, Pipeline};

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, full_trace: bool) -> Result<()> {
    let mut ctx = CommandContext::setup(config_paths, "destroy", Some(profile), profile_path, full_trace)?;
    let outcome = async {
        let client_factory = ctx.client_factory.clone().expect("Client factory required for destroy");

        let pipeline = Pipeline::new(ctx.registry.clone(), client_factory.clone());
        // One observer for both the reconciliation stage (live counter, Phase 2) and the
        // execution stage. Built before reconciliation so the reconcile callbacks reach it.
        let observer = Arc::new(CliProgressObserver::new(ctx.collector.clone()));
        let (operation_id, plan, filtered_graph, seed) = pipeline.plan_for_destroy_with(&mut ctx.config, observer.clone()).await?;

        ctx.lock_collector().print_plan();

        let executor_config = ExecutorConfig::new(ctx.concurrency_config.global_limit, ctx.concurrency_config.default_timeout_secs);
        let executor = DagExecutor::with_observer(ctx.registry.clone(), client_factory, executor_config, observer);

        let results = executor.execute_destroy_seeded(&operation_id, plan.operations, &filtered_graph, seed).await?;

        handle_execution_results(&ctx.operation_id, &results, "Destroy")?;
        ctx.finish()
    }
    .await;
    ctx.finalize_run_result(&outcome);
    outcome
}
