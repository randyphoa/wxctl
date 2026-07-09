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

/// Maximum visible chars of a backend id shown in a row's `[id=…]` annotation, sized to a
/// 36-char UUID. Longer ids (CP4D hrefs, composite ids) are middle-truncated; the full value
/// stays in the exec footer's `created resources:` list and in run records.
const ID_DISPLAY_MAX: usize = 36;
/// Minimum id chars kept when the Name column is width-squeezed — below this the id stops
/// being recognizable, so the row is allowed to overflow the panel instead.
const ID_DISPLAY_MIN: usize = 16;
/// Visible overhead of the annotation around the id itself: `"  [id="` (6) + `"]"` (1).
const ID_ANNOT_OVERHEAD: usize = 7;

/// Middle-truncate `s` to `max` visible chars, keeping head and tail around `ell` — both
/// ends of an href or composite id carry signal. No-op when `s` already fits.
fn middle_truncate(s: &str, max: usize, ell: &str) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(ell.chars().count()).max(2);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let h: String = s.chars().take(head).collect();
    let t: String = s.chars().skip(n - tail).collect();
    format!("{h}{ell}{t}")
}

/// Visible width the `[id=…]` annotation gets on a row: the full annotation when it fits the
/// Name-column budget, otherwise squeezed to the leftover space — but never below the
/// `ID_DISPLAY_MIN` floor and never wider than the `ID_DISPLAY_MAX` slot (so the live grid,
/// which reserves that slot before ids arrive, never shifts when one lands).
pub(crate) fn id_annot_width(name_len: usize, full_annot: usize, name_id_cap: usize) -> usize {
    if full_annot == 0 {
        return 0;
    }
    let slot = full_annot.min(ID_ANNOT_OVERHEAD + ID_DISPLAY_MAX);
    let avail = name_id_cap.saturating_sub(name_len);
    slot.min(avail.max(ID_ANNOT_OVERHEAD + ID_DISPLAY_MIN))
}

/// The `[id=…]` slot the live grid reserves for a not-yet-known id (assumes a UUID), on the
/// same budget rule the renderer truncates with — projection and settled rows stay aligned.
pub(crate) fn id_slot_reserve(name_len: usize, name_id_cap: usize) -> usize {
    id_annot_width(name_len, ID_ANNOT_OVERHEAD + ID_DISPLAY_MAX, name_id_cap)
}

/// As [`id_annotation`], but middle-truncated to the width [`id_annot_width`] grants the row.
fn id_annotation_fitted(panel: &Panel, name_len: usize, id: Option<&str>, name_id_cap: usize) -> String {
    let full = id_annotation(id);
    let full_w = full.chars().count();
    if full.is_empty() {
        return full;
    }
    let w = id_annot_width(name_len, full_w, name_id_cap);
    if w >= full_w {
        return full;
    }
    let ell = ascii_or(panel, "\u{2026}", "...");
    format!("  [id={}]", middle_truncate(id.unwrap_or_default(), w - ID_ANNOT_OVERHEAD, ell))
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
    /// Max visible chars the Name column (`name + [id=…]`) may occupy before ids are
    /// middle-truncated — derived from the panel width via [`ExecWidths::name_id_budget`].
    pub name_id_cap: usize,
}

impl ExecWidths {
    /// The Name-column budget for a panel width: what's left after the fixed row overhead
    /// (indent 10 + three 2-char gaps + a ~7-char Time column) and the Kind/Action columns.
    pub fn name_id_budget(panel_width: usize, kind_w: usize, action_w: usize) -> usize {
        panel_width.saturating_sub(23 + kind_w + action_w)
    }

    /// Derive widths from already-completed rows (the static render path).
    pub fn from_rows(panel: &Panel, rows: &[ExecRow]) -> Self {
        let kind_w = rows.iter().map(|r| r.kind.chars().count()).max().unwrap_or(4).max(4);
        // Action column = widest past-tense label (`recreated`), floored at the `Action` header.
        let action_w = rows.iter().map(|r| exec_marker_attrs(panel, r.marker).1.chars().count()).max().unwrap_or(6).max("Action".len());
        // Name column includes the Terraform-style `[id=…]` annotation, so it's sized to
        // `name + [id=…]` — with the annotation capped to the panel-width budget so one long
        // backend id (a CP4D href, a composite id) can't wrap every row in the table.
        let name_id_cap = Self::name_id_budget(panel.width, kind_w, action_w);
        let name_w = rows
            .iter()
            .map(|r| {
                let name_len = r.name.chars().count();
                name_len + id_annot_width(name_len, id_annotation(r.id.as_deref()).chars().count(), name_id_cap)
            })
            .max()
            .unwrap_or(4)
            .max(4);
        Self { kind_w, name_w, action_w, name_id_cap }
    }

    /// Build widths directly (the live path, where `kind_w`/`name_w`/`action_w` are
    /// projected from the planned resource set + a reserved `[id=…]` slot). Ids still cap
    /// at the `ID_DISPLAY_MAX` UUID slot; the width-derived squeeze only applies once a
    /// budget is attached via [`ExecWidths::with_name_id_cap`].
    pub fn new(kind_w: usize, name_w: usize, action_w: usize) -> Self {
        Self { kind_w: kind_w.max(4), name_w: name_w.max(4), action_w: action_w.max("Action".len()), name_id_cap: usize::MAX }
    }

    /// Attach the panel-width Name-column budget (the live path computes it alongside the
    /// projected widths so live rows truncate ids exactly like the settled table).
    pub fn with_name_id_cap(mut self, cap: usize) -> Self {
        self.name_id_cap = cap;
        self
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
    let id_annot = id_annotation_fitted(panel, r.name.chars().count(), r.id.as_deref(), w.name_id_cap);
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
            if f.retained > 0 {
                parts.push(panel.paint(Role::Active, &format!("={} retained", f.retained)));
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
    fn execution_long_id_middle_truncated_so_rows_fit_width() {
        let p = plain(110);
        let href = "/v2/assets/e2b6e1a2-4c5d-4e07-abb1-73bb666a2c70?project_id=7039e7bb-44b5-4da8-8a29-5eee3932ed07";
        let s = ExecutionSection {
            rows: vec![
                ExecRow { marker: ExecMarker::Created, kind: "orchestrate_connection".into(), name: "watsonx_credentials".into(), changed_fields: vec![], duration_ms: 3700, last: false, id: Some(href.into()) },
                ExecRow { marker: ExecMarker::Created, kind: "model".into(), name: "watsonx_gpt_oss".into(), changed_fields: vec![], duration_ms: 900, last: true, id: Some("14ac41e1-5e4f-466b-a688-a957e5b602d2".into()) },
            ],
        };
        let lines = render_execution(&p, &s);
        for line in &lines {
            assert!(line.chars().count() <= 110, "row fits the panel width ({} chars): {line:?}", line.chars().count());
        }
        let out = lines.join("\n");
        assert!(out.contains('\u{2026}'), "long id is middle-truncated with an ellipsis: {out}");
        assert!(out.contains("[id=/v2/assets/"), "truncated id keeps its head: {out}");
        assert!(out.contains("32ed07]"), "truncated id keeps its tail: {out}");
        assert!(out.contains("[id=14ac41e1-5e4f-466b-a688-a957e5b602d2]"), "a UUID that fits renders whole: {out}");
    }

    #[test]
    fn execution_squeezed_id_keeps_recognizable_floor() {
        // Degenerate width: the budget leaves less than the floor — the id keeps
        // ID_DISPLAY_MIN chars (the row may overflow) instead of vanishing.
        let p = plain(60);
        let s = ExecutionSection { rows: vec![ExecRow { marker: ExecMarker::Created, kind: "orchestrate_connection".into(), name: "watsonx_credentials".into(), changed_fields: vec![], duration_ms: 100, last: true, id: Some("e2b6e1a2-4c5d-4e07-abb1-73bb666a2c70".into()) }] };
        let out = render_execution(&p, &s).join("\n");
        let annot = out.split("[id=").nth(1).and_then(|s| s.split(']').next()).expect("id annotation present");
        assert_eq!(annot.chars().count(), ID_DISPLAY_MIN, "squeezed id keeps exactly the floor: {annot:?}");
        assert!(annot.contains('\u{2026}'), "floor id is still middle-truncated: {annot:?}");
    }

    #[test]
    fn exec_footer_ok_lists_created_urls() {
        let p = plain(80);
        let f = ExecFooter { outcome: ExecOutcome::Ok, command: "apply".into(), created: 2, updated: 0, deleted: 0, retained: 0, failed: 0, duration_ms: 4200, urls: vec![CreatedUrl { name: "agent.httpbin_agent".into(), url: "https://x/agents/123".into() }], run_id: "r".into() };
        let out = render_exec_footer(&p, &f).join("\n");
        assert!(out.contains("+2 created"), "apply-tense created count: {out}");
        assert!(out.contains("created resources:"), "url section header present");
        assert!(out.contains("https://x/agents/123"), "url listed: {out}");
    }

    #[test]
    fn exec_footer_failed_carries_run_id_and_debug_hint() {
        let p = plain(80);
        let f = ExecFooter { outcome: ExecOutcome::Failed, command: "apply".into(), created: 0, updated: 0, deleted: 0, retained: 0, failed: 1, duration_ms: 900, urls: vec![], run_id: "20260612-000000-apply-abc123".into() };
        let out = render_exec_footer(&p, &f).join("\n");
        assert!(out.contains("apply failed"), "failure verb: {out}");
        assert!(out.contains("20260612-000000-apply-abc123"), "run id present: {out}");
        assert!(out.contains("wxctl debug"), "debug hint present: {out}");
    }

    #[test]
    fn exec_footer_retained_only_destroy_shows_retained_not_no_changes() {
        let p = plain(80);
        let f = ExecFooter { outcome: ExecOutcome::Ok, command: "destroy".into(), created: 0, updated: 0, deleted: 0, retained: 4, failed: 0, duration_ms: 2100, urls: vec![], run_id: "r".into() };
        let out = render_exec_footer(&p, &f).join("\n");
        assert!(out.contains("=4 retained"), "retain-only destroy names the kept scope: {out}");
        assert!(!out.contains("no changes"), "retain-only destroy is not 'no changes': {out}");
    }
}
