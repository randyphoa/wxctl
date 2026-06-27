//! Pure formatters: typed section → `Vec<String>`, painted via the injected
//! `Panel`. No I/O, no globals — every function is total over its inputs, which
//! is what makes the plan screens snapshot-testable offline.

use crate::output::color::format_duration;
use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::Role;
// `CreatedUrl` and `ExecRow` are only referenced in test struct literals (they
// appear as field types of ExecFooter/ExecutionSection, not in function signatures).
#[allow(unused_imports)]
use crate::output::sections::{ChangeMarker, ChangesSection, CreatedUrl, ErrorsSection, ExecFooter, ExecMarker, ExecOutcome, ExecRow, ExecutionSection, Footer, Outcome, PipelineSection};

/// Width of the stage-name column in the `▌ Pipeline` section — the longest stage
/// label (`reconciliation`). Durations align just past it instead of being
/// stretched to the panel's right edge: a step list reads better with the time
/// next to its label than leader-gapped to a wide table column. A constant (vs.
/// deriving the max per call) is what keeps the streaming per-row render in
/// `collector.rs` — which only ever sees one row at a time — aligned with this
/// batch render.
const PIPELINE_STAGE_COL: usize = "reconciliation".len();
/// Gap between the stage-name column and the duration.
const PIPELINE_TIME_GAP: usize = 3;

/// `▌ Pipeline` section: one row per stage, green `✓ <stage>   <dur>`,
/// red `✗ <stage>` on failure, dim `· <stage>…` while in-flight. Duration sits
/// in a compact column anchored to the longest stage name; missing duration
/// renders `—`.
pub fn render_pipeline(panel: &Panel, section: &PipelineSection) -> Vec<String> {
    let mut out = vec![panel.section("Pipeline", None)];
    let dash = panel.g("emdash");
    for row in &section.rows {
        let (glyph, role) = match row.status.as_str() {
            "completed" => (panel.g("check"), Role::Success),
            "failed" => (panel.g("cross"), Role::Danger),
            _ => (panel.g("bullet"), Role::Meta),
        };
        let marker = panel.paint(role, glyph);
        let name = panel.paint(role, &row.stage);
        let dur = match (row.status.as_str(), row.duration_ms) {
            ("completed", Some(ms)) => format_duration(ms),
            ("completed", None) => dash.to_string(),
            _ => String::new(),
        };
        // Dim detail (e.g. "5 reconciled") sits just past the stage-name column,
        // between the name and the duration. Only populated rows carry it.
        let detail = row.detail.as_deref().filter(|d| !d.is_empty());
        if dur.is_empty() && detail.is_none() {
            out.push(format!("    {} {}", marker, name));
        } else {
            // Pad the (uncolored-length) stage name to the column, then a fixed gap.
            let pad = (PIPELINE_STAGE_COL + PIPELINE_TIME_GAP).saturating_sub(row.stage.chars().count()).max(PIPELINE_TIME_GAP);
            let detail_seg = match detail {
                Some(d) => format!("{}{}", panel.paint(Role::Meta, d), " ".repeat(PIPELINE_TIME_GAP)),
                None => String::new(),
            };
            let dur_seg = if dur.is_empty() { String::new() } else { panel.paint(Role::Meta, &dur) };
            out.push(format!("    {} {}{}{}{}", marker, name, " ".repeat(pad), detail_seg, dur_seg));
        }
    }
    out
}

/// `▌ Changes` section: column-aligned `<marker>  <kind>  <name>  <action>`.
/// Only the marker and action carry color (AC10). `?`/`!` rows are amber/red and
/// trigger a footnote legend when present.
pub fn render_changes(panel: &Panel, section: &ChangesSection) -> Vec<String> {
    if section.rows.is_empty() {
        return vec![panel.section("Changes", None), format!("    {}", panel.paint(Role::Meta, "No changes. All resources match desired state."))];
    }
    let kind_w = section.rows.iter().map(|r| r.kind.chars().count()).max().unwrap_or(4).max(4);
    let name_w = section.rows.iter().map(|r| r.name.chars().count()).max().unwrap_or(4).max(4);

    let mut out = vec![panel.section("Changes", None)];
    // header row (dim, uncolored cells) — capitalized to match the Type/Name labels.
    out.push(panel.paint(Role::Meta, &format!("       {:<kw$}  {:<nw$}  Action", "Type", "Name", kw = kind_w, nw = name_w)));

    for r in &section.rows {
        let (glyph, role) = marker_glyph_role(panel, r.marker);
        let marker = panel.paint(role, glyph);
        let action = panel.paint(role, &r.action);
        // Type and Name are uncolored (AC10).
        let mut line = format!("    {}  {:<kw$}  {:<nw$}  {}", marker, r.kind, r.name, action, kw = kind_w, nw = name_w);
        line.push_str(&changed_fields_suffix(panel, &r.changed_fields));
        out.push(line);
    }

    if section.has_uncertain() {
        out.push(String::new());
        let q = panel.g("query");
        let b = panel.g("bang");
        out.push(format!("    {}", panel.paint(Role::Meta, &format!("{q} unchecked — identity path templated; {b} undetermined — discovery failed (see {} Errors)", panel.g("bar")))));
    }
    out
}

/// Map a marker class to its glyph + role (single source of truth for AC10/17).
fn marker_glyph_role(panel: &Panel, m: ChangeMarker) -> (&'static str, Role) {
    match m {
        ChangeMarker::Add => (ascii_or(panel, "+", "+"), Role::Success),
        ChangeMarker::Change => (ascii_or(panel, "~", "~"), Role::Caution),
        ChangeMarker::Destroy => (ascii_or(panel, "-", "-"), Role::Danger),
        ChangeMarker::Recreate => (ascii_or(panel, "\u{00b1}", "+-"), Role::Caution),
        ChangeMarker::Retain => (ascii_or(panel, "=", "="), Role::Active),
        ChangeMarker::Unchecked => (panel.g("query"), Role::Caution),
        ChangeMarker::Undetermined => (panel.g("bang"), Role::Danger),
        ChangeMarker::Skip => (panel.g("dot"), Role::Meta),
    }
}

/// Pick a unicode glyph or its ascii fallback by the panel's glyph set, for the
/// markers that aren't in the shared glyph table (`+ ~ - ± =`).
fn ascii_or(panel: &Panel, unicode: &'static str, ascii: &'static str) -> &'static str {
    if panel.glyphs == GlyphSet::Ascii { ascii } else { unicode }
}

/// Build the `"  [~a, ~b]"` dim suffix for changed fields, painted as `Role::Meta`.
/// Returns an empty `String` when `fields` is empty (making `push_str` a no-op).
fn changed_fields_suffix(panel: &Panel, fields: &[String]) -> String {
    if fields.is_empty() {
        return String::new();
    }
    let diff: Vec<String> = fields.iter().map(|f| format!("~{f}")).collect();
    format!("  {}", panel.paint(Role::Meta, &format!("[{}]", diff.join(", "))))
}

/// Build the plain `"  [id=…]"` annotation for an execution row's backend resource
/// id (Terraform-style), shown next to the resource name. Empty `String` when there's
/// no id (deletes, responses without one). Returned unpainted so the caller can both
/// measure its width (for column padding) and paint it.
fn id_annotation(id: Option<&str>) -> String {
    match id {
        Some(id) if !id.is_empty() => format!("  [id={id}]"),
        _ => String::new(),
    }
}

/// Join `parts` with the dim dot separator used by footer lines.
fn dot_join(panel: &Panel, dot: &str, parts: &[String]) -> String {
    parts.join(&format!(" {} ", panel.paint(Role::Meta, dot)))
}

/// The past-tense action label for an execution marker (panel-independent). Exposed so
/// the collector can size the live Action column from the planned decisions before any
/// row has completed.
pub fn exec_marker_action_label(m: ExecMarker) -> &'static str {
    match m {
        ExecMarker::Created => "created",
        ExecMarker::Updated => "updated",
        ExecMarker::Deleted => "deleted",
        ExecMarker::Recreated => "recreated",
        ExecMarker::Failed => "failed",
    }
}

/// Map an execution-row marker to its glyph, past-tense action label, and role in one call.
fn exec_marker_attrs(panel: &Panel, m: ExecMarker) -> (&'static str, &'static str, Role) {
    let role = match m {
        ExecMarker::Created => Role::Success,
        ExecMarker::Updated | ExecMarker::Recreated => Role::Caution,
        ExecMarker::Deleted | ExecMarker::Failed => Role::Danger,
    };
    let glyph = match m {
        ExecMarker::Created => "+",
        ExecMarker::Updated => "~",
        ExecMarker::Deleted => "-",
        ExecMarker::Recreated => ascii_or(panel, "\u{00b1}", "+-"),
        ExecMarker::Failed => panel.g("cross"),
    };
    (glyph, exec_marker_action_label(m), role)
}

/// `▌ Errors` section: full single-render detail per error (code, resource,
/// field, message, fix). Renders nothing when empty.
pub fn render_errors(panel: &Panel, section: &ErrorsSection) -> Vec<String> {
    if section.blocks.is_empty() {
        return Vec::new();
    }
    let mut out = vec![panel.section("Errors", None)];
    for b in &section.blocks {
        out.push(String::new());
        let head = match (&b.kind, &b.name) {
            (Some(k), Some(n)) => format!("{}/{}", k, n),
            _ => format!("{} stage", b.stage),
        };
        out.push(format!("    {} {}  {}", panel.paint(Role::Danger, panel.g("cross")), panel.paint(Role::Heading, &head), panel.paint(Role::Meta, &b.code)));
        if let Some(fp) = &b.field_path {
            out.push(format!("      {} {}", panel.paint(Role::Meta, "field"), fp));
        }
        for line in panel.wrap_hanging(&b.message, 6) {
            out.push(line);
        }
        out.push(format!("      {} {}", panel.paint(Role::Active, "fix"), b.fix));
    }
    out
}

/// Build plan-tense count parts (`+N to add`, `~N to change`, `-N to destroy`,
/// `=N to retain`, `K undetermined`). Skips zero counts. When `include_skipped`
/// is false the `skipped` bucket is omitted (not shown in the Failed arm).
/// Returns an empty `Vec` when all relevant counts are zero.
fn build_count_parts(panel: &Panel, f: &Footer, include_skipped: bool) -> Vec<String> {
    let mut parts: Vec<String> = Vec::new();
    if f.created > 0 {
        parts.push(panel.paint(Role::Success, &format!("+{} to add", f.created)));
    }
    if f.updated > 0 {
        parts.push(panel.paint(Role::Caution, &format!("~{} to change", f.updated)));
    }
    if f.deleted > 0 {
        parts.push(panel.paint(Role::Danger, &format!("-{} to destroy", f.deleted)));
    }
    if f.retained > 0 {
        parts.push(panel.paint(Role::Active, &format!("={} to retain", f.retained)));
    }
    if include_skipped && f.skipped > 0 {
        parts.push(panel.paint(Role::Meta, &format!("{} skipped", f.skipped)));
    }
    if f.undetermined > 0 {
        parts.push(panel.paint(Role::Danger, &format!("{} undetermined", f.undetermined)));
    }
    parts
}

/// Plan-screen footer: `✓ plan: +N to add · Xs` (+ AC17 `, K undetermined`) plus
/// a dim `next: wxctl apply -f <config>` hint; failure with non-zero counts →
/// `✗ plan failed · +N to add · K undetermined · run <id> — wxctl debug`;
/// failure with all-zero counts (e.g. validation failure) → `✗ plan failed ·
/// run <id> — wxctl debug`.
pub fn render_footer(panel: &Panel, f: &Footer) -> Vec<String> {
    let dot = panel.g("dot");
    let dur = format_duration(f.duration_ms);
    match f.outcome {
        Outcome::Failed => {
            let parts = build_count_parts(panel, f, false);
            let line = if parts.is_empty() {
                format!("{} {} {} run {} {} wxctl debug", panel.paint(Role::Danger, panel.g("cross")), panel.paint(Role::Heading, &format!("{} failed", f.command)), panel.paint(Role::Meta, dot), f.run_id, panel.paint(Role::Meta, panel.g("emdash")))
            } else {
                let counts = dot_join(panel, dot, &parts);
                format!("{} {} {} {} {} run {} {} wxctl debug", panel.paint(Role::Danger, panel.g("cross")), panel.paint(Role::Heading, &format!("{} failed", f.command)), panel.paint(Role::Meta, dot), counts, panel.paint(Role::Meta, dot), f.run_id, panel.paint(Role::Meta, panel.g("emdash")))
            };
            vec![String::new(), line]
        }
        Outcome::PlanNoChanges => {
            let line = format!("{} {} {} {}", panel.paint(Role::Success, panel.g("check")), panel.paint(Role::Heading, &format!("{}: no changes", f.command)), panel.paint(Role::Meta, dot), panel.paint(Role::Meta, &dur));
            vec![String::new(), line]
        }
        Outcome::PlanOk => {
            let parts = build_count_parts(panel, f, true);
            let verb = panel.paint(Role::Heading, &format!("{}:", f.command));
            let line = format!("{} {} {} {} {}", panel.paint(Role::Success, panel.g("check")), verb, dot_join(panel, dot, &parts), panel.paint(Role::Meta, dot), panel.paint(Role::Meta, &dur));
            let hint = format!("  {}", panel.paint(Role::Meta, &format!("next: wxctl apply -f {}", f.config_hint)));
            vec![String::new(), line, hint]
        }
    }
}

/// Fixed column widths for the `▌ Execution` table. Shared by the static render
/// and the live Animator effects so the header, the streaming live rows, and the
/// final rows all land on the same grid (no column "snap" when the table settles).
#[derive(Clone)]
pub struct ExecWidths {
    pub kind_w: usize,
    pub name_w: usize,
    pub action_w: usize,
}

impl ExecWidths {
    /// Derive widths from already-completed rows (the static render path).
    pub fn from_rows(panel: &Panel, rows: &[ExecRow]) -> Self {
        let kind_w = rows.iter().map(|r| r.kind.chars().count()).max().unwrap_or(4).max(4);
        // Name column includes the Terraform-style `[id=…]` annotation, so it's sized
        // to `name + [id=…]` and Action/Time still align past it.
        let name_w = rows.iter().map(|r| r.name.chars().count() + id_annotation(r.id.as_deref()).chars().count()).max().unwrap_or(4).max(4);
        // Action column = widest past-tense label (`recreated`), floored at the `Action` header.
        let action_w = rows.iter().map(|r| exec_marker_attrs(panel, r.marker).1.chars().count()).max().unwrap_or(6).max("Action".len());
        Self { kind_w, name_w, action_w }
    }

    /// Build widths directly (the live path, where `kind_w`/`name_w`/`action_w` are
    /// projected from the planned resource set + a reserved `[id=…]` slot).
    pub fn new(kind_w: usize, name_w: usize, action_w: usize) -> Self {
        Self { kind_w: kind_w.max(4), name_w: name_w.max(4), action_w: action_w.max("Action".len()) }
    }
}

/// Pre-painted, pre-measured cells for one `▌ Execution` row, assembled by
/// [`exec_row_line`]. Color is applied by the caller; the `*_vis` fields carry the
/// *visible* (uncolored) widths used for padding so ANSI bytes never skew columns.
pub struct ExecCells<'a> {
    pub connector: &'a str,
    pub marker: &'a str,
    pub kind: &'a str,
    pub name: &'a str,
    pub id_painted: &'a str,
    pub id_vis: usize,
    pub action_painted: &'a str,
    pub action_vis: usize,
    pub time_painted: &'a str,
    pub suffix: &'a str,
}

/// The dim column-header line for the `▌ Execution` table. Leading pad of 10 =
/// indent(4) plus connector(2) plus space(1) plus marker(1) plus gap(2), aligning
/// the labels over the data columns; `Time` sits over the duration column.
pub fn exec_header_line(panel: &Panel, w: &ExecWidths) -> String {
    panel.paint(Role::Meta, &format!("{}{:<kw$}  {:<nw$}  {:<aw$}  Time", " ".repeat(10), "Type", "Name", "Action", kw = w.kind_w, nw = w.name_w, aw = w.action_w))
}

/// Assemble one `▌ Execution` row on the fixed `w` grid from pre-painted cells.
/// Shared by the static render and the live Animator effects so every row — running
/// or done — lands on the same columns. Layout:
/// `    {connector} {marker}  {kind:<kind_w}  {name}{id}{pad}  {action}{pad}  {time}{suffix}`.
pub fn exec_row_line(w: &ExecWidths, c: &ExecCells) -> String {
    let name_pad = " ".repeat(w.name_w.saturating_sub(c.name.chars().count() + c.id_vis));
    let action_pad = " ".repeat(w.action_w.saturating_sub(c.action_vis));
    format!("    {} {}  {:<kw$}  {}{}{}  {}{}  {}{}", c.connector, c.marker, c.kind, c.name, c.id_painted, name_pad, c.action_painted, action_pad, c.time_painted, c.suffix, kw = w.kind_w)
}

/// Render one completed `▌ Execution` row (DAG connector + past-tense marker + name +
/// `[id=…]` + action + duration) on the fixed grid. Shared by the static render and the
/// live "done" Animator effect, so a row drawn live is byte-identical to its final form.
pub fn exec_done_row(panel: &Panel, w: &ExecWidths, r: &ExecRow) -> String {
    let connector = panel.paint(Role::Meta, panel.g(if r.last { "ell" } else { "tee" }));
    let (glyph, action_label, role) = exec_marker_attrs(panel, r.marker);
    let marker = panel.paint(role, glyph);
    let action = panel.paint(role, action_label);
    let dur = panel.paint(Role::Meta, &format_duration(r.duration_ms));
    let id_annot = id_annotation(r.id.as_deref());
    let id_painted = if id_annot.is_empty() { String::new() } else { panel.paint(Role::Meta, &id_annot) };
    let suffix = changed_fields_suffix(panel, &r.changed_fields);
    let cells = ExecCells { connector: &connector, marker: &marker, kind: &r.kind, name: &r.name, id_painted: &id_painted, id_vis: id_annot.chars().count(), action_painted: &action, action_vis: action_label.chars().count(), time_painted: &dur, suffix: &suffix };
    exec_row_line(w, &cells)
}

/// `▌ Execution` section: one static row per completed resource, with a DAG
/// connector (`├─` / `└─` for the last row), the past-tense marker + resource,
/// and a duration in a compact column just past Action (not stretched to the
/// panel edge). Only the marker + action carry color (mirrors AC10 for the plan
/// Changes section); the connector is dim.
pub fn render_execution(panel: &Panel, section: &ExecutionSection) -> Vec<String> {
    if section.rows.is_empty() {
        return Vec::new();
    }
    let w = ExecWidths::from_rows(panel, &section.rows);
    render_execution_with_widths(panel, section, &w)
}

/// As [`render_execution`], but on a caller-supplied fixed grid — so the final
/// settled table reuses the same widths the live Animator drew with.
pub fn render_execution_with_widths(panel: &Panel, section: &ExecutionSection, w: &ExecWidths) -> Vec<String> {
    if section.rows.is_empty() {
        return Vec::new();
    }
    let mut out = vec![panel.section("Execution", None)];
    out.push(exec_header_line(panel, w));
    for r in &section.rows {
        out.push(exec_done_row(panel, w, r));
    }
    out
}

/// Execution-screen footer: `✓ apply: +N created · Xs` (+ created URLs under it)
/// on success; `✗ apply failed · run <id> — wxctl debug` on failure. `test`
/// passes its pass/fail counts as created/failed for a uniform shape.
pub fn render_exec_footer(panel: &Panel, f: &ExecFooter) -> Vec<String> {
    let dot = panel.g("dot");
    let dur = format_duration(f.duration_ms);
    match f.outcome {
        ExecOutcome::Failed => {
            let line = format!("{} {} {} run {} {} wxctl debug", panel.paint(Role::Danger, panel.g("cross")), panel.paint(Role::Heading, &format!("{} failed", f.command)), panel.paint(Role::Meta, dot), f.run_id, panel.paint(Role::Meta, panel.g("emdash")));
            vec![String::new(), line]
        }
        ExecOutcome::Ok => {
            let mut parts: Vec<String> = Vec::new();
            if f.created > 0 {
                parts.push(panel.paint(Role::Success, &format!("+{} created", f.created)));
            }
            if f.updated > 0 {
                parts.push(panel.paint(Role::Caution, &format!("~{} updated", f.updated)));
            }
            if f.deleted > 0 {
                parts.push(panel.paint(Role::Danger, &format!("-{} deleted", f.deleted)));
            }
            if parts.is_empty() {
                parts.push(panel.paint(Role::Meta, "no changes"));
            }
            let verb = panel.paint(Role::Heading, &format!("{}:", f.command));
            let line = format!("{} {} {} {} {}", panel.paint(Role::Success, panel.g("check")), verb, dot_join(panel, dot, &parts), panel.paint(Role::Meta, dot), panel.paint(Role::Meta, &dur));
            let mut out = vec![String::new(), line];
            if !f.urls.is_empty() {
                out.push(String::new());
                out.push(format!("  {}", panel.paint(Role::Meta, "created resources:")));
                for u in &f.urls {
                    out.push(format!("    {}  {}", u.name, panel.paint(Role::Active, &u.url)));
                }
            }
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::panel::theme::{ColorMode, Theme};
    use crate::output::sections::{ChangeRow, ErrorBlock, PipelineRow};

    fn plain(width: usize) -> Panel {
        Panel::new(Theme::new(ColorMode::Plain), width, GlyphSet::Unicode)
    }

    #[test]
    fn pipeline_failed_row_uses_cross_not_check() {
        let p = plain(80);
        let s = PipelineSection { rows: vec![PipelineRow { stage: "validation".into(), status: "failed".into(), duration_ms: None, detail: None }] };
        let lines = render_pipeline(&p, &s).join("\n");
        assert!(lines.contains("\u{2717} validation"), "failed stage shows ✗: {lines}");
        assert!(!lines.contains("\u{2713} validation"), "failed stage must not show ✓");
    }

    #[test]
    fn pipeline_detail_renders_on_completed_reconciliation_only() {
        let p = plain(80);
        let s = PipelineSection { rows: vec![PipelineRow { stage: "reconciliation".into(), status: "completed".into(), duration_ms: Some(840), detail: Some("5 reconciled".into()) }, PipelineRow { stage: "planning".into(), status: "completed".into(), duration_ms: Some(15), detail: None }] };
        let lines = render_pipeline(&p, &s).join("\n");
        let recon = lines.lines().find(|l| l.contains("reconciliation")).expect("recon row");
        let plan = lines.lines().find(|l| l.contains("planning")).expect("planning row");
        assert!(recon.contains("5 reconciled"), "completed reconciliation row carries the count: {recon:?}");
        assert!(recon.contains("0.8s"), "row still shows its duration after the detail: {recon:?}");
        assert!(!plan.contains("reconciled"), "non-reconciliation rows carry no detail: {plan:?}");
    }

    #[test]
    fn changes_footnote_present_only_with_uncertain_rows() {
        let p = plain(80);
        let confident = ChangesSection { rows: vec![ChangeRow { marker: ChangeMarker::Add, kind: "tool".into(), name: "t".into(), action: "create".into(), changed_fields: vec![] }] };
        assert!(!render_changes(&p, &confident).join("\n").contains("undetermined"));
        let uncertain = ChangesSection { rows: vec![ChangeRow { marker: ChangeMarker::Undetermined, kind: "agent".into(), name: "a".into(), action: "undetermined".into(), changed_fields: vec![] }] };
        let out = render_changes(&p, &uncertain).join("\n");
        assert!(out.contains("undetermined"), "footnote legend present: {out}");
    }

    #[test]
    fn footer_plan_ok_says_to_add_and_next_hint_never_created() {
        let p = plain(80);
        let f = Footer { outcome: Outcome::PlanOk, command: "plan".into(), created: 3, updated: 0, deleted: 0, retained: 0, skipped: 0, undetermined: 0, duration_ms: 1200, run_id: "r".into(), config_hint: "examples/x/config.yaml".into() };
        let out = render_footer(&p, &f).join("\n");
        assert!(out.contains("+3 to add"), "plan-tense add: {out}");
        assert!(out.contains("next: wxctl apply -f examples/x/config.yaml"), "next hint: {out}");
        assert!(!out.contains("created"), "plan footer must never say 'created'");
    }

    #[test]
    fn footer_failed_carries_run_id_and_debug_hint() {
        let p = plain(80);
        let f = Footer { outcome: Outcome::Failed, command: "plan".into(), created: 0, updated: 0, deleted: 0, retained: 0, skipped: 0, undetermined: 0, duration_ms: 400, run_id: "20260612-000000-plan-abc123".into(), config_hint: "x".into() };
        let out = render_footer(&p, &f).join("\n");
        assert!(out.contains("plan failed"), "failure verb: {out}");
        assert!(out.contains("20260612-000000-plan-abc123"), "run id present: {out}");
        assert!(out.contains("wxctl debug"), "debug hint present: {out}");
    }

    #[test]
    fn errors_block_single_render_has_code_and_fix() {
        let p = plain(80);
        let s = ErrorsSection { blocks: vec![ErrorBlock { stage: "validation".into(), code: "WXCTL-V001".into(), kind: Some("agent".into()), name: Some("broken".into()), field_path: None, message: "Missing required field: name".into(), fix: "Add the name field".into() }] };
        let out = render_errors(&p, &s).join("\n");
        assert!(out.contains("agent/broken"), "resource head: {out}");
        assert!(out.contains("WXCTL-V001"), "code: {out}");
        assert!(out.contains("Add the name field"), "fix: {out}");
    }

    #[test]
    fn execution_rows_use_dag_connectors_last_row_is_ell() {
        let p = plain(80);
        let s = ExecutionSection {
            rows: vec![
                ExecRow { marker: ExecMarker::Created, kind: "tool".into(), name: "a".into(), changed_fields: vec![], duration_ms: 1200, last: false, id: Some("tool-abc123".into()) },
                ExecRow { marker: ExecMarker::Created, kind: "agent".into(), name: "b".into(), changed_fields: vec![], duration_ms: 800, last: true, id: None },
            ],
        };
        let lines = render_execution(&p, &s).join("\n");
        assert!(lines.contains("\u{251c}\u{2500}"), "non-last row uses ├─: {lines}");
        assert!(lines.contains("\u{2514}\u{2500}"), "last row uses └─: {lines}");
        assert!(lines.contains("created"), "past-tense action present");
        // A row with an id renders the `[id=…]` suffix; a row without one does not.
        assert!(lines.contains("[id=tool-abc123]"), "row with id renders [id=…] suffix: {lines}");
        assert_eq!(lines.matches("[id=").count(), 1, "only the row with Some(id) carries the suffix: {lines}");
    }

    #[test]
    fn exec_footer_ok_lists_created_urls() {
        let p = plain(80);
        let f = ExecFooter { outcome: ExecOutcome::Ok, command: "apply".into(), created: 2, updated: 0, deleted: 0, failed: 0, duration_ms: 4200, urls: vec![CreatedUrl { name: "agent.httpbin_agent".into(), url: "https://x/agents/123".into() }], run_id: "r".into() };
        let out = render_exec_footer(&p, &f).join("\n");
        assert!(out.contains("+2 created"), "apply-tense created count: {out}");
        assert!(out.contains("created resources:"), "url section header present");
        assert!(out.contains("https://x/agents/123"), "url listed: {out}");
    }

    #[test]
    fn exec_footer_failed_carries_run_id_and_debug_hint() {
        let p = plain(80);
        let f = ExecFooter { outcome: ExecOutcome::Failed, command: "apply".into(), created: 0, updated: 0, deleted: 0, failed: 1, duration_ms: 900, urls: vec![], run_id: "20260612-000000-apply-abc123".into() };
        let out = render_exec_footer(&p, &f).join("\n");
        assert!(out.contains("apply failed"), "failure verb: {out}");
        assert!(out.contains("20260612-000000-apply-abc123"), "run id present: {out}");
        assert!(out.contains("wxctl debug"), "debug hint present: {out}");
    }
}
