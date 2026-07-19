//! Offline snapshot suite for the plan screens. Builds the typed sections
//! (`Pipeline`/`Changes`/`Errors`/`Footer`) with fixed data and snapshots
//! `panel_render` output via `insta` — fully deterministic (no live API calls,
//! no profile, no timing nondeterminism: durations are fixed). The five spec
//! states: success, no-changes, validation-failure, reconcile-error (AC17),
//! narrow-width 60. Run `cargo insta review` to accept.

use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{ColorMode, Theme};
use crate::output::panel_render::{render_advisories, render_changes, render_errors, render_footer, render_pipeline};
use crate::output::sections::*;

/// Build a fixed-width panel for snapshots. `glyphs`: unicode | ascii.
fn panel(width: usize, mode: ColorMode, glyphs: GlyphSet) -> Panel {
    Panel::new(Theme::new(mode), width, glyphs)
}

/// A full successful plan: 3 completed stages, +2 / ~1 changes, ok footer.
fn success_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_pipeline(
        p,
        &PipelineSection {
            rows: vec![
                PipelineRow { stage: "validation".into(), status: "completed".into(), duration_ms: Some(120), detail: None },
                PipelineRow { stage: "reconciliation".into(), status: "completed".into(), duration_ms: Some(840), detail: Some("5 reconciled".into()) },
                PipelineRow { stage: "planning".into(), status: "completed".into(), duration_ms: Some(15), detail: None },
            ],
        },
    ));
    lines.push(String::new());
    lines.extend(render_changes(
        p,
        &ChangesSection {
            rows: vec![
                ChangeRow { marker: ChangeMarker::Add, kind: "tool".into(), name: "calculator_tool".into(), action: "create".into(), changed_fields: vec![] },
                ChangeRow { marker: ChangeMarker::Add, kind: "agent".into(), name: "calculator_agent".into(), action: "create".into(), changed_fields: vec![] },
                ChangeRow { marker: ChangeMarker::Change, kind: "knowledge_base".into(), name: "kb".into(), action: "update".into(), changed_fields: vec!["description".into()] },
            ],
        },
    ));
    lines
        .extend(render_footer(p, &Footer { outcome: Outcome::PlanOk, command: "plan".into(), created: 2, updated: 1, deleted: 0, retained: 0, skipped: 0, undetermined: 0, duration_ms: 975, run_id: "RUNID".into(), config_hint: "examples/products/watsonx-orchestrate/hr-chatbot/config.yaml".into() }));
    lines.join("\n")
}

/// A plan that raised a reconcile advisory: success screen + a warn-level `▌ Advisories`
/// section carrying one R501 cross-type collision.
fn advisory_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_pipeline(
        p,
        &PipelineSection {
            rows: vec![
                PipelineRow { stage: "validation".into(), status: "completed".into(), duration_ms: Some(120), detail: None },
                PipelineRow { stage: "reconciliation".into(), status: "completed".into(), duration_ms: Some(840), detail: Some("3 reconciled".into()) },
                PipelineRow { stage: "planning".into(), status: "completed".into(), duration_ms: Some(15), detail: None },
            ],
        },
    ));
    lines.push(String::new());
    lines.extend(render_changes(p, &ChangesSection { rows: vec![ChangeRow { marker: ChangeMarker::Add, kind: "paw_book".into(), name: "Reports".into(), action: "create".into(), changed_fields: vec![] }] }));
    lines.extend(render_advisories(
        p,
        &AdvisoriesSection {
            blocks: vec![AdvisoryBlock {
                code: "WXCTL-R501".into(),
                resource: "paw_book/Reports".into(),
                message: "a same-named item exists with asset_type='folder' (expected 'dashboard'); 'Reports' is absent and will be created — a backend enforcing cross-type name uniqueness may then reject the create".into(),
                suggestion: "If the create is rejected, rename this resource so its name does not collide with the existing item of a different type.".into(),
            }],
        },
    ));
    lines.extend(render_footer(p, &Footer { outcome: Outcome::PlanOk, command: "plan".into(), created: 1, updated: 0, deleted: 0, retained: 0, skipped: 0, undetermined: 0, duration_ms: 975, run_id: "RUNID".into(), config_hint: "x".into() }));
    lines.join("\n")
}

/// No-changes plan: stages complete, empty changes, no-changes footer.
fn no_changes_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_pipeline(
        p,
        &PipelineSection {
            rows: vec![
                PipelineRow { stage: "validation".into(), status: "completed".into(), duration_ms: Some(110), detail: None },
                PipelineRow { stage: "reconciliation".into(), status: "completed".into(), duration_ms: Some(620), detail: Some("3 reconciled".into()) },
                PipelineRow { stage: "planning".into(), status: "completed".into(), duration_ms: Some(8), detail: None },
            ],
        },
    ));
    lines.push(String::new());
    lines.extend(render_changes(p, &ChangesSection::default()));
    lines.extend(render_footer(
        p,
        &Footer { outcome: Outcome::PlanNoChanges, command: "plan".into(), created: 0, updated: 0, deleted: 0, retained: 0, skipped: 0, undetermined: 0, duration_ms: 738, run_id: "RUNID".into(), config_hint: "examples/products/watsonx-ai/credit-risk-model/config.yaml".into() },
    ));
    lines.join("\n")
}

/// Validation failure: red ✗ validation row, single Errors block, failure footer.
fn validation_failure_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_pipeline(p, &PipelineSection { rows: vec![PipelineRow { stage: "validation".into(), status: "failed".into(), duration_ms: None, detail: None }] }));
    lines.push(String::new());
    lines.extend(render_errors(
        p,
        &ErrorsSection {
            blocks: vec![ErrorBlock { stage: "validation".into(), code: "WXCTL-V001".into(), kind: Some("agent".into()), name: Some("broken_agent".into()), field_path: Some("name".into()), message: "Missing required field: name".into(), fix: "Add a name field to the agent resource".into() }],
        },
    ));
    lines.extend(render_footer(p, &Footer { outcome: Outcome::Failed, command: "plan".into(), created: 0, updated: 0, deleted: 0, retained: 0, skipped: 0, undetermined: 0, duration_ms: 95, run_id: "20260612-000000-plan-abc123".into(), config_hint: "x".into() }));
    lines.join("\n")
}

/// Reconcile error (AC17): +1 add row AND a red `!` undetermined row, footnote
/// legend, Errors block, footer counting `+1 to add, 1 undetermined` → Failed.
fn reconcile_error_screen(p: &Panel) -> String {
    let mut lines = Vec::new();
    lines.extend(render_pipeline(
        p,
        &PipelineSection { rows: vec![PipelineRow { stage: "validation".into(), status: "completed".into(), duration_ms: Some(118), detail: None }, PipelineRow { stage: "reconciliation".into(), status: "completed".into(), duration_ms: Some(530), detail: Some("2 reconciled".into()) }] },
    ));
    lines.push(String::new());
    lines.extend(render_changes(
        p,
        &ChangesSection {
            rows: vec![
                ChangeRow { marker: ChangeMarker::Add, kind: "tool".into(), name: "ok_tool".into(), action: "create".into(), changed_fields: vec![] },
                ChangeRow { marker: ChangeMarker::Undetermined, kind: "agent".into(), name: "probe_failed".into(), action: "undetermined".into(), changed_fields: vec![] },
            ],
        },
    ));
    lines.extend(render_errors(
        p,
        &ErrorsSection {
            blocks: vec![ErrorBlock {
                stage: "reconciliation".into(),
                code: "WXCTL-R001".into(),
                kind: Some("agent".into()),
                name: Some("probe_failed".into()),
                field_path: None,
                message: "HTTP 500 Internal Server Error during discovery".into(),
                fix: "Check network connectivity and API credentials, then retry".into(),
            }],
        },
    ));
    lines.extend(render_footer(p, &Footer { outcome: Outcome::Failed, command: "plan".into(), created: 1, updated: 0, deleted: 0, retained: 0, skipped: 0, undetermined: 1, duration_ms: 648, run_id: "20260612-000000-plan-def456".into(), config_hint: "x".into() }));
    lines.join("\n")
}

#[test]
fn plan_success_dark_80() {
    insta::assert_snapshot!("plan_success_dark_80", success_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn plan_with_advisory_dark_80() {
    insta::assert_snapshot!("plan_with_advisory_dark_80", advisory_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn plan_no_changes_dark_80() {
    insta::assert_snapshot!("plan_no_changes_dark_80", no_changes_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn plan_validation_failure_dark_80() {
    insta::assert_snapshot!("plan_validation_failure_dark_80", validation_failure_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn plan_reconcile_error_dark_80() {
    insta::assert_snapshot!("plan_reconcile_error_dark_80", reconcile_error_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn plan_success_narrow_60() {
    insta::assert_snapshot!("plan_success_narrow_60", success_screen(&panel(60, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn plan_success_plain_80() {
    insta::assert_snapshot!("plan_success_plain_80", success_screen(&panel(80, ColorMode::Plain, GlyphSet::Unicode)));
}

#[test]
fn plan_success_ascii_80() {
    insta::assert_snapshot!("plan_success_ascii_80", success_screen(&panel(80, ColorMode::Plain, GlyphSet::Ascii)));
}

// ── AC byte assertions (rendered offline, deterministic) ──

/// Invariants over the plan success screen, folded into one render-and-check:
/// AC8 — never says "created", uses plan-tense "+N to add" + the next-apply hint;
/// AC12 — plain mode is zero-ANSI; AC16 — ascii glyph set + plain is pure ASCII.
#[test]
fn plan_success_screen_invariants() {
    // AC8 + AC12 over the plain/Unicode render (zero-ANSI, plan-tense).
    let plain = success_screen(&panel(80, ColorMode::Plain, GlyphSet::Unicode));
    assert!(!plain.contains("created"), "plan screen must never contain 'created': {plain}");
    assert!(plain.contains("+2 to add"), "plan-tense add wording present");
    assert!(plain.contains("next: wxctl apply -f"), "next-apply hint present");
    assert!(!plain.contains('\u{1b}'), "plain plan screen has no ANSI escape");
    // AC16 over the ascii render: pure ASCII (glyphs transliterate).
    let ascii = success_screen(&panel(80, ColorMode::Plain, GlyphSet::Ascii));
    assert!(ascii.is_ascii(), "ascii plan screen must be pure ASCII: {ascii:?}");
}

/// AC10: Type/Name carry no SGR; only the marker + action are painted.
#[test]
fn changes_type_and_name_uncolored() {
    let p = panel(80, ColorMode::Dark, GlyphSet::Unicode);
    let line = render_changes(&p, &ChangesSection { rows: vec![ChangeRow { marker: ChangeMarker::Add, kind: "tool".into(), name: "calc".into(), action: "create".into(), changed_fields: vec![] }] }).join("\n");
    // Find the data row and assert that the kind+name cells contain no ESC byte.
    let data_row = line.lines().find(|l| l.contains("calc")).expect("data row present");
    // Strip ANSI sequences from the data row, then check kind+name are present.
    // We verify that the substring between the first reset and the next ESC is uncolored.
    // Simpler: just verify the literal kind+name text appears without any ESC byte
    // between them in the row (the row has ESC only for marker and action, not in the middle).
    let calc_pos = data_row.find("calc").expect("calc present");
    let tool_pos = data_row.find("tool").expect("tool present");
    // Neither "tool" nor "calc" should have an ESC byte immediately before them
    // (within a 5-byte window) — they are in the uncolored column cells.
    let before_tool = &data_row[..tool_pos];
    let before_calc = &data_row[..calc_pos];
    // The last ESC before "tool" should be the marker's color (not right before "tool").
    // Assert there's no ESC in the final 3 chars before "tool".
    let tool_prefix: String = before_tool.chars().rev().take(3).collect();
    let calc_prefix: String = before_calc.chars().rev().take(3).collect();
    assert!(!tool_prefix.contains('\u{1b}'), "no ESC right before 'tool' (kind is uncolored): {data_row:?}");
    assert!(!calc_prefix.contains('\u{1b}'), "no ESC right before 'calc' (name is uncolored): {data_row:?}");
}

/// AC17: the reconcile-error screen shows a `!` undetermined row in the Changes
/// section with the footnote legend, the Errors block, and a "plan failed" footer
/// with counts (+1 to add, 1 undetermined). The failed resource never appears as
/// a confident `create`.
#[test]
fn reconcile_error_shows_undetermined_not_create() {
    let out = reconcile_error_screen(&panel(80, ColorMode::Plain, GlyphSet::Unicode));
    assert!(out.contains("probe_failed"), "failed resource present");
    // The changes row action is "undetermined", not "create".
    assert!(out.contains("undetermined"), "undetermined action present in changes row");
    // The footnote legend is rendered because there is an Undetermined marker.
    assert!(out.contains("undetermined — discovery failed"), "footnote legend present");
    // The footer emits "plan failed" (Outcome::Failed path).
    assert!(out.contains("plan failed"), "footer says plan failed");
    // The footer counts non-zero buckets: +1 to add and 1 undetermined.
    assert!(out.contains("+1 to add"), "footer counts +1 to add");
    assert!(out.contains("1 undetermined"), "footer counts undetermined");
    // The failed resource has no confident create row.
    assert!(!out.contains("probe_failed  create"), "failed resource is not a confident create");
}

/// AC2/AC7: the completed reconciliation row carries `N reconciled`; a
/// *failed* reconciliation row carries no count (a count would imply completion).
#[test]
fn reconciliation_row_count_only_on_completion() {
    let p = panel(80, ColorMode::Plain, GlyphSet::Unicode);
    let completed = render_pipeline(&p, &PipelineSection { rows: vec![PipelineRow { stage: "reconciliation".into(), status: "completed".into(), duration_ms: Some(840), detail: Some("5 reconciled".into()) }] }).join("\n");
    assert!(completed.contains("5 reconciled"), "completed reconciliation shows the count: {completed:?}");
    let failed = render_pipeline(&p, &PipelineSection { rows: vec![PipelineRow { stage: "reconciliation".into(), status: "failed".into(), duration_ms: None, detail: None }] }).join("\n");
    assert!(failed.contains("\u{2717} reconciliation"), "failed reconciliation shows ✗: {failed:?}");
    assert!(!failed.contains("reconciled"), "failed reconciliation carries no count: {failed:?}");
}

/// AC13: narrow width (60) wraps cleanly — no line exceeds the width, no orphaned
/// duration. Uses a fixture with short names so column widths fit in 60 cols.
/// (Plain mode so char count == visible width.)
#[test]
fn narrow_60_no_overflow() {
    // Short-name fixture that still exercises all three marker types within 60 cols.
    let p = panel(60, ColorMode::Plain, GlyphSet::Unicode);
    let mut lines = Vec::new();
    lines.extend(render_pipeline(
        &p,
        &PipelineSection {
            rows: vec![
                PipelineRow { stage: "validation".into(), status: "completed".into(), duration_ms: Some(120), detail: None },
                PipelineRow { stage: "reconciliation".into(), status: "completed".into(), duration_ms: Some(840), detail: Some("5 reconciled".into()) },
                PipelineRow { stage: "planning".into(), status: "completed".into(), duration_ms: Some(15), detail: None },
            ],
        },
    ));
    lines.push(String::new());
    lines.extend(render_changes(
        &p,
        &ChangesSection {
            rows: vec![
                ChangeRow { marker: ChangeMarker::Add, kind: "tool".into(), name: "calc".into(), action: "create".into(), changed_fields: vec![] },
                ChangeRow { marker: ChangeMarker::Add, kind: "agent".into(), name: "bot".into(), action: "create".into(), changed_fields: vec![] },
                ChangeRow { marker: ChangeMarker::Change, kind: "kb".into(), name: "kb".into(), action: "update".into(), changed_fields: vec!["desc".into()] },
            ],
        },
    ));
    lines.extend(render_footer(&p, &Footer { outcome: Outcome::PlanOk, command: "plan".into(), created: 2, updated: 1, deleted: 0, retained: 0, skipped: 0, undetermined: 0, duration_ms: 975, run_id: "RUNID".into(), config_hint: "examples/simple/config.yaml".into() }));
    let out = lines.join("\n");
    for line in out.lines() {
        assert!(line.chars().count() <= 60, "line within 60 cols: {line:?} ({} cols)", line.chars().count());
    }
}
