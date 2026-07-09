use super::common::CommandContext;
use super::progress_observer::CliProgressObserver;
use crate::cli::OutputFormat;
use anyhow::Result;
use std::sync::Arc;
use wxctl_engine::Pipeline;

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, full_trace: bool, output: Option<&OutputFormat>) -> Result<()> {
    // `-o json` owns stdout: put the collector in quiet mode before anything prints so
    // the header/stage panel can't corrupt the single JSON document.
    let json = matches!(output, Some(OutputFormat::Json));
    let mut ctx = CommandContext::setup_with_render(config_paths, "plan", Some(profile), profile_path, full_trace, !json)?;
    let outcome = async {
        let client_factory = ctx.client_factory.clone().expect("Client factory required for plan");

        let pipeline = Pipeline::new(ctx.registry.clone(), client_factory);
        // Same observer drives the live reconciliation counter. Its reconcile methods are
        // no-ops in quiet mode, so JSON output is unaffected.
        let observer = Arc::new(CliProgressObserver::new(ctx.collector.clone()));
        let plan = pipeline.plan_with(&mut ctx.config, observer).await?;

        if json {
            // Any valid plan is a success (changes are reported in `summary`); exit 0.
            let out = wxctl_sdk::json::PlanOutput::from(&plan);
            println!("{}", serde_json::to_string_pretty(&out)?);
            return Ok(());
        }

        ctx.lock_collector().print_plan();
        ctx.finish()
    }
    .await;
    ctx.finalize_run_result(&outcome);
    outcome
}
