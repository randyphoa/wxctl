//! `wxctl debug [run_id]` — diagnose a failed run. Defaults to the latest failed
//! run; prints the agent-ready bundle (markdown default, `-o json`). Read-only.

use crate::cli::OutputFormat;
use anyhow::{Result, bail};
use wxctl_core::diagnose::{build_bundle, find_latest_failed, list_runs, load_artifact};

pub fn execute(run_id: Option<&str>, output: Option<&OutputFormat>) -> Result<()> {
    let target = match run_id {
        Some(id) => id.to_string(),
        None => match find_latest_failed() {
            Some(id) => id,
            None => {
                let available = list_runs();
                if available.is_empty() {
                    bail!("no run records found. Run a command (apply/plan/destroy/test) first; failed runs are diagnosable here.");
                }
                bail!("no failed or aborted run found. Pass an explicit run id (see `wxctl runs list`); most recent: {}", available.first().map(|r| r.run_id.as_str()).unwrap_or("-"));
            }
        },
    };

    let art = load_artifact(&target)?;
    let bundle = build_bundle(&art);

    match output {
        Some(OutputFormat::Json) => println!("{}", serde_json::to_string_pretty(&bundle.render_json())?),
        None => print!("{}", bundle.render_markdown()),
    }
    Ok(())
}
