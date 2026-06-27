use super::common::CommandContext;
use super::progress_observer::CliProgressObserver;
use anyhow::Result;
use std::sync::Arc;
use wxctl_engine::Pipeline;

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, full_trace: bool) -> Result<()> {
    let mut ctx = CommandContext::setup(config_paths, "plan", Some(profile), profile_path, full_trace)?;
    let outcome = async {
        let client_factory = ctx.client_factory.clone().expect("Client factory required for plan");

        let pipeline = Pipeline::new(ctx.registry.clone(), client_factory);
        // Same observer drives the live reconciliation counter (Phase 2). In Phase 1 its
        // reconcile methods are default no-ops, so output is unchanged.
        let observer = Arc::new(CliProgressObserver::new(ctx.collector.clone()));
        let _plan = pipeline.plan_with(&mut ctx.config, observer).await?;

        ctx.lock_collector().print_plan();
        ctx.finish()
    }
    .await;
    ctx.finalize_run_result(&outcome);
    outcome
}
