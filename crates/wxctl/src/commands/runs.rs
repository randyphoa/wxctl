//! `wxctl runs list` / `wxctl runs show <id>` — list run records and inspect one.
//! Read-only reporting over the Phase 1 artifact tree; does not open a `run` span
//! or write its own artifact (mirrors `explain`/`resources`).

use crate::output::color::{Color, Theme};
use anyhow::Result;
use wxctl_core::diagnose::{list_runs, load_artifact};

/// `wxctl runs list`.
pub fn list() -> Result<()> {
    let theme = Theme::resolve(None);
    let runs = list_runs();
    if runs.is_empty() {
        println!("\n  {}\n", theme.paint(Color::Dim, "No run records found. Run a command (apply/plan/destroy/test) to produce one."));
        return Ok(());
    }
    println!();
    println!("  {:<32} {:<9} {:<22} {:<9} ERRORS", "RUN ID", "COMMAND", "STARTED", "OUTCOME");
    println!("  {}", theme.paint(Color::Dim, &"─".repeat(86)));
    for r in &runs {
        let outcome_color = match r.outcome.as_str() {
            "success" => Color::Green,
            "failed" | "aborted" => Color::Red,
            _ => Color::Dim,
        };
        println!(
            "  {:<32} {:<9} {:<22} {} {}",
            r.run_id,
            theme.paint(Color::Blue, &format!("{:<9}", r.command)),
            theme.paint(Color::Dim, &format!("{:<22}", r.started)),
            theme.paint(outcome_color, &format!("{:<9}", r.outcome)),
            if r.error_count > 0 { theme.paint(Color::Red, &r.error_count.to_string()) } else { theme.paint(Color::Dim, "0") }
        );
    }
    println!();
    Ok(())
}

/// `wxctl runs show <id>` (`--full` dumps raw events).
pub fn show(run_id: &str, full: bool) -> Result<()> {
    let theme = Theme::resolve(None);
    let art = load_artifact(run_id)?;
    let m = &art.manifest;

    println!();
    println!("  {}   {} · {}", theme.paint(Color::BoldWhite, &m.run_id), theme.paint(Color::Blue, &m.command), theme.paint(outcome_color(m.outcome.as_deref()), m.outcome.as_deref().unwrap_or("unknown")));
    if let Some(p) = &m.profile {
        println!("  {}", theme.paint(Color::Dim, &format!("profile · {p}")));
    }
    if !m.config_paths.is_empty() {
        println!("  {}", theme.paint(Color::Dim, &format!("config · {}", m.config_paths.join(", "))));
    }
    println!("  {}", theme.paint(Color::Dim, &format!("started · {}   finished · {}", m.started, m.finished.as_deref().unwrap_or("-"))));
    println!("  {}", theme.paint(Color::Dim, &format!("events · {}   full_trace · {}", art.events.len(), m.full_trace)));

    if !m.errors.is_empty() {
        println!();
        println!("  {} {}", theme.paint(Color::Red, "▌"), theme.paint(Color::BoldWhite, "Errors"));
        println!("  {}", theme.paint(Color::Dim, &"─".repeat(56)));
        for e in &m.errors {
            let res = e.resource.as_deref().unwrap_or("");
            println!("    {}  {}  {}", theme.paint(Color::Red, &e.code), theme.paint(Color::Dim, res), e.message);
            if let Some(fix) = &e.fix {
                println!("    {}  {}", theme.paint(Color::Dim, &" ".repeat(e.code.len())), theme.paint(Color::Yellow, fix));
            }
        }
    }

    if full {
        println!();
        println!("  {} {}", theme.paint(Color::Blue, "▌"), theme.paint(Color::BoldWhite, "Events"));
        println!("  {}", theme.paint(Color::Dim, &"─".repeat(56)));
        let events_text = std::fs::read_to_string(art.dir.join("events.jsonl")).unwrap_or_default();
        print!("{events_text}");
    }
    println!();
    Ok(())
}

fn outcome_color(outcome: Option<&str>) -> Color {
    match outcome {
        Some("success") => Color::Green,
        Some("failed") | Some("aborted") => Color::Red,
        _ => Color::Dim,
    }
}
