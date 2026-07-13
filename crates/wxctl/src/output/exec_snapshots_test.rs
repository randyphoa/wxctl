//! Offline snapshot suite for the execution final screens (apply/destroy/test).
//! Builds the typed `▌ Execution` section + `ExecFooter` with fixed data and
//! snapshots `render_execution`/`render_exec_footer` via `insta` — deterministic
//! (no live API, no Animator, fixed durations). Covers AC15: apply summary lists
//! created resources with URLs; the failure footer carries run id + `wxctl debug`.
//! Run `cargo insta review` to accept.

use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{ColorMode, Theme};
use crate::output::panel_render::{render_errors, render_exec_footer, render_execution};
use crate::output::sections::*;

fn panel(width: usize, mode: ColorMode, glyphs: GlyphSet) -> Panel {
    Panel::new(Theme::new(mode), width, glyphs)
}

/// Apply success: 3 created rows (DAG connectors), footer with URLs.
fn apply_success_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_execution(
        p,
        &ExecutionSection {
            rows: vec![
                ExecRow { marker: ExecMarker::Created, kind: "orchestrate_connection".into(), name: "httpbin_bearer".into(), changed_fields: vec![], duration_ms: 620, last: false, id: Some("dd33a261-f041-43e3-be43-f41be3418c21".into()) },
                ExecRow { marker: ExecMarker::Created, kind: "tool".into(), name: "httpbin_tools".into(), changed_fields: vec![], duration_ms: 1840, last: false, id: Some("805591f1-148f-4f79-8c95-be4785cd0733".into()) },
                ExecRow { marker: ExecMarker::Created, kind: "agent".into(), name: "httpbin_agent".into(), changed_fields: vec![], duration_ms: 2310, last: true, id: Some("5d59f99e-95ae-4b54-8719-3cb993fa0072".into()) },
            ],
        },
    ));
    lines.extend(render_exec_footer(
        p,
        &ExecFooter {
            outcome: ExecOutcome::Ok,
            command: "apply".into(),
            created: 3,
            updated: 0,
            deleted: 0,
            retained: 0,
            failed: 0,
            duration_ms: 4770,
            urls: vec![CreatedUrl { name: "agent.httpbin_agent".into(), url: "https://api.watson-orchestrate.ibm.com/agents/abc123".into() }],
            run_id: "RUNID".into(),
        },
    ));
    lines.join("\n")
}

/// Apply failure: one created row, one failed row, Errors block, failure footer.
fn apply_failure_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_execution(
        p,
        &ExecutionSection {
            rows: vec![
                ExecRow { marker: ExecMarker::Created, kind: "tool".into(), name: "httpbin_tools".into(), changed_fields: vec![], duration_ms: 1500, last: false, id: None },
                ExecRow { marker: ExecMarker::Failed, kind: "agent".into(), name: "httpbin_agent".into(), changed_fields: vec![], duration_ms: 900, last: true, id: None },
            ],
        },
    ));
    lines.extend(render_errors(
        p,
        &ErrorsSection {
            blocks: vec![ErrorBlock {
                stage: "execution".into(),
                code: "WXCTL-E001".into(),
                kind: Some("agent".into()),
                name: Some("httpbin_agent".into()),
                field_path: None,
                message: "HTTP 422 unprocessable entity: tool reference unresolved".into(),
                fix: "Check the error message and fix the resource configuration".into(),
            }],
        },
    ));
    lines.extend(render_exec_footer(p, &ExecFooter { outcome: ExecOutcome::Failed, command: "apply".into(), created: 1, updated: 0, deleted: 0, retained: 0, failed: 1, duration_ms: 2400, urls: vec![], run_id: "20260612-000000-apply-def456".into() }));
    lines.join("\n")
}

/// Destroy success: 3 deleted rows, destroy footer.
fn destroy_success_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_execution(
        p,
        &ExecutionSection {
            rows: vec![
                ExecRow { marker: ExecMarker::Deleted, kind: "agent".into(), name: "httpbin_agent".into(), changed_fields: vec![], duration_ms: 410, last: false, id: None },
                ExecRow { marker: ExecMarker::Deleted, kind: "tool".into(), name: "httpbin_tools".into(), changed_fields: vec![], duration_ms: 380, last: false, id: None },
                ExecRow { marker: ExecMarker::Deleted, kind: "orchestrate_connection".into(), name: "httpbin_bearer".into(), changed_fields: vec![], duration_ms: 290, last: true, id: None },
            ],
        },
    ));
    lines.extend(render_exec_footer(p, &ExecFooter { outcome: ExecOutcome::Ok, command: "destroy".into(), created: 0, updated: 0, deleted: 3, retained: 0, failed: 0, duration_ms: 1080, urls: vec![], run_id: "RUNID".into() }));
    lines.join("\n")
}

/// Test summary: pass + fail rows + a test footer (created=passed, failed=failed).
fn test_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_execution(
        p,
        &ExecutionSection {
            rows: vec![
                ExecRow { marker: ExecMarker::Created, kind: "test".into(), name: "test_echo_get".into(), changed_fields: vec![], duration_ms: 0, last: false, id: None },
                ExecRow { marker: ExecMarker::Failed, kind: "test".into(), name: "test_echo_post".into(), changed_fields: vec![], duration_ms: 0, last: true, id: None },
            ],
        },
    ));
    lines.extend(render_exec_footer(p, &ExecFooter { outcome: ExecOutcome::Failed, command: "test".into(), created: 1, updated: 0, deleted: 0, retained: 0, failed: 1, duration_ms: 5200, urls: vec![], run_id: "20260612-000000-test-aaa111".into() }));
    lines.join("\n")
}

#[test]
fn apply_success_dark_80() {
    insta::assert_snapshot!("apply_success_dark_80", apply_success_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn apply_failure_dark_80() {
    insta::assert_snapshot!("apply_failure_dark_80", apply_failure_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn destroy_success_dark_80() {
    insta::assert_snapshot!("destroy_success_dark_80", destroy_success_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn test_summary_dark_80() {
    insta::assert_snapshot!("test_summary_dark_80", test_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn apply_success_ascii_80() {
    insta::assert_snapshot!("apply_success_ascii_80", apply_success_screen(&panel(80, ColorMode::Plain, GlyphSet::Ascii)));
}

#[test]
fn apply_success_narrow_60() {
    insta::assert_snapshot!("apply_success_narrow_60", apply_success_screen(&panel(60, ColorMode::Dark, GlyphSet::Unicode)));
}

// ── AC15 byte assertions (rendered offline, deterministic) ──

/// Invariants over the apply-success screen, folded into one render-and-check:
/// AC15 — lists created resources with URLs, apply-tense `+N created`; Option A —
/// each created row surfaces its backend id as a `[id=…]` suffix (one per row);
/// plain mode is zero-ANSI; the ascii render is pure ASCII (connectors transliterate).
#[test]
fn apply_success_screen_invariants() {
    let out = apply_success_screen(&panel(80, ColorMode::Plain, GlyphSet::Unicode));
    // AC15: apply-tense count + created-URL list.
    assert!(out.contains("+3 created"), "apply-tense created count: {out}");
    assert!(out.contains("created resources:"), "url section header present");
    assert!(out.contains("https://api.watson-orchestrate.ibm.com/agents/abc123"), "created URL listed: {out}");
    // Option A: each created row shows its backend resource id — middle-truncated to the
    // width-80 Name-column budget (head + tail survive; full ids stay in the URL footer).
    assert!(out.contains("[id=805591f1\u{2026}5cd0733]"), "tool row shows its create id, middle-truncated: {out}");
    assert!(out.contains("[id=5d59f99e\u{2026}3fa0072]"), "agent row shows its create id, middle-truncated: {out}");
    assert_eq!(out.matches("[id=").count(), 3, "one [id=…] suffix per created row: {out}");
    // Zero-ANSI in plain mode.
    assert!(!out.contains('\u{1b}'), "plain execution screen has no ANSI escape");
    // ascii render is pure ASCII.
    let ascii = apply_success_screen(&panel(80, ColorMode::Plain, GlyphSet::Ascii));
    assert!(ascii.is_ascii(), "ascii execution screen must be pure ASCII: {ascii:?}");
}

/// Deletes carry no create response → no `[id=…]` suffix on destroy rows.
#[test]
fn destroy_rows_have_no_id_suffix() {
    let out = destroy_success_screen(&panel(80, ColorMode::Plain, GlyphSet::Unicode));
    assert!(!out.contains("[id="), "destroy rows must not render an id suffix: {out}");
}

/// AC15: a failed apply's footer carries run id + `wxctl debug`.
#[test]
fn apply_failure_footer_has_run_id_and_debug() {
    let out = apply_failure_screen(&panel(80, ColorMode::Plain, GlyphSet::Unicode));
    assert!(out.contains("apply failed"), "failure verb: {out}");
    assert!(out.contains("20260612-000000-apply-def456"), "run id present: {out}");
    assert!(out.contains("wxctl debug"), "debug hint present: {out}");
}
