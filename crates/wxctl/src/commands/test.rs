use super::common::{load_configs, resolve_config_dir, resolve_file_paths};
use super::progress_observer::CliTestObserver;
use anyhow::{Result, bail};
use std::sync::Arc;
use wxctl_sdk::{TestResults, TurnOutcome, WxctlClient};

pub async fn execute(config_paths: &[String], profile: &str, profile_path: Option<&str>) -> Result<()> {
    let content = load_configs(config_paths)?;
    let mut config = wxctl_core::Config::from_yaml(&content)?;
    if let Some(config_dir) = resolve_config_dir(config_paths) {
        resolve_file_paths(&mut config, &config_dir);
    }

    // Set up output infrastructure for progress spinners
    let color_pref = wxctl_core::load_color_preference(profile_path);
    let theme = crate::output::color::Theme::resolve(color_pref.as_deref());
    let collector = Arc::new(parking_lot::Mutex::new(crate::output::OutputCollector::new(uuid::Uuid::new_v4().to_string(), theme)));

    let _guard = crate::output::install_collector(collector.clone());

    let observer = Arc::new(CliTestObserver::new(collector));
    let client = WxctlClient::new(profile, profile_path)?;
    let results = client.test_with_observer(&mut config, observer).await?;

    print_results(&results);

    let total = results.total();
    println!("\n{} test{}, {} passed, {} failed", total, if total == 1 { "" } else { "s" }, results.passed, results.failed);

    if results.has_failures() {
        bail!("{} test{} failed", results.failed, if results.failed == 1 { "" } else { "s" });
    }

    Ok(())
}

fn print_results(results: &TestResults) {
    for result in &results.tests {
        println!("\nTest: {}", result.ref_name);
        if let (Some(agent_ref), Some(agent_id)) = (&result.agent_ref, &result.agent_id) {
            println!("  Agent: {} (id: {})", agent_ref, truncate(agent_id, 12));
        } else if let (Some(deploy_ref), Some(deploy_id)) = (&result.deployment_ref, &result.deployment_id) {
            println!("  Deployment: {} (id: {})", deploy_ref, truncate(deploy_id, 12));
        } else if let (Some(flow_ref), Some(flow_id)) = (&result.flow_ref, &result.flow_id) {
            println!("  Flow: {} (id: {})", flow_ref, truncate(flow_id, 12));
        }

        for tr in &result.turns {
            println!("  Turn {}/{}: \"{}\"", tr.turn_num, tr.total_turns, truncate(&tr.message, 50));

            match &tr.outcome {
                TurnOutcome::Success { content, .. } => {
                    if !tr.expect_tools.is_empty() {
                        println!("    \x1b[32m✓\x1b[0m Tools: {}", tr.expect_tools.join(", "));
                    }
                    println!("    \x1b[32m✓\x1b[0m Response received ({} chars)", content.len());
                    if let Some(ref expected) = tr.expect_answer {
                        println!("    ℹ Expected: {}", expected);
                        println!("    ℹ Actual: {}", content);
                    }
                }
                TurnOutcome::ToolMismatch { expected, actual, content } => {
                    println!("    \x1b[31m✗\x1b[0m Tools: expected [{}], got [{}]", expected.join(", "), if actual.is_empty() { "none".to_string() } else { actual.join(", ") });
                    println!("    \x1b[32m✓\x1b[0m Response received ({} chars)", content.len());
                    if let Some(ref expected) = tr.expect_answer {
                        println!("    ℹ Expected: {}", expected);
                        println!("    ℹ Actual: {}", content);
                    }
                }
                TurnOutcome::Error(e) => {
                    println!("    \x1b[31m✗\x1b[0m Error: {}", e);
                }
            }
        }

        if result.passed {
            println!("  \x1b[32mPASSED\x1b[0m");
        } else {
            println!("  \x1b[31mFAILED\x1b[0m");
        }
    }
}

/// Truncate `s` to at most `n` characters (UTF-8 safe), appending "..." when shortened.
fn truncate(s: &str, n: usize) -> String {
    if s.chars().count() > n { format!("{}...", s.chars().take(n).collect::<String>()) } else { s.to_string() }
}
