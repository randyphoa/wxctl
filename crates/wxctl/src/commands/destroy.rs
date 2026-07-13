use super::common::{CommandContext, handle_execution_results};
use super::progress_observer::CliProgressObserver;
use crate::cli::OutputFormat;
use anyhow::Result;
use std::sync::Arc;
use wxctl_engine::{DagExecutor, ExecutorConfig, Pipeline};

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, full_trace: bool, output: Option<&OutputFormat>) -> Result<()> {
    let json = matches!(output, Some(OutputFormat::Json));
    let mut ctx = CommandContext::setup_with_render(config_paths, "destroy", Some(profile), profile_path, full_trace, !json)?;
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

        // Ctrl-C cancels the executor token instead of killing the process, so the
        // run record lists what actually completed (A8b). for_destroy=true + the
        // reconciliation-seeded runtime store mirror execute_destroy_seeded.
        let (cancel, ctrl_c_listener) = super::common::spawn_ctrl_c_cancel();
        let results = executor.execute_with_cancel(&operation_id, plan.operations, &filtered_graph, cancel, true, seed).await?;
        ctrl_c_listener.abort();

        if json {
            // Emit the DTO first, then the result handler (logs error events + Err on
            // failure). `ctx._run_id` = run-record id (matches wxctl-mcp); feeds wxctl debug.
            let out = wxctl_sdk::json::execute_output(ctx._run_id.clone(), &results, false);
            println!("{}", serde_json::to_string_pretty(&out)?);
            handle_execution_results(&ctx.operation_id, &results, "Destroy")?;
            return Ok(());
        }

        handle_execution_results(&ctx.operation_id, &results, "Destroy")?;
        ctx.finish()
    }
    .await;
    ctx.finalize_run_result(&outcome);
    outcome
}
