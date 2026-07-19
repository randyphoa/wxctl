use super::common::load_configs_resolved;
use super::progress_observer::{CliProgressObserver, CliTestObserver};
use crate::cli::OutputFormat;
use crate::output::color::{Color, Theme};
use anyhow::{Result, bail};
use std::sync::Arc;
use wxctl_core::logging::run_record::{RunCounts, RunManifest, RunSink, generate_run_id, utc_now_string};
use wxctl_sdk::{MetricOutcome, TestResults, TurnOutcome, WxctlClient};

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>, output: Option<&OutputFormat>) -> Result<()> {
    // Run record: install a sink so `wxctl runs` / `wxctl debug` see test runs too — the
    // "every run writes a run record" contract (runs.rs's own empty-state hint lists test).
    // plan/apply install theirs via CommandContext::setup, but `wxctl test` drives
    // WxctlClient directly, so wire one here and finalize on every exit path.
    let full_trace = crate::config::env_bool("WXCTL_FULL_TRACE");
    crate::output::set_full_trace(full_trace);
    let run_id = generate_run_id("test");
    let manifest = RunManifest {
        run_id: run_id.clone(),
        command: "test".to_string(),
        args: std::env::args().skip(1).collect(),
        profile: Some(profile.to_string()),
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
    let _run_guard = crate::output::install_run_sink(run_sink.clone());

    let outcome = run_tests(config_paths, profile, profile_path, &run_id, output).await;
    run_sink.finalize(if outcome.is_ok() { "success" } else { "failed" });
    outcome
}

async fn run_tests(config_paths: &[String], profile: &str, profile_path: Option<&str>, run_id: &str, output: Option<&OutputFormat>) -> Result<()> {
    let json = matches!(output, Some(OutputFormat::Json));
    let mut config = load_configs_resolved(config_paths)?;

    // Set up output infrastructure for progress spinners. `test` has no CommandContext,
    // so it gates its own collector: in JSON mode set quiet before any render call so the
    // spinners / summary can't corrupt the single JSON document (all test output routes
    // through the collector-backed CliTestObserver, which honors quiet).
    let color_pref = wxctl_core::load_color_preference(profile_path);
    // The collector panel draws to stderr → gate color on stderr's TTY.
    let theme = Theme::resolve_for_stderr(color_pref.as_deref());
    let collector = Arc::new(parking_lot::Mutex::new(crate::output::OutputCollector::new(uuid::Uuid::new_v4().to_string(), theme.clone())));
    if json {
        collector.lock().set_quiet();
    }

    let _guard = crate::output::install_collector(collector.clone());

    let observer = Arc::new(CliTestObserver::new(collector.clone()));
    // The discovery plan's reconciliation stage drives this exec observer, so the
    // pipeline panel's `N reconciled` counter reflects the real resource count
    // instead of a stale `0` (the test flow's plan otherwise runs with NoOpObserver).
    let exec_observer = Arc::new(CliProgressObserver::new(collector));
    let client = WxctlClient::new(profile, profile_path)?;
    // Record the run's deployment scope now that the profile is resolved (the manifest,
    // installed in `execute` before this async body runs, predates profile load). Mirrors
    // `CommandContext::setup_with_render`'s recording, via the same active-sink helper since
    // `test` has no `CommandContext`. Defaults to `Saas` like `WxctlClient::profile_deployment`'s
    // callers elsewhere treat an absent profile-level deployment.
    let deployment = client.profile_deployment().unwrap_or(wxctl_core::types::Deployment::Saas);
    crate::output::set_active_run_deployment(Some(deployment.flavor().to_string()));
    let results = client.test_with_observers(&mut config, observer, exec_observer).await?;

    if json {
        // Emit the DTO first (with per-case / per-turn detail), then exit 1 if any test
        // failed. `run_id` is the run-record id (feeds wxctl debug / run_diagnose).
        let out = wxctl_sdk::json::test_output(run_id.to_string(), &results);
        println!("{}", serde_json::to_string_pretty(&out)?);
        if results.has_failures() {
            bail!("{} test{} failed", results.failed, if results.failed == 1 { "" } else { "s" });
        }
        return Ok(());
    }

    print_results(&theme, &results);

    let total = results.total();
    println!("\n{} test{}, {} passed, {} failed", total, if total == 1 { "" } else { "s" }, results.passed, results.failed);

    if results.has_failures() {
        bail!("{} test{} failed", results.failed, if results.failed == 1 { "" } else { "s" });
    }

    Ok(())
}

fn print_results(theme: &Theme, results: &TestResults) {
    let check = theme.paint(Color::Green, "✓");
    let cross = theme.paint(Color::Red, "✗");
    for result in &results.tests {
        println!("\nTest: {}", result.ref_name);
        if let (Some(agent_ref), Some(agent_id)) = (&result.agent_ref, &result.agent_id) {
            println!("  Agent: {} (id: {})", agent_ref, truncate(agent_id, 12));
        } else if let (Some(deploy_ref), Some(deploy_id)) = (&result.deployment_ref, &result.deployment_id) {
            println!("  Deployment: {} (id: {})", deploy_ref, truncate(deploy_id, 12));
        } else if let (Some(flow_ref), Some(flow_id)) = (&result.flow_ref, &result.flow_id) {
            println!("  Flow: {} (id: {})", flow_ref, truncate(flow_id, 12));
        } else if let (Some(exposure_ref), Some(exposure_path)) = (&result.exposure_ref, &result.exposure_id) {
            println!("  Exposure: {} (path: {})", exposure_ref, exposure_path);
        }

        for tr in &result.turns {
            println!("  Turn {}/{}: \"{}\"", tr.turn_num, tr.total_turns, truncate(&tr.message, 50));

            match &tr.outcome {
                TurnOutcome::Success { content, .. } => {
                    if !tr.expect_tools.is_empty() {
                        println!("    {} Tools: {}", check, tr.expect_tools.join(", "));
                    }
                    println!("    {} Response received ({} chars)", check, content.len());
                    if let Some(ref expected) = tr.expect_answer {
                        println!("    ℹ Expected: {}", expected);
                        println!("    ℹ Actual: {}", content);
                    }
                }
                TurnOutcome::ToolMismatch { expected, actual, content } => {
                    println!("    {} Tools: expected [{}], got [{}]", cross, expected.join(", "), if actual.is_empty() { "none".to_string() } else { actual.join(", ") });
                    println!("    {} Response received ({} chars)", check, content.len());
                    if let Some(ref expected) = tr.expect_answer {
                        println!("    ℹ Expected: {}", expected);
                        println!("    ℹ Actual: {}", content);
                    }
                }
                TurnOutcome::Error(e) => {
                    println!("    {} Error: {}", cross, e);
                }
            }
        }

        for m in &result.metrics {
            match &m.outcome {
                MetricOutcome::Ready { value } => println!("  {} Metric {} [{}]: {}", check, m.metric_id, truncate(&m.monitor_ref, 30), value),
                MetricOutcome::Timeout { elapsed_secs, last_response } => println!("  {} Metric {} [{}]: no value after {}s. Last response: {}", cross, m.metric_id, truncate(&m.monitor_ref, 30), elapsed_secs, truncate(last_response, 200)),
                MetricOutcome::Error(e) => println!("  {} Metric {} [{}]: {}", cross, m.metric_id, truncate(&m.monitor_ref, 30), e),
            }
        }

        if result.passed {
            println!("  {}", theme.paint(Color::Green, "PASSED"));
        } else {
            println!("  {}", theme.paint(Color::Red, "FAILED"));
        }
    }
}

/// Truncate `s` to at most `n` characters (UTF-8 safe), appending "..." when shortened.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}...", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}
