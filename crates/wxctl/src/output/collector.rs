use crate::output::color::{Color, Theme};
use crate::output::formatters::format_error_stream_line;
use crate::output::formatters::stage::{format_stage_spinner_msg, format_substage};
use crate::output::formatters::summary::OperationSummary;
use crate::output::panel::animate::{Animator, AnimatorRow, Effect};
use crate::output::panel::glyphs::{GlyphSet, glyph};
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::unicode_capable;
use crate::output::panel_render::{ExecWidths, exec_marker_action_label, id_slot_reserve, render_advisories, render_changes, render_errors, render_exec_footer, render_execution, render_footer};
use crate::output::sections::{AdvisoriesSection, AdvisoryBlock, ChangeMarker, ChangeRow, ChangesSection, CreatedUrl, ErrorBlock, ErrorsSection, ExecFooter, ExecMarker, ExecOutcome, ExecRow, ExecutionSection, Footer, Outcome, PipelineRow, PipelineSection};
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use wxctl_core::logging::*;

/// Set when the collector renders any styled error block during a command.
/// `main` reads this to suppress the redundant top-level `Error: …` anyhow
/// dump — the failure is already shown once in the styled output. Process-global
/// (not on the collector) because the per-command collector + its CollectorGuard
/// are dropped before `main` inspects the command result.
static STYLED_ERROR_RENDERED: AtomicBool = AtomicBool::new(false);

/// Record that a styled error block was rendered for the active command.
pub fn mark_styled_error_rendered() {
    STYLED_ERROR_RENDERED.store(true, Ordering::Relaxed);
}

/// Whether the collector rendered a styled error block. Read by `main` to
/// decide whether to suppress the top-level anyhow re-print.
pub fn styled_error_rendered() -> bool {
    STYLED_ERROR_RENDERED.load(Ordering::Relaxed)
}

/// Collects and formats events for terminal output
pub struct OutputCollector {
    operation_id: String,
    stages: Vec<StageEvent>,
    decisions: Vec<DecisionEvent>,
    buffered_decisions: Vec<DecisionEvent>,
    errors: Vec<ErrorEvent>,
    advisories: Vec<AdvisoryBlock>,
    /// Dedup set for display-only deduplication of error events.  Keyed by
    /// "res:kind:name" for resource-scoped errors, "bare:code:first_line" for
    /// resource-less ones.  E000 rollups are discarded before any key is built.
    error_keys_seen: HashSet<String>,
    summary: OperationSummary,
    stage_count: usize,
    current_stage_num: usize,
    /// Maps "kind.name" -> decision type for execution progress display
    decision_map: HashMap<String, String>,
    /// Maps "kind.name" -> field diffs for update operations
    decision_diffs: HashMap<String, Vec<wxctl_core::logging::FieldDiff>>,
    /// Whether this operation has an execution stage (apply/destroy skip decision list)
    has_execution_stage: bool,
    theme: Theme,
    /// Glyph set resolved once (aligned with `theme` via `unicode_capable`) in
    /// `new`. Read by `panel()`, `print_header`, `record_test_complete`, and the
    /// error stream line so every live glyph agrees with the color mode.
    glyphs: GlyphSet,
    /// Run id (e.g. `20260612-153042-plan-a1b2c3`) for the failure footer's
    /// `run <id> — wxctl debug` hint. Empty until `set_run_id`.
    run_id: String,
    /// Config paths joined for the `next: wxctl apply -f <config>` hint.
    config_hint: String,
    /// Command name (e.g. "apply", "destroy") used to pick the CounterBar noun.
    command_name: String,
    multi: Option<MultiProgress>,
    stage_spinner: Option<ProgressBar>,
    /// Active per-resource spinners during execution (key: "kind.name" → (ProgressBar, Animator row id))
    execution_spinners: HashMap<String, (ProgressBar, usize)>,
    /// Total resources expected in execution
    execution_resource_count: usize,
    /// Resources completed so far (shared with the Animator's CounterBar effect)
    exec_done: Arc<AtomicUsize>,
    /// Total resources expected in reconciliation (set on `on_reconcile_start`).
    reconciliation_resource_count: usize,
    /// Resources reconciled so far (shared with the Animator's CounterBar effect).
    recon_done: Arc<AtomicUsize>,
    /// Current-resource label shown on the live reconciliation line (`<kind> <name>`).
    /// Carried into the CounterBar so the ticker can paint it; advanced per resource.
    recon_label: Arc<Mutex<String>>,
    /// Animator row id of the current non-execution stage spinner line, or `None`.
    /// Stored so `on_reconcile_start` can swap that row's effect to a CounterBar.
    stage_spinner_row: Option<usize>,
    /// Animator drives all live-region effects from one ticker thread
    animator: Animator,
    /// Buffered typed execution rows, flushed at execution-stage close
    exec_rows: Vec<ExecRow>,
    /// Fixed column grid for the live `▌ Execution` table, computed once at execution
    /// stage start from the planned decisions so the header + every streaming row align.
    exec_widths: Option<ExecWidths>,
    /// `kind.name` keys in plan/execution order (the order the skeleton rows are laid out).
    /// The final settled table is reordered to match this so it never reshuffles.
    exec_start_order: Vec<String>,
    /// `kind.name` of the last row in `exec_start_order` — the one that gets the `└─`
    /// connector (known up front so the prefilled skeleton draws the tree correctly).
    exec_last_key: Option<String>,
    /// Set when the command returned `Err` (pipeline/executor failure) before `print_summary`.
    /// The final footer must render `Failed` even when the failure carried no per-resource
    /// error event — e.g. a pre-execution executor abort whose only signal is the dropped
    /// `WXCTL-E000` rollup, which would otherwise leave `has_errors`/`summary.failed` at 0.
    command_failed: bool,
    /// Completed-operation tallies, bumped in `record_operation` and never cleared — the
    /// exec footer's counts source. Deliberately NOT derived from `exec_rows`: the stage-close
    /// `drain_execution_cleanup` `mem::take`s that buffer to render the static Execution table
    /// *before* `print_summary` runs, so tallying rows there always read zero (live-caught:
    /// `ok destroy: no changes` after 5 real deletes).
    completed_created: usize,
    completed_updated: usize,
    completed_deleted: usize,
    /// Machine-readable output mode (`-o json`): suppress every terminal render —
    /// header, stage/substage lines, spinners, summary — so the command's own JSON
    /// document is the only stdout. State collection is unaffected. Also set true
    /// by `--progress none`, which suppresses the panel with no replacement output.
    quiet: bool,
    /// True when stdout carries a machine document that already reports errors
    /// (`-o json`). Distinguishes json-quiet (error is in the JSON → main must
    /// stay silent) from `--progress none` quiet (error is nowhere → main's
    /// stderr fallback must fire). Gates whether `add_error` claims the failure
    /// was surfaced (`mark_styled_error_rendered`).
    machine_output: bool,
}

/// Whether a decision performs a side-effecting operation during execution (gets a spinner + is counted).
fn decision_executes(decision: &str) -> bool {
    !matches!(decision, "NoOp" | "Retain" | "SkipAbsent" | "SkipDeferred")
}

impl OutputCollector {
    pub fn new(operation_id: String, theme: Theme) -> Self {
        // Progress is diagnostics, not machine output: the live panel draws to
        // stderr, leaving stdout for `--output json`. `progress_mode()` (set once
        // in `main` from `--progress` / `WXCTL_PROGRESS`) decides whether to
        // animate a live region, stream plain lines, or stay silent.
        let progress = crate::output::progress::progress_mode();
        let multi = if progress.animates(theme.is_plain()) { Some(MultiProgress::with_draw_target(indicatif::ProgressDrawTarget::stderr())) } else { None };
        let glyphs = GlyphSet::resolve(unicode_capable(&theme));
        let animator = Animator::new(theme.clone(), glyphs);
        Self {
            operation_id,
            stages: Vec::new(),
            decisions: Vec::new(),
            buffered_decisions: Vec::new(),
            errors: Vec::new(),
            advisories: Vec::new(),
            error_keys_seen: HashSet::new(),
            summary: OperationSummary::default(),
            stage_count: 4,
            current_stage_num: 0,
            decision_map: HashMap::new(),
            decision_diffs: HashMap::new(),
            has_execution_stage: false,
            theme,
            glyphs,
            run_id: String::new(),
            config_hint: String::new(),
            command_name: String::new(),
            multi,
            stage_spinner: None,
            execution_spinners: HashMap::new(),
            execution_resource_count: 0,
            exec_done: Arc::new(AtomicUsize::new(0)),
            reconciliation_resource_count: 0,
            recon_done: Arc::new(AtomicUsize::new(0)),
            recon_label: Arc::new(Mutex::new(String::new())),
            stage_spinner_row: None,
            animator,
            exec_rows: Vec::new(),
            exec_widths: None,
            exec_start_order: Vec::new(),
            exec_last_key: None,
            command_failed: false,
            completed_created: 0,
            completed_updated: 0,
            completed_deleted: 0,
            // `--progress none` suppresses the panel outright; errors still reach
            // the terminal via main's stderr error path. `-o json` sets `quiet`
            // too (via set_quiet), so the two suppression paths converge — but
            // json also sets `machine_output`, which none does not.
            quiet: progress.is_quiet(),
            machine_output: false,
        }
    }

    /// Project the fixed `▌ Execution` column grid from the planned (executing) decisions,
    /// before any row has run. Sizes the Name column to `name + a reserved [id=…] slot`
    /// (a UUID, squeezed to the panel-width budget exactly like the renderer truncates)
    /// for id-returning operations (everything but deletes) so the columns don't shift
    /// when the first backend id arrives.
    fn compute_exec_widths(&self) -> ExecWidths {
        let mut kind_w = 4usize;
        let mut action_w = "Action".len();
        for d in &self.decisions {
            if !decision_executes(&d.decision) {
                continue;
            }
            kind_w = kind_w.max(d.resource_type.chars().count());
            action_w = action_w.max(exec_marker_action_label(decision_to_exec_marker(&d.decision)).chars().count());
        }
        let name_id_cap = ExecWidths::name_id_budget(Panel::resolve_width(), kind_w, action_w);
        let mut name_w = 4usize;
        for d in &self.decisions {
            if !decision_executes(&d.decision) {
                continue;
            }
            let name_len = d.resource_name.chars().count();
            let id_reserve = if decision_to_exec_marker(&d.decision) == ExecMarker::Deleted { 0 } else { id_slot_reserve(name_len, name_id_cap) };
            name_w = name_w.max(name_len + id_reserve);
        }
        ExecWidths::new(kind_w, name_w, action_w).with_name_id_cap(name_id_cap)
    }

    /// Set the total number of stages for this operation
    pub fn set_stage_count(&mut self, count: usize) {
        self.stage_count = count;
    }

    /// Mark whether this operation has an execution stage (apply/destroy).
    /// When true, the decision list is suppressed — execution progress shows the same info with timing.
    pub fn set_has_execution_stage(&mut self, v: bool) {
        self.has_execution_stage = v;
    }

    /// Set the run id used by the failure footer.
    pub fn set_run_id(&mut self, run_id: String) {
        self.run_id = run_id;
    }

    /// Set the command name and config hint.
    pub fn set_command(&mut self, command_name: String, config_hint: String) {
        self.command_name = command_name;
        self.config_hint = config_hint;
    }

    /// Mark that the command failed (returned `Err`). Called before the final
    /// `print_summary` so the footer renders `Failed` even when the failure never
    /// surfaced a per-resource error event (e.g. a pre-execution executor abort).
    pub fn mark_command_failed(&mut self) {
        self.command_failed = true;
    }

    /// Build a `Panel` from the collector's resolved theme + the env/terminal
    /// width + glyph capability. One place so every section renders consistently.
    fn panel(&self) -> Panel {
        Panel::new(self.theme.clone(), Panel::resolve_width(), self.glyphs)
    }

    /// Enter quiet (machine-readable) mode for `-o json`. Must be set before any
    /// render call. Sets `machine_output` too: stdout carries the JSON document
    /// (which reports errors), so `main` must not also print a stderr fallback.
    pub fn set_quiet(&mut self) {
        self.quiet = true;
        self.machine_output = true;
    }

    /// Route output through MultiProgress when spinners are active, otherwise println
    fn emit(&self, line: &str) {
        if self.quiet {
            return;
        }
        if let Some(ref multi) = self.multi {
            let _ = multi.println(line);
        } else {
            // Panel is diagnostics → stderr, keeping stdout clean for `-o json`.
            eprintln!("{}", line);
        }
    }

    /// Mutate state for a stage transition and return a `StageRenderPlan` the
    /// caller should `execute()` outside the collector lock. Splits state
    /// updates from the slow indicatif calls (`multi.add`, `multi.println`,
    /// `pb.finish_and_clear`) so the collector mutex is never held during
    /// them. Pair with `install_stage_spinner_pb` to attach the resulting PB.
    pub fn add_stage_state(&mut self, event: StageEvent) -> StageRenderPlan {
        let plan = self.build_stage_render_plan(&event);
        self.stages.push(event);
        plan
    }

    fn build_stage_render_plan(&mut self, event: &StageEvent) -> StageRenderPlan {
        let multi = self.multi.clone();
        let theme = self.theme.clone();
        let drain = self.stage_spinner.take();
        // Quiet mode: advance the stage bookkeeping, render nothing (no spinner,
        // no Pipeline rows). Only rendering-feed state is skipped.
        if self.quiet {
            if event.status == "started" {
                self.current_stage_num += 1;
            }
            return StageRenderPlan { multi: None, emit_lines: Vec::new(), drain_spinner: drain, spinner: None };
        }
        if event.status == "started" {
            self.current_stage_num += 1;
            let mut emit_lines = Vec::new();
            if event.stage == "execution" {
                self.execution_resource_count = self.decision_map.values().filter(|d| decision_executes(d.as_str())).count();
                self.exec_done.store(0, Ordering::Relaxed);
                // Project the fixed table grid now so the live ▌ Execution header and every
                // row share columns. Blank line separates it from ▌ Pipeline above.
                let widths = self.compute_exec_widths();
                self.exec_widths = Some(widths);
                // Display order: the prefilled skeleton + the final table both lay out in this
                // order, and the last one gets the `└─` connector. Decisions arrive in topo
                // order (dependencies first); destroy executes in REVERSE-topo (walk back the
                // DAG — dependents deleted before their dependencies), so reverse it to match.
                self.exec_start_order = self.decisions.iter().filter(|d| decision_executes(&d.decision)).map(|d| format!("{}.{}", d.resource_type, d.resource_name)).collect();
                if self.command_name == "destroy" {
                    self.exec_start_order.reverse();
                }
                self.exec_last_key = self.exec_start_order.last().cloned();
                emit_lines.push(String::new());
            }
            return StageRenderPlan { multi, spinner: Some(StageSpinnerArgs { theme, stage_num: self.current_stage_num, stage_count: self.stage_count, stage_name: event.stage.clone() }), emit_lines, drain_spinner: drain };
        }
        let mut lines: Vec<String> = Vec::new();
        if (event.status == "completed" || event.status == "failed") && event.stage != "execution" {
            // All non-execution stage closes render typed Pipeline section rows (plan AND apply/destroy).
            let panel = self.panel();
            if self.current_stage_num == 1 {
                // Open the Pipeline section once, on the first stage to close.
                lines.push(String::new());
                lines.push(panel.section("Pipeline", None));
            }
            let detail = if event.stage == "reconciliation" && event.status == "completed" { Some(format!("{} reconciled", self.reconciliation_resource_count)) } else { None };
            let row = PipelineRow { stage: event.stage.clone(), status: event.status.clone(), duration_ms: event.duration_ms, detail };
            // Render just this row (skip the section header render_pipeline would add).
            let single = PipelineSection { rows: vec![row] };
            let mut row_lines = crate::output::panel_render::render_pipeline(&panel, &single);
            // Drop the section header render_pipeline prepends (first line).
            lines.extend(row_lines.drain(1..));
            // Render Changes only on the plan path after the planning stage closes.
            // Apply/destroy must NOT print a Changes section — their ▌ Execution section
            // shows what actually happened.
            if event.status == "completed" && event.stage == "planning" && !self.has_execution_stage {
                let changes = self.build_changes_section();
                self.buffered_decisions.clear();
                if !changes.rows.is_empty() {
                    lines.push(String::new());
                    lines.extend(render_changes(&panel, &changes));
                }
            }
            // Drop leftover decisions only on the execution path (apply/destroy), where they
            // are never rendered as Changes. On the plan path the buffer must survive from
            // reconciliation-close (where decisions are emitted) to planning-close above —
            // clearing here unconditionally would empty it before render_changes can read it,
            // silently dropping the plan preview. The planning-close branch clears it itself.
            if self.has_execution_stage && !self.buffered_decisions.is_empty() {
                self.buffered_decisions.clear();
            }
        } else if event.stage == "execution" {
            // Execution-stage (apply/destroy) close: skip the inline stage row.
            // The typed ▌ Execution section is emitted by ExecutionCleanupPlan::execute().
            // Errors section + footer carry execution failures.
            if !self.buffered_decisions.is_empty() {
                self.buffered_decisions.clear();
            }
        }
        StageRenderPlan { multi, spinner: None, emit_lines: lines, drain_spinner: drain }
    }

    /// Install the `ProgressBar` produced by `StageRenderPlan::execute()` and
    /// wire it into the Animator. Run after `execute()` — the indicatif handle
    /// exists only after `multi.add` has completed outside the collector lock.
    /// `Animator::register` only acquires the Animator's internal rows lock —
    /// no indicatif calls inside — so this is safe under the collector lock.
    pub fn install_stage_spinner_pb(&mut self, pb: Option<ProgressBar>, is_execution: bool) {
        if let Some(pb) = pb {
            self.animator.start();
            if is_execution {
                let total = self.execution_resource_count;
                // The execution stage line becomes the live ▌ Execution table header: restyle the
                // bar to a bare multi-line message (drop the `[N/N]` prefix) and drive it with
                // ExecSummary (section bar + `<bar> done/total · elapsed` + the column header).
                pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
                pb.set_prefix("");
                let widths = self.exec_widths.clone().unwrap_or_else(|| self.compute_exec_widths());
                self.animator.register(pb.clone(), Effect::ExecSummary { done: self.exec_done.clone(), total, started: std::time::Instant::now(), panel: self.panel(), widths });
                self.stage_spinner_row = None;
            } else {
                let label = self.stages.last().map(|s| s.stage.clone()).unwrap_or_default();
                let row_id = self.animator.register(pb.clone(), Effect::Ellipsis { label });
                self.stage_spinner_row = Some(row_id);
            }
            self.stage_spinner = Some(pb);
        }
    }

    /// Build the plan to prefill the live `▌ Execution` table with a dim `pending` row per
    /// executing resource, in plan order, so the full scope is visible from the first frame
    /// (rows then flip to running/done in place). The `multi.add` calls run in
    /// `PrefillRowsPlan::execute()` outside the collector lock. Inert (empty rows) in plain
    /// mode or before `exec_widths` is set — the final static table still renders.
    pub fn prefill_exec_rows_plan(&self) -> PrefillRowsPlan {
        let widths = self.exec_widths.clone().unwrap_or_else(|| self.compute_exec_widths());
        let panel = self.panel();
        // The row list is just data — projected from the executing decisions, then ordered by
        // `exec_start_order` (topo for apply, reverse-topo for destroy) so the skeleton lays out
        // in the same order the rows will execute. The final (bottom) row gets the `└─` flag.
        let order: HashMap<&str, usize> = self.exec_start_order.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
        let mut rows: Vec<PrefillRow> = self.decisions.iter().filter(|d| decision_executes(&d.decision)).map(|d| PrefillRow { key: format!("{}.{}", d.resource_type, d.resource_name), kind: d.resource_type.clone(), name: d.resource_name.clone(), last: false }).collect();
        rows.sort_by_key(|r| order.get(r.key.as_str()).copied().unwrap_or(usize::MAX));
        if let Some(last) = rows.last_mut() {
            last.last = true;
        }
        // Only create live bars when the Animator is active (TTY); otherwise `execute()` is
        // inert and the final static table renders these rows.
        let (multi, rows_handle) = if self.animator.is_active() && self.multi.is_some() { (self.multi.clone(), Some(self.animator.rows_handle())) } else { (None, None) };
        PrefillRowsPlan { multi, rows_handle, panel, widths, rows }
    }

    /// Store the prefilled per-resource bars (from `PrefillRowsPlan::execute()`) so
    /// `log_start` / `record_operation` can flip them to running / done by row id.
    pub fn install_prefilled_rows(&mut self, installed: Vec<(String, ProgressBar, usize)>) {
        for (key, pb, row_id) in installed {
            self.execution_spinners.insert(key, (pb, row_id));
        }
    }

    /// Drain execution-stage cleanup state (Animator, lingering execution
    /// spinners, buffered typed exec rows). Caller invokes this only when the
    /// completed stage is `execution`; the method itself does not re-check.
    pub fn drain_execution_cleanup(&mut self) -> ExecutionCleanupPlan {
        self.animator.stop();
        let pbs: Vec<ProgressBar> = self.execution_spinners.drain().map(|(_, (pb, _))| pb).collect();
        let mut rows: Vec<ExecRow> = std::mem::take(&mut self.exec_rows);
        // Quiet (machine-readable) mode: the Animator is stopped and the progress bars are
        // cleared above, but emit no static ▌ Execution table — stdout must carry only the
        // machine-readable document (`--output json`). Mirrors the quiet guards in `emit`,
        // `build_stage_render_plan`, and `add_substage_state`.
        if self.quiet {
            return ExecutionCleanupPlan { pbs_to_clear: pbs, rows: Vec::new(), multi: self.multi.clone(), panel: self.panel() };
        }
        // Reorder into execution-start order (matches the live stream) rather than the
        // completion order the rows were buffered in — a stable order so the settled table
        // doesn't reshuffle. Rows with no recorded start (shouldn't happen) sort last.
        let start_index: HashMap<&str, usize> = self.exec_start_order.iter().enumerate().map(|(i, k)| (k.as_str(), i)).collect();
        rows.sort_by_key(|r| start_index.get(format!("{}.{}", r.kind, r.name).as_str()).copied().unwrap_or(usize::MAX));
        if let Some(last) = rows.last_mut() {
            last.last = true;
        }
        ExecutionCleanupPlan { pbs_to_clear: pbs, rows, multi: self.multi.clone(), panel: self.panel() }
    }

    /// Build a `SubstageRenderPlan` (formatted line + cloned multi) for emit
    /// outside the collector lock.
    pub fn add_substage_state(&self, name: &str, duration_ms: Option<u64>) -> SubstageRenderPlan {
        // Quiet mode: an empty line is the suppression sentinel (a real substage
        // line is never empty — `format_substage` always renders the name).
        if self.quiet {
            return SubstageRenderPlan { multi: None, line: String::new() };
        }
        SubstageRenderPlan { multi: self.multi.clone(), line: format_substage(&self.theme, name, duration_ms) }
    }

    /// Add decision event — buffered until reconciliation completes
    pub fn add_decision(&mut self, event: DecisionEvent) {
        self.summary.add_decision(&event.decision);
        // Store decision for execution progress display
        let key = format!("{}.{}", event.resource_type, event.resource_name);
        self.decision_map.insert(key.clone(), event.decision.clone());
        // Store field diffs for update operations (shown in execution progress)
        if !event.field_diffs.is_empty() {
            self.decision_diffs.insert(key, event.field_diffs.clone());
        }
        self.buffered_decisions.push(event.clone());
        self.decisions.push(event);
    }

    /// Add error event — display-only dedup.
    ///
    /// Rules (human renderer only; run records / RUST_LOG sinks are unaffected):
    /// 1. `WXCTL-E000` command-level rollups are dropped entirely — they are the
    ///    footer's job and must never appear as a stream line or Errors block.
    /// 2. Each distinct failure is shown once.  The dedup key is:
    ///    - resource-scoped (`resource_type` + `resource_name` both `Some`):
    ///      `"res:<type>:<name>"` — one entry per failing resource regardless of
    ///      how many wrapper events the engine emits.
    ///    - resource-less (H001 HTTP root, etc.):
    ///      `"bare:<code>:<first line of message>"` — collapsed per unique message.
    /// 3. `summary.failed` is only incremented for events that pass the filter,
    ///    so the footer counter stays accurate.
    pub fn add_error(&mut self, event: ErrorEvent) {
        // Rule 1: drop command-level rollups entirely.
        if event.error_code == "WXCTL-E000" {
            return;
        }
        // Rule 2: build dedup key and skip if already seen.
        let key = match (&event.resource_type, &event.resource_name) {
            (Some(rt), Some(rn)) => format!("res:{}:{}", rt, rn),
            _ => format!("bare:{}:{}", event.error_code, event.message.lines().next().unwrap_or("")),
        };
        if !self.error_keys_seen.insert(key) {
            return;
        }
        // Rule 3: only count / emit / store events that pass the filter.
        self.summary.failed += 1;
        self.emit(&format_error_stream_line(&self.theme, self.glyphs, &event));
        // Claim the failure was surfaced (so `main` suppresses its fallback) only
        // when it actually reached the user: a rendered panel, or the JSON document
        // (`machine_output`). Under `--progress none` the emit above is suppressed
        // and there is no JSON — leave the flag unset so `main`'s stderr fallback
        // still reports the error.
        if !self.quiet || self.machine_output {
            mark_styled_error_rendered();
        }
        self.errors.push(event);
    }

    /// Print operation header with context info
    pub fn print_header(&self, command: &str, resource_count: usize, profile: Option<&str>, config_paths: &[String]) {
        let sep = self.theme.paint(Color::Dim, &format!(" {} ", glyph(self.glyphs, "bullet")));
        let mut line = self.theme.paint(Color::BoldWhite, &format!("wxctl {}", command));

        if let Some(p) = profile {
            line = format!("{}{}{}", line, sep, self.theme.paint(Color::Dim, &format!("profile: {}", p)));
        }

        // Always render the count — including zero, so an empty config (or one that's
        // all `kind: test`, filtered out) reads as a deliberate "nothing to do" rather
        // than silently dropping the segment.
        let count_text = match resource_count {
            0 => "no resources".to_string(),
            1 => "1 resource".to_string(),
            n => format!("{n} resources"),
        };
        line = format!("{}{}{}", line, sep, self.theme.paint(Color::Dim, &count_text));

        let mut out = format!("\n{}", line);

        // Config path(s) live on their own line(s), never the title line: a long path
        // (or several `-f` files) would otherwise overflow the width / pile into one
        // comma-joined run-on. One file per line; continuations align under the first
        // path (8 = width of "config: ").
        if let Some((first, rest)) = config_paths.split_first() {
            out.push_str(&format!("\n{}", self.theme.paint(Color::Dim, &format!("config: {}", first))));
            for p in rest {
                out.push_str(&format!("\n        {}", self.theme.paint(Color::Dim, p)));
            }
        }

        self.emit(&out);
    }

    /// Plan totals now render in the footer (`print_summary`). Kept as a shim so
    /// `plan`/`apply`/`destroy` call sites compile unchanged; emits nothing.
    pub fn print_plan(&self) {}

    /// Print the final `▌ Errors` section + footer. For plan-style commands
    /// (no execution stage) this renders the typed panel sections; commands with
    /// an execution stage render the typed `▌ Execution` footer.
    pub fn print_summary(&self, command: &str) {
        let has_errors = !self.errors.is_empty();
        if !self.has_execution_stage {
            let panel = self.panel();
            // If the plan failed before the planning stage closed (e.g. reconcile
            // error), buffered_decisions were never flushed by the stage-close path.
            // Render Changes here — before Errors — so the partial-failure screen
            // still shows the `!` (Undetermined) row (AC17). No double-render risk:
            // if the planning stage did close cleanly, it already cleared the buffer.
            if !self.buffered_decisions.is_empty() {
                let changes = self.build_changes_section();
                if !changes.rows.is_empty() {
                    self.emit("");
                    for line in render_changes(&panel, &changes) {
                        self.emit(&line);
                    }
                }
            }
            if has_errors {
                self.emit_errors_section(&panel);
            }
            if !self.advisories.is_empty() {
                self.emit_advisories_section(&panel);
            }
            let footer = self.build_footer(command, has_errors);
            for line in render_footer(&panel, &footer) {
                self.emit(&line);
            }
            self.emit("");
            return;
        }
        // ── execution-stage final screen (apply/destroy) ──
        let panel = self.panel();
        if has_errors {
            self.emit_errors_section(&panel);
        }
        let footer = self.build_exec_footer(command, has_errors);
        for line in render_exec_footer(&panel, &footer) {
            self.emit(&line);
        }
        self.emit("");
    }

    /// Emit the typed `▌ Errors` section into the panel. Shared by the plan and
    /// execution final-screen paths.
    fn emit_errors_section(&self, panel: &Panel) {
        let errors = ErrorsSection { blocks: errors_display_subset(&self.errors).into_iter().map(error_event_to_block).collect() };
        self.emit("");
        for line in render_errors(panel, &errors) {
            self.emit(&line);
        }
    }

    /// Set the advisories to render in the `▌ Advisories` section. Called by
    /// `validate` before `finish()`; empty (the default) renders nothing.
    pub fn set_advisories(&mut self, advisories: Vec<AdvisoryBlock>) {
        self.advisories = advisories;
    }

    /// Emit the typed `▌ Advisories` section into the panel (warn-level, non-blocking).
    fn emit_advisories_section(&self, panel: &Panel) {
        let section = AdvisoriesSection { blocks: self.advisories.clone() };
        self.emit("");
        for line in render_advisories(panel, &section) {
            self.emit(&line);
        }
    }

    /// Build the plan footer from the summary counts + outcome.
    fn build_footer(&self, command: &str, has_errors: bool) -> Footer {
        let outcome = if has_errors || self.summary.failed > 0 || self.command_failed {
            Outcome::Failed
        } else if self.summary.created + self.summary.updated + self.summary.deleted + self.summary.retained == 0 {
            Outcome::PlanNoChanges
        } else {
            Outcome::PlanOk
        };
        Footer {
            outcome,
            command: command.to_string(),
            created: self.summary.created,
            updated: self.summary.updated,
            deleted: self.summary.deleted,
            retained: self.summary.retained,
            skipped: self.summary.skipped_absent + self.summary.skipped_deferred,
            undetermined: self.summary.undetermined,
            duration_ms: self.summary.total_duration_ms,
            run_id: self.run_id.clone(),
            config_hint: self.config_hint.clone(),
        }
    }

    /// Build the execution-screen footer for apply/destroy.
    ///
    /// Counts come from the buffered `exec_rows` — one row per *completed* operation —
    /// not the planned `summary` decisions, so an aborted or partially-failed run reports
    /// what actually ran (a pre-execution abort buffers zero rows → all zero). `retained`
    /// stays sourced from the plan (a retain executes no side-effecting op, so it can't
    /// fail). `command_failed` flips the outcome to `Failed` even when the failure carried
    /// no per-resource error event.
    fn build_exec_footer(&self, command: &str, has_errors: bool) -> ExecFooter {
        let outcome = if has_errors || self.summary.failed > 0 || self.command_failed { ExecOutcome::Failed } else { ExecOutcome::Ok };
        let (created, updated, deleted) = self.exec_completion_counts();
        let urls: Vec<CreatedUrl> = self.summary.resource_urls.iter().map(|(name, url)| CreatedUrl { name: name.clone(), url: url.clone() }).collect();
        ExecFooter { outcome, command: command.to_string(), created, updated, deleted, retained: self.summary.retained, failed: self.summary.failed, duration_ms: self.summary.total_duration_ms, urls, run_id: self.run_id.clone() }
    }

    /// `(created, updated, deleted)` operations that actually completed — the counters
    /// `record_operation` bumps, never planned-but-unrun decisions. Kept as dedicated
    /// fields rather than a tally over `exec_rows`, which `drain_execution_cleanup`
    /// `mem::take`s at stage close before the footer builds.
    fn exec_completion_counts(&self) -> (usize, usize, usize) {
        (self.completed_created, self.completed_updated, self.completed_deleted)
    }

    /// Add resource URL to summary
    pub fn add_resource_url(&mut self, name: String, url: String) {
        self.summary.add_resource_url(name, url);
    }

    /// Set total duration
    pub fn set_duration(&mut self, duration_ms: u64) {
        self.summary.total_duration_ms = duration_ms;
    }

    /// Log operation start — mutates state and returns a `StartSpinnerPlan` the
    /// caller must `execute()` outside the collector lock (it calls `multi.add`).
    /// After `execute()`, pass the result to `install_exec_spinner_pb` under lock.
    pub fn log_start(&mut self, kind: &str, name: &str) -> StartSpinnerPlan {
        tracing::debug!(target: "wxctl::substage::execution", operation_id = %self.operation_id, kind = %kind, name = %name, "starting operation");
        let key = format!("{}.{}", kind, name);
        let decision = self.decision_map.get(&key).map(|s| s.as_str()).unwrap_or("Create");
        if !decision_executes(decision) {
            return StartSpinnerPlan::noop();
        }
        let last = self.exec_last_key.as_deref() == Some(key.as_str());
        // Skeleton path: the row was prefilled at execution start — flip it from `pending` to
        // `running` in place (set_effect only locks the Animator rows; no indicatif → lock-safe).
        if let Some(&(_, row_id)) = self.execution_spinners.get(&key)
            && let Some(w) = self.exec_widths.clone()
        {
            self.animator.set_effect(row_id, Effect::ExecRowLive { panel: self.panel(), widths: w, kind: kind.to_string(), name: name.to_string(), started: std::time::Instant::now(), last });
            return StartSpinnerPlan::noop();
        }
        // Fallback (no prefilled row — e.g. plain/degraded): stream a fresh bar. The row renders
        // its own indent + connector, so the bar template is bare `{msg}`; legacy `● kind/name`
        // spinner if the table grid isn't ready.
        let rows_handle = if self.animator.is_active() { Some(self.animator.rows_handle()) } else { None };
        let (effect, template) = match &self.exec_widths {
            Some(w) => (Effect::ExecRowLive { panel: self.panel(), widths: w.clone(), kind: kind.to_string(), name: name.to_string(), started: std::time::Instant::now(), last }, "{msg}"),
            None => (Effect::Spinner { label: format!("{}/{}", kind, name) }, "    {msg}"),
        };
        StartSpinnerPlan { multi: self.multi.clone(), key, effect: Some(effect), template, rows_handle }
    }

    /// Install a per-resource spinner `ProgressBar` (returned by `StartSpinnerPlan::execute()`)
    /// back into the collector. Safe under the collector lock: only HashMap inserts.
    pub fn install_exec_spinner_pb(&mut self, key: String, pb: Option<ProgressBar>, row_id: usize) {
        if let Some(pb) = pb {
            self.execution_spinners.insert(key, (pb, row_id));
        }
    }

    /// Record operation result. On success, settles the spinner row in-place (bright ✓ →
    /// normal green) and leaves the PB in the map until the stage-close drain. On failure,
    /// removes and returns the PB for immediate clearing. Bumps counters and buffers a typed
    /// ExecRow. Returns a `ClearSpinnerPlan` the caller must `execute()` outside the lock.
    pub fn record_operation(&mut self, kind: &str, name: &str, success: bool, duration: std::time::Duration, id: Option<String>) -> ClearSpinnerPlan {
        let duration_ms = duration.as_millis() as u64;
        let key = format!("{}.{}", kind, name);
        let decision = self.decision_map.get(&key).cloned().unwrap_or_else(|| "Create".to_string());

        if success {
            if decision_executes(&decision) {
                // Bump the shared atomic counter (safe: atomic, no indicatif).
                self.exec_done.fetch_add(1, Ordering::Relaxed);

                let diffs = if decision == "Update" { self.decision_diffs.get(&key).cloned().unwrap_or_default() } else { Vec::new() };
                let marker = decision_to_exec_marker(&decision);
                let changed_fields: Vec<String> = diffs.iter().map(|d| d.path.clone()).collect();

                // Tally the completion for the footer here — `exec_rows` gets drained at
                // stage close, before the footer builds (see the field docs). Recreated
                // folds into none of the three buckets (matching the pre-existing footer).
                match marker {
                    ExecMarker::Created => self.completed_created += 1,
                    ExecMarker::Updated => self.completed_updated += 1,
                    ExecMarker::Deleted => self.completed_deleted += 1,
                    ExecMarker::Recreated | ExecMarker::Failed => {}
                }

                // Settle the live row into its completed ▌ Execution row, in place. The PB stays
                // in the map — drain_execution_cleanup clears it at stage close. Falls back to the
                // legacy bright-✓ Settle if the table grid isn't available.
                if let Some(&(_, row_id)) = self.execution_spinners.get(&key) {
                    let last = self.exec_last_key.as_deref() == Some(key.as_str());
                    let effect = match &self.exec_widths {
                        Some(w) => Effect::ExecRowDone { panel: self.panel(), widths: w.clone(), marker, kind: kind.to_string(), name: name.to_string(), id: id.clone(), duration_ms, changed_fields: changed_fields.clone(), last },
                        None => Effect::Settle { label: format!("{}/{}", kind, name), done_at: std::time::Instant::now() },
                    };
                    self.animator.set_effect(row_id, effect);
                }

                // Buffer typed ExecRow for the final static render.
                self.exec_rows.push(ExecRow { marker, kind: kind.to_string(), name: name.to_string(), changed_fields, duration_ms, last: false, id });
            }

            tracing::info!(
                target: "wxctl::substage::execution",
                operation_id = %self.operation_id,
                kind = %kind,
                name = %name,
                duration_ms = duration.as_millis(),
                "operation succeeded"
            );
            // Success: spinner stays until drain; no PB to clear immediately.
            ClearSpinnerPlan { pb: None }
        } else {
            self.exec_done.fetch_add(1, Ordering::Relaxed);
            self.exec_rows.push(ExecRow { marker: ExecMarker::Failed, kind: kind.to_string(), name: name.to_string(), changed_fields: vec![], duration_ms, last: false, id: None });

            tracing::error!(
                target: "wxctl::substage::execution",
                operation_id = %self.operation_id,
                kind = %kind,
                name = %name,
                duration_ms = duration.as_millis(),
                "operation failed"
            );
            // Failure: detach and clear immediately — the Errors section carries the failure.
            let pb_to_clear = self.execution_spinners.remove(&key).map(|(pb, _)| pb);
            ClearSpinnerPlan { pb: pb_to_clear }
        }
    }

    // ── Test progress ──

    /// Start a per-test spinner — returns a `StartSpinnerPlan` to execute outside lock.
    /// After `execute()`, pass the result to `install_exec_spinner_pb` under lock.
    pub fn log_test_start(&mut self, test_name: &str) -> StartSpinnerPlan {
        // Start the Animator on the first test (idempotent).
        self.animator.start();
        let key = format!("test.{}", test_name);
        let label = format!("{} {}", glyph(self.glyphs, "hourglass"), test_name);
        let rows_handle = if self.animator.is_active() { Some(self.animator.rows_handle()) } else { None };
        StartSpinnerPlan { multi: self.multi.clone(), key, effect: Some(Effect::Spinner { label }), template: "    {msg}", rows_handle }
    }

    /// Record test completion. On pass, settles the spinner row (bright ✓ → normal green)
    /// and leaves the PB in the map until the last-test drain. On fail, detaches and clears
    /// immediately. Returns a `ClearSpinnerPlan` to execute outside the lock.
    pub fn record_test_complete(&mut self, test_name: &str, passed: bool, completed: usize, total: usize) -> ClearSpinnerPlan {
        let key = format!("test.{}", test_name);

        // Update stage spinner with progress count (set_message is safe: indicatif
        // pb.set_message only writes to the pb's internal state, not via MultiProgress).
        if let Some(ref stage_pb) = self.stage_spinner {
            let status = if passed { self.theme.paint(Color::Green, glyph(self.glyphs, "check")) } else { self.theme.paint(Color::Red, glyph(self.glyphs, "cross")) };
            stage_pb.set_message(format!("{} [{}/{}] {} {}", self.theme.paint(Color::Blue, "Testing"), completed, total, status, test_name,));
        }

        let pb_to_clear = if passed {
            // Settle the row in-place; PB stays until the last-test drain.
            if let Some(&(_, row_id)) = self.execution_spinners.get(&key) {
                self.animator.set_effect(row_id, Effect::Settle { label: test_name.to_string(), done_at: std::time::Instant::now() });
            }
            None
        } else {
            // Fail: remove and clear immediately.
            self.execution_spinners.remove(&key).map(|(pb, _)| pb)
        };

        // Stop Animator after last test; drain any leftover (settled) spinners.
        if completed == total {
            self.animator.stop();
            for (_, (pb, _)) in self.execution_spinners.drain() {
                pb.finish_and_clear();
            }
        }

        ClearSpinnerPlan { pb: pb_to_clear }
    }

    /// Build the typed `▌ Changes` section from buffered decisions (NoOp dropped).
    fn build_changes_section(&self) -> ChangesSection {
        let mut rows: Vec<ChangeRow> = self
            .buffered_decisions
            .iter()
            .filter(|d| d.decision != "NoOp")
            .map(|d| {
                let (marker, action) = decision_to_marker_action(&d.decision);
                ChangeRow { marker, kind: d.resource_type.clone(), name: d.resource_name.clone(), action: action.to_string(), changed_fields: d.field_diffs.iter().map(|f| f.path.clone()).collect() }
            })
            .collect();
        rows.sort_by_key(|r| match r.marker {
            ChangeMarker::Add => 0,
            ChangeMarker::Change => 1,
            ChangeMarker::Recreate => 2,
            ChangeMarker::Destroy => 3,
            ChangeMarker::Retain => 4,
            ChangeMarker::Skip => 5,
            ChangeMarker::Unchecked => 6,
            ChangeMarker::Undetermined => 7,
        });
        ChangesSection { rows }
    }

    /// Reconciliation began: store the total, reset the done counter, and — when
    /// the Animator is live (TTY) — swap the reconciliation stage line's effect
    /// from `Ellipsis` to a determinate `CounterBar` carrying the current-resource
    /// label. No-op render in Plain mode (Animator inert); the counters still set
    /// so the settled row can stamp the count.
    pub fn on_reconcile_start(&mut self, total: usize) {
        self.reconciliation_resource_count = total;
        self.recon_done.store(0, Ordering::Relaxed);
        if let Ok(mut g) = self.recon_label.lock() {
            g.clear();
        }
        if let Some(row_id) = self.stage_spinner_row {
            self.animator.set_effect(row_id, Effect::CounterBar { done: self.recon_done.clone(), total, noun: "reconciled".into(), cells: 16, started: std::time::Instant::now(), label: Some(self.recon_label.clone()) });
        }
    }

    /// About to discover this resource: update the current-resource label shown on
    /// the live line (`<kind> <name>`). The CounterBar effect reads `recon_label`
    /// via the Animator on its next tick.
    pub fn reconcile_resource_start(&mut self, kind: &str, name: &str) {
        if let Ok(mut g) = self.recon_label.lock() {
            *g = format!("{} {}", kind, name);
        }
    }

    /// One resource finished discovery/enrichment (success or error): advance the
    /// shared done counter. Display-only; never affects the reconcile outcome.
    pub fn reconcile_resource_complete(&mut self) {
        self.recon_done.fetch_add(1, Ordering::Relaxed);
    }

    /// Record skipped operation
    pub fn record_skipped(&mut self, kind: &str, name: &str, reason: &str) {
        tracing::warn!(
            target: "wxctl::substage::execution",
            operation_id = %self.operation_id,
            kind = %kind,
            name = %name,
            reason = %reason,
            "operation skipped"
        );
    }
}

/// Map a decision string to its Changes marker + lowercase action label.
fn decision_to_marker_action(decision: &str) -> (ChangeMarker, &'static str) {
    match decision {
        "Create" => (ChangeMarker::Add, "create"),
        "CreateUnchecked" => (ChangeMarker::Unchecked, "create (unchecked)"),
        "Undetermined" => (ChangeMarker::Undetermined, "undetermined"),
        "Update" => (ChangeMarker::Change, "update"),
        "Delete" => (ChangeMarker::Destroy, "delete"),
        "Recreate" => (ChangeMarker::Recreate, "recreate"),
        "Retain" => (ChangeMarker::Retain, "retain"),
        "SkipAbsent" => (ChangeMarker::Skip, "skip (absent)"),
        "SkipDeferred" => (ChangeMarker::Skip, "skip (deferred)"),
        _ => (ChangeMarker::Skip, "no-op"),
    }
}

/// Map a decision string to its past-tense `ExecMarker`.
fn decision_to_exec_marker(decision: &str) -> ExecMarker {
    match decision {
        "Update" => ExecMarker::Updated,
        "Delete" => ExecMarker::Deleted,
        "Recreate" => ExecMarker::Recreated,
        _ => ExecMarker::Created,
    }
}

/// Map a buffered `ErrorEvent` to a typed `ErrorBlock` for the panel Errors section.
pub(crate) fn error_event_to_block(e: &ErrorEvent) -> ErrorBlock {
    ErrorBlock {
        stage: e.stage.clone(),
        code: e.error_code.clone(),
        kind: e.resource_type.clone(),
        name: e.resource_name.clone(),
        field_path: e.field_path.clone(),
        message: e.message.clone(),
        fix: crate::output::remediation::fix_for(&e.error_code, e.field_path.as_deref()).unwrap_or_else(|| e.fix.clone()),
    }
}

/// Return the display subset of `errors` for the final `▌ Errors` section.
///
/// If any event in the slice is resource-scoped (both `resource_type` and
/// `resource_name` are `Some`), drop all resource-less errors — the
/// resource-scoped wrapper already embeds the root cause and names the
/// resource, making the bare HTTP-layer echo redundant in the final section.
/// When no resource-scoped error exists (a genuine stage-level failure), keep
/// all events so the section is never empty.
pub fn errors_display_subset(errors: &[ErrorEvent]) -> Vec<&ErrorEvent> {
    let has_resource_scoped = errors.iter().any(|e| e.resource_type.is_some() && e.resource_name.is_some());
    if has_resource_scoped { errors.iter().filter(|e| e.resource_type.is_some() && e.resource_name.is_some()).collect() } else { errors.iter().collect() }
}

/// Snapshot of rendering inputs for a stage transition. Carries cloned
/// `MultiProgress` (Arc-wrapped, cheap) plus everything needed to drive the
/// indicatif calls outside the collector lock — fixing the deadlock where
/// `on_new_span` held the parking_lot mutex during a `MultiProgress::add`
/// call that internally contended on indicatif's own state lock.
pub struct StageRenderPlan {
    multi: Option<MultiProgress>,
    /// Lines to emit (after clearing the previous spinner) for completed/failed
    /// transitions. Empty for `started`.
    emit_lines: Vec<String>,
    /// ProgressBar to clear (`finish_and_clear`) before drawing — the previous
    /// stage's spinner that was detached under lock.
    drain_spinner: Option<ProgressBar>,
    /// Inputs for building a new stage spinner. `Some` for `started`, `None`
    /// otherwise — keeps the spinner-only theme clone and frame allocation
    /// off the completion path.
    spinner: Option<StageSpinnerArgs>,
}

struct StageSpinnerArgs {
    theme: Theme,
    stage_num: usize,
    stage_count: usize,
    stage_name: String,
}

impl StageRenderPlan {
    /// Execute the indicatif work this plan describes. Runs WITHOUT the
    /// collector lock so a hanging `multi.add` can't block other threads
    /// from accessing the collector. Returns the new stage `ProgressBar`
    /// (if any) which the caller installs back into the collector.
    pub fn execute(self) -> Option<ProgressBar> {
        if let Some(pb) = self.drain_spinner {
            pb.finish_and_clear();
        }
        for line in &self.emit_lines {
            emit_line(&self.multi, line);
        }
        let args = self.spinner?;
        let multi = self.multi?;
        Some(build_stage_spinner_pb(&multi, &args.theme, args.stage_num, args.stage_count, &args.stage_name))
    }
}

/// Build a stage `ProgressBar` (the slow indicatif path: `multi.add`).
/// Standalone so callers can run it outside the collector lock.
/// The template has no `{spinner}` token — the Animator's Ellipsis/CounterBar
/// message is the only live motion on the stage line.
fn build_stage_spinner_pb(multi: &MultiProgress, theme: &Theme, stage_num: usize, stage_count: usize, stage_name: &str) -> ProgressBar {
    let pb = multi.add(ProgressBar::new_spinner());
    // 4-space indent in the template aligns the live `[N/N] …` stage line's left edge
    // with the panel body (Pipeline rows + per-resource rows, both at column 4) instead
    // of jutting out at column 0.
    pb.set_style(ProgressStyle::with_template("    {prefix}{msg}\n\n").unwrap());
    pb.set_prefix(theme.paint(Color::Blue, &format!("[{}/{}] ", stage_num, stage_count)));
    pb.set_message(format_stage_spinner_msg(theme, stage_name));
    pb
}

/// Emit one line via `MultiProgress::println` when present, falling back to
/// `eprintln!` (the panel is diagnostics → stderr). Shared by all
/// `*RenderPlan::execute()` paths.
fn emit_line(multi: &Option<MultiProgress>, line: &str) {
    match multi {
        Some(m) => {
            let _ = m.println(line);
        }
        None => eprintln!("{}", line),
    }
}

/// Snapshot of rendering inputs for a substage emit (`multi.println`). Lets
/// the caller emit outside the collector lock.
pub struct SubstageRenderPlan {
    multi: Option<MultiProgress>,
    line: String,
}

impl SubstageRenderPlan {
    pub fn execute(self) {
        // Empty line = quiet-mode sentinel from `add_substage_state`.
        if self.line.is_empty() {
            return;
        }
        emit_line(&self.multi, &self.line);
    }
}

/// Plan for starting a per-resource spinner. `multi.add` (the indicatif call)
/// and the `AnimatorRow` push both happen in `execute()`, outside the collector
/// lock. The `rows_handle` is the Animator's shared rows `Arc<Mutex<_>>` —
/// pushing to it takes only the Animator's internal lock, not the collector's.
pub struct StartSpinnerPlan {
    multi: Option<MultiProgress>,
    /// "kind.name" key for `install_exec_spinner_pb`. Empty string = no-op (NoOp decision).
    key: String,
    /// The effect to bind to the new row's `ProgressBar`. `None` = no-op.
    effect: Option<Effect>,
    /// indicatif style template for the row's bar: `"{msg}"` for live ▌ Execution rows
    /// (which render their own indent + connector) or `"    {msg}"` for legacy/test rows.
    template: &'static str,
    rows_handle: Option<Arc<Mutex<Vec<AnimatorRow>>>>,
}

impl StartSpinnerPlan {
    /// A no-op plan (NoOp/Retain/Skip decisions, or non-TTY): `execute()` adds nothing.
    fn noop() -> Self {
        StartSpinnerPlan { multi: None, key: String::new(), effect: None, template: "    {msg}", rows_handle: None }
    }

    /// Execute the indicatif work (multi.add + AnimatorRow push) outside the
    /// collector lock. Returns `(key, Some(pb), row_id)` to pass to `install_exec_spinner_pb`.
    /// `row_id` is the Animator row index for later `set_effect` calls; 0 when no row was pushed.
    pub fn execute(self) -> (String, Option<ProgressBar>, usize) {
        if self.key.is_empty() {
            return (self.key, None, 0);
        }
        let Some(multi) = self.multi else { return (self.key, None, 0) };
        let Some(effect) = self.effect else { return (self.key, None, 0) };
        let pb = multi.add(ProgressBar::new_spinner());
        pb.set_style(ProgressStyle::with_template(self.template).unwrap());
        let row_id = if let Some(rows) = self.rows_handle {
            let mut guard = rows.lock().unwrap();
            let id = guard.len();
            guard.push(AnimatorRow { pb: pb.clone(), effect, last_msg: None, cached: None });
            id
        } else {
            0
        };
        (self.key, Some(pb), row_id)
    }
}

/// One prefilled skeleton row to create.
struct PrefillRow {
    key: String,
    kind: String,
    name: String,
    last: bool,
}

/// Plan for prefilling the live `▌ Execution` table with one `pending` bar per resource.
/// The `multi.add` calls (indicatif lock) run in `execute()` outside the collector lock;
/// each bar's `AnimatorRow` is pushed via the shared `rows_handle` (Animator lock only).
pub struct PrefillRowsPlan {
    multi: Option<MultiProgress>,
    rows_handle: Option<Arc<Mutex<Vec<AnimatorRow>>>>,
    panel: Panel,
    widths: ExecWidths,
    rows: Vec<PrefillRow>,
}

impl PrefillRowsPlan {
    /// Create the skeleton bars outside the collector lock. Mirrors `StartSpinnerPlan`:
    /// `multi.add` releases the indicatif lock before the Animator-rows lock is taken (push),
    /// so it never nests indicatif under the rows lock (the ticker's order). Returns
    /// `(key, pb, row_id)` per row for `install_prefilled_rows`.
    pub fn execute(self) -> Vec<(String, ProgressBar, usize)> {
        let (Some(multi), Some(rows_handle)) = (self.multi, self.rows_handle) else {
            return Vec::new();
        };
        let mut installed = Vec::with_capacity(self.rows.len());
        for r in self.rows {
            let pb = multi.add(ProgressBar::new_spinner());
            pb.set_style(ProgressStyle::with_template("{msg}").unwrap());
            let row_id = {
                let mut guard = rows_handle.lock().unwrap();
                let id = guard.len();
                guard.push(AnimatorRow { pb: pb.clone(), effect: Effect::ExecRowPending { panel: self.panel.clone(), widths: self.widths.clone(), kind: r.kind, name: r.name, last: r.last }, last_msg: None, cached: None });
                id
            };
            installed.push((r.key, pb, row_id));
        }
        installed
    }
}

/// Plan for clearing a single per-resource `ProgressBar` outside the collector lock.
pub struct ClearSpinnerPlan {
    pb: Option<ProgressBar>,
}

impl ClearSpinnerPlan {
    pub fn execute(self) {
        if let Some(pb) = self.pb {
            pb.finish_and_clear();
        }
    }
}

/// Cleanup plan for the execution stage's `completed` transition. Carries
/// the lingering per-resource progress bars, buffered typed rows, and
/// indicatif handles needed to render them — all extracted under collector
/// lock, then `execute()` runs the indicatif teardown without it.
pub struct ExecutionCleanupPlan {
    pbs_to_clear: Vec<ProgressBar>,
    rows: Vec<ExecRow>,
    multi: Option<MultiProgress>,
    panel: Panel,
}

impl ExecutionCleanupPlan {
    pub fn execute(self) {
        for pb in self.pbs_to_clear {
            pb.finish_and_clear();
        }
        if self.rows.is_empty() {
            return;
        }
        // No leading blank here: the blank line emitted at execution-stage start (above the
        // live ▌ Execution header) persists in scrollback and already separates this final
        // static table from the ▌ Pipeline section above.
        let section = ExecutionSection { rows: self.rows };
        for line in render_execution(&self.panel, &section) {
            emit_line(&self.multi, &line);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::output::panel::theme::{ColorMode, Theme};

    /// Build a minimal `ErrorEvent` for tests.
    fn make_error(code: &str, resource_type: Option<&str>, resource_name: Option<&str>, message: &str) -> ErrorEvent {
        ErrorEvent {
            operation_id: "op-test".to_string(),
            stage: "execution".to_string(),
            error_code: code.to_string(),
            resource_type: resource_type.map(|s| s.to_string()),
            resource_name: resource_name.map(|s| s.to_string()),
            field_path: None,
            message: message.to_string(),
            fix: String::new(),
            cause: None,
            caused_by: None,
            expected: None,
            actual: None,
            context: None,
        }
    }

    /// Regression: a cascade of (H001, E001, E001-dup, E000, E000) for ONE logical
    /// failure must produce exactly:
    ///   - `collector.errors.len() == 2`  (H001 + first E001; dup-E001 and both E000 discarded)
    ///   - `collector.summary.failed == 2` (E000s never counted; dup-E001 not double-counted)
    ///   - Part-B display subset narrows to 1 block (the resource-scoped E001)
    #[test]
    fn add_error_dedup_cascade() {
        let theme = Theme::new(ColorMode::Plain);
        let mut collector = OutputCollector::new("op-test".to_string(), theme);

        // H001: bare HTTP-layer root (no resource scope).
        collector.add_error(make_error("WXCTL-H001", None, None, "HTTP POST /v2/connections returned 409 Conflict"));

        // E001 first wrapper: resource-scoped.
        collector.add_error(make_error("WXCTL-E001", Some("orchestrate_connection"), Some("dup_conn_a"), "Create failed: HTTP POST /v2/connections returned 409 Conflict"));

        // E001 duplicate wrapper: same resource — must be deduped away.
        collector.add_error(make_error("WXCTL-E001", Some("orchestrate_connection"), Some("dup_conn_a"), "Create failed: resource already exists (409)"));

        // E000 rollup ×2: must be discarded entirely (not counted, not pushed).
        collector.add_error(make_error("WXCTL-E000", None, None, "Execution failed with 1 errors"));
        collector.add_error(make_error("WXCTL-E000", None, None, "Execution failed with 1 errors"));

        // Part A assertions.
        assert_eq!(collector.errors.len(), 2, "expected H001 + first E001 only; dup-E001 and both E000 must be discarded");
        assert_eq!(collector.summary.failed, 2, "E000 must not increment failed; dup-E001 must not double-count");

        // Part B: display subset must contain only the resource-scoped E001.
        let subset = errors_display_subset(&collector.errors);
        assert_eq!(subset.len(), 1, "resource-less H001 must be dropped when a resource-scoped error exists");
        assert_eq!(subset[0].resource_type.as_deref(), Some("orchestrate_connection"));
        assert_eq!(subset[0].resource_name.as_deref(), Some("dup_conn_a"));
    }

    /// Build a `StageEvent` for the lifecycle-ordering tests.
    fn stage_event(stage: &str, status: &str, duration_ms: Option<u64>) -> StageEvent {
        StageEvent { operation_id: "op-test".to_string(), stage: stage.to_string(), status: status.to_string(), resource_count: 0, duration_ms }
    }

    /// Build a `Create` `DecisionEvent` for the lifecycle-ordering tests.
    fn create_decision(resource_type: &str, resource_name: &str) -> DecisionEvent {
        DecisionEvent { operation_id: "op-test".to_string(), resource_type: resource_type.to_string(), resource_name: resource_name.to_string(), decision: "Create".to_string(), reason: "absent".to_string(), field_diffs: vec![] }
    }

    /// Build a `Delete` `DecisionEvent` for the destroy-ordering test.
    fn delete_decision(resource_type: &str, resource_name: &str) -> DecisionEvent {
        DecisionEvent { operation_id: "op-test".to_string(), resource_type: resource_type.to_string(), resource_name: resource_name.to_string(), decision: "Delete".to_string(), reason: "present".to_string(), field_diffs: vec![] }
    }

    /// Regression: `plan` must render the `▌ Changes` list, not just the footer count.
    ///
    /// The engine emits decisions *inside the reconciliation stage*, whose span closes
    /// before the planning span opens. A prior bug cleared `buffered_decisions` at *every*
    /// non-execution stage close, so reconciliation's close wiped the decisions before
    /// planning's close could render them — `plan` silently dropped its whole preview and
    /// showed only `+N to add`. This drives the real event order and asserts the Changes
    /// rows survive to the planning-close render.
    #[test]
    fn plan_changes_survive_reconciliation_stage_close() {
        let mut c = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        c.set_has_execution_stage(false); // plan path

        // validation
        c.add_stage_state(stage_event("validation", "started", None));
        c.add_stage_state(stage_event("validation", "completed", Some(10)));
        // reconciliation — decisions are emitted here, before the stage closes
        c.add_stage_state(stage_event("reconciliation", "started", None));
        c.add_decision(create_decision("orchestrate_connection", "httpbin_bearer"));
        c.add_decision(create_decision("tool", "httpbin_tools_echoGet"));
        c.add_decision(create_decision("agent", "httpbin_agent"));
        c.add_stage_state(stage_event("reconciliation", "completed", Some(2500)));
        // planning — this close must render the Changes section from the buffered decisions
        c.add_stage_state(stage_event("planning", "started", None));
        let planning_close = c.add_stage_state(stage_event("planning", "completed", Some(5)));

        let rendered = planning_close.emit_lines.join("\n");
        assert!(rendered.contains("Changes"), "planning-close renders the Changes section header: {rendered:?}");
        assert!(rendered.contains("httpbin_bearer"), "Changes lists the connection resource: {rendered:?}");
        assert!(rendered.contains("httpbin_tools_echoGet"), "Changes lists the tool resource: {rendered:?}");
        assert!(rendered.contains("httpbin_agent"), "Changes lists the agent resource: {rendered:?}");
        assert!(rendered.contains("create"), "rows carry the create action label: {rendered:?}");
    }

    /// Apply/destroy must NOT print a Changes section (their `▌ Execution` section shows
    /// what actually happened) — but the buffered-decision clear on the execution path must
    /// still fire so the buffer doesn't leak across stages.
    #[test]
    fn apply_path_does_not_render_changes_section() {
        let mut c = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        c.set_has_execution_stage(true); // apply/destroy path

        c.add_stage_state(stage_event("validation", "started", None));
        c.add_stage_state(stage_event("validation", "completed", Some(10)));
        c.add_stage_state(stage_event("reconciliation", "started", None));
        c.add_decision(create_decision("agent", "httpbin_agent"));
        let recon_close = c.add_stage_state(stage_event("reconciliation", "completed", Some(2500)));
        c.add_stage_state(stage_event("planning", "started", None));
        let planning_close = c.add_stage_state(stage_event("planning", "completed", Some(5)));

        let rendered = format!("{}\n{}", recon_close.emit_lines.join("\n"), planning_close.emit_lines.join("\n"));
        assert!(!rendered.contains("Changes"), "apply/destroy must not render a Changes section: {rendered:?}");
        assert!(c.buffered_decisions.is_empty(), "execution path clears the decision buffer after reconciliation close");
    }

    /// Regression (live-caught on a real destroy): the exec footer's counts must survive
    /// `drain_execution_cleanup` — the stage-close drain `mem::take`s `exec_rows` to render
    /// the static Execution table BEFORE `print_summary` builds the footer, so a footer
    /// tallying `exec_rows` reported `ok destroy: no changes` after five real deletes.
    #[test]
    fn exec_footer_counts_survive_stage_close_drain() {
        let mut c = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        c.set_has_execution_stage(true);
        c.add_decision(delete_decision("space", "alpha"));
        c.add_decision(delete_decision("data_asset", "bravo"));
        c.add_decision(create_decision("agent", "charlie"));
        let _ = c.record_operation("space", "alpha", true, std::time::Duration::from_millis(80), None);
        let _ = c.record_operation("data_asset", "bravo", true, std::time::Duration::from_millis(60), None);
        let _ = c.record_operation("agent", "charlie", true, std::time::Duration::from_millis(40), None);
        let _ = c.drain_execution_cleanup(); // stage close: renders the static table, takes exec_rows
        let f = c.build_exec_footer("destroy", false);
        assert!(matches!(f.outcome, ExecOutcome::Ok), "no errors → Ok outcome");
        assert_eq!((f.created, f.updated, f.deleted), (1, 0, 2), "completed counts survive the drain");
    }

    /// The final settled ▌ Execution table follows execution-START order (matching the live
    /// stream), not the COMPLETION order rows are buffered in — so it never reshuffles when it
    /// settles. Resources start alpha→bravo→charlie but finish in a different order; the table
    /// must still read alpha, bravo, charlie.
    #[test]
    fn final_exec_table_orders_by_start_not_completion() {
        let mut c = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        c.set_has_execution_stage(true);
        for n in ["alpha", "bravo", "charlie"] {
            c.add_decision(create_decision("tool", n));
        }
        // Execution-stage start records the plan order (alpha, bravo, charlie) + last key.
        c.add_stage_state(stage_event("execution", "started", None));
        assert_eq!(c.exec_start_order, vec!["tool.alpha", "tool.bravo", "tool.charlie"], "plan order captured at stage start");
        assert_eq!(c.exec_last_key.as_deref(), Some("tool.charlie"), "last key captured");
        for n in ["alpha", "bravo", "charlie"] {
            let _ = c.log_start("tool", n);
        }
        // Finish in a different order (e.g. by duration): bravo, charlie, alpha.
        c.record_operation("tool", "bravo", true, std::time::Duration::from_millis(1000), None);
        c.record_operation("tool", "charlie", true, std::time::Duration::from_millis(3000), None);
        c.record_operation("tool", "alpha", true, std::time::Duration::from_millis(5000), None);

        let plan = c.drain_execution_cleanup();
        let names: Vec<&str> = plan.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["alpha", "bravo", "charlie"], "settled table follows plan order, not completion order");
        assert!(plan.rows.last().unwrap().last, "last row is flagged for the └─ connector");
    }

    /// The prefill plan projects one row per executing resource, in plan order, with the last
    /// row flagged. (Plain mode has an inert Animator, so the bars themselves aren't created
    /// here — this asserts the plan's row set/order, which is what drives the skeleton.)
    #[test]
    fn prefill_plan_lists_every_executing_resource_in_plan_order() {
        let mut c = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        c.set_has_execution_stage(true);
        c.add_decision(create_decision("tool", "alpha"));
        c.add_decision(create_decision("agent", "bravo"));
        // A NoOp decision must be excluded from the skeleton (it doesn't execute).
        c.add_decision(DecisionEvent { operation_id: "op-test".to_string(), resource_type: "tool".to_string(), resource_name: "skipme".to_string(), decision: "NoOp".to_string(), reason: "match".to_string(), field_diffs: vec![] });
        c.add_stage_state(stage_event("execution", "started", None));

        let plan = c.prefill_exec_rows_plan();
        let keys: Vec<&str> = plan.rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(keys, vec!["tool.alpha", "agent.bravo"], "skeleton lists executing resources in plan order, NoOp excluded");
        assert!(!plan.rows[0].last && plan.rows[1].last, "only the final row carries the └─ flag");
    }

    /// Destroy walks the DAG in reverse (dependents deleted before their dependencies), so the
    /// table reverses the topo order it's reconciled in: the dependent row is laid out first and
    /// the dependency last (└─). Both the skeleton and the settled table must follow this.
    #[test]
    fn destroy_table_reverses_into_dag_walkback_order() {
        let mut c = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        c.set_has_execution_stage(true);
        c.set_command("destroy".to_string(), String::new());
        // Decisions arrive in topo order (dependency first): the tool, then the agent that needs it.
        c.add_decision(delete_decision("tool", "weather_tool"));
        c.add_decision(delete_decision("agent", "weather_agent"));
        c.add_stage_state(stage_event("execution", "started", None));

        // Reverse-topo: the dependent agent is deleted first, the tool last.
        assert_eq!(c.exec_start_order, vec!["agent.weather_agent", "tool.weather_tool"], "destroy lays out in reverse-topo (walk-back) order");
        assert_eq!(c.exec_last_key.as_deref(), Some("tool.weather_tool"), "the dependency is deleted last (└─)");

        let keys: Vec<String> = c.prefill_exec_rows_plan().rows.iter().map(|r| r.key.clone()).collect();
        assert_eq!(keys, vec!["agent.weather_agent", "tool.weather_tool"], "skeleton follows the walk-back order");

        // Settle in some order; the final table still reads in walk-back order.
        let _ = c.log_start("agent", "weather_agent");
        let _ = c.log_start("tool", "weather_tool");
        c.record_operation("tool", "weather_tool", true, std::time::Duration::from_millis(500), None);
        c.record_operation("agent", "weather_agent", true, std::time::Duration::from_millis(500), None);
        let plan = c.drain_execution_cleanup();
        let names: Vec<&str> = plan.rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, vec!["weather_agent", "weather_tool"], "settled destroy table reads in walk-back order");
        assert!(plan.rows.last().unwrap().last, "dependency row flagged └─");
    }

    /// Regression: `--output json` (quiet mode) must not leak the settled ▌ Execution table to
    /// stdout. `drain_execution_cleanup` is called unconditionally on execution-stage close (on
    /// apply/destroy), so in quiet mode it must return an empty row set — the caller's
    /// `ExecutionCleanupPlan::execute` no-ops on empty rows, keeping stdout pure JSON. A
    /// non-quiet control collector proves the row IS produced absent the guard.
    #[test]
    fn quiet_drain_execution_cleanup_emits_no_rows() {
        let mut quiet = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        quiet.set_has_execution_stage(true);
        let _ = quiet.record_operation("tool", "t1", true, std::time::Duration::from_millis(10), None);
        quiet.set_quiet();
        let plan = quiet.drain_execution_cleanup();
        assert!(plan.rows.is_empty(), "quiet mode: no rows for the settled Execution table");

        let mut loud = OutputCollector::new("op-test".to_string(), Theme::new(ColorMode::Plain));
        loud.set_has_execution_stage(true);
        let _ = loud.record_operation("tool", "t1", true, std::time::Duration::from_millis(10), None);
        let plan = loud.drain_execution_cleanup();
        assert!(!plan.rows.is_empty(), "non-quiet mode: the row is present, proving the guard (not row production) suppresses it");
    }
}
