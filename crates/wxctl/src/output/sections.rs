//! Typed view-model for the panel screens (spec "Typed sections"). The collector
//! populates these pure structs from the event stream; `panel_render` turns each
//! into `Vec<String>`. Snapshot tests construct them directly with fixed data so
//! the plan screens are testable offline (no live API calls). Phase 3 covers the
//! plan screens: `Header`, `PipelineSection`, `ChangesSection`, `ErrorsSection`,
//! `Footer`. The live `Execution` section is Phase 4.

/// One pipeline-stage row: `✓ validation   0.4s` / red `✗ validation` on failure.
#[derive(Debug, Clone)]
pub struct PipelineRow {
    /// Display name, lowercase: "validation" | "reconciliation" | "planning".
    pub stage: String,
    /// "completed" | "failed" | "started" (started = in-flight, animated).
    pub status: String,
    /// `None` while in-flight or when timing is unavailable → renders `—`.
    pub duration_ms: Option<u64>,
    /// Optional dim detail rendered between the stage name and the duration.
    /// Only the *completed* reconciliation row populates it (`"N reconciled"`);
    /// every other row — and any failed row — leaves it `None`.
    pub detail: Option<String>,
}

/// The `▌ Pipeline` section.
#[derive(Debug, Clone, Default)]
pub struct PipelineSection {
    pub rows: Vec<PipelineRow>,
}

/// Marker class for a Changes row — drives marker glyph + color (spec AC10: only
/// the marker + action carry SGR; Type/Name are uncolored).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeMarker {
    /// `+` green — confident create.
    Add,
    /// `~` amber — update.
    Change,
    /// `-` red — delete.
    Destroy,
    /// `±` amber — recreate.
    Recreate,
    /// `=` blue — retain.
    Retain,
    /// `?` amber — create-unchecked (identity path templated; carries footnote).
    Unchecked,
    /// `!` red — undetermined (discovery failed; AC17).
    Undetermined,
    /// `·` dim — skipped (absent/deferred).
    Skip,
}

/// One Changes row.
#[derive(Debug, Clone)]
pub struct ChangeRow {
    pub marker: ChangeMarker,
    pub kind: String,
    pub name: String,
    /// Action label: "create" | "update" | "delete" | "recreate" | "retain"
    /// | "create (unchecked)" | "undetermined" | "skip (absent)" | "skip (deferred)".
    pub action: String,
    /// Changed-field names for update rows (rendered as a dim `[~a, ~b]` suffix).
    pub changed_fields: Vec<String>,
}

/// The `▌ Changes` section.
#[derive(Debug, Clone, Default)]
pub struct ChangesSection {
    pub rows: Vec<ChangeRow>,
}

impl ChangesSection {
    /// True when any row needs the unchecked/undetermined footnote legend.
    pub fn has_uncertain(&self) -> bool {
        self.rows.iter().any(|r| matches!(r.marker, ChangeMarker::Unchecked | ChangeMarker::Undetermined))
    }
}

/// One error block in the `▌ Errors` section (single-render full detail).
#[derive(Debug, Clone)]
pub struct ErrorBlock {
    pub stage: String,
    pub code: String,
    pub kind: Option<String>,
    pub name: Option<String>,
    pub field_path: Option<String>,
    pub message: String,
    pub fix: String,
}

/// The `▌ Errors` section.
#[derive(Debug, Clone, Default)]
pub struct ErrorsSection {
    pub blocks: Vec<ErrorBlock>,
}

/// One advisory block in the `▌ Advisories` section (warn-level, non-blocking).
#[derive(Debug, Clone)]
pub struct AdvisoryBlock {
    pub code: String,
    pub resource: String,
    pub message: String,
    pub suggestion: String,
}

/// The `▌ Advisories` section. Warn-level; never affects validity or the exit code.
#[derive(Debug, Clone, Default)]
pub struct AdvisoriesSection {
    pub blocks: Vec<AdvisoryBlock>,
}

/// Plan-screen footer outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Plan succeeded with changes (`✓ plan: +N to add · Xs`).
    PlanOk,
    /// Plan succeeded, no changes (`✓ plan: no changes · Xs`).
    PlanNoChanges,
    /// Plan failed (`✗ plan failed · run <id> — wxctl debug`).
    Failed,
}

/// Plan-screen footer. Counts mirror `OperationSummary`; `undetermined` feeds the
/// AC17 `+N to add, K undetermined` wording.
#[derive(Debug, Clone)]
pub struct Footer {
    pub outcome: Outcome,
    /// `"plan"` (used for the verb in the footer + the `next:` hint).
    pub command: String,
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    pub retained: usize,
    pub skipped: usize,
    pub undetermined: usize,
    pub duration_ms: u64,
    /// Run id, shown on the failure footer (`run <id> — wxctl debug`).
    pub run_id: String,
    /// Config paths, joined for the `next: wxctl apply -f <config>` hint.
    pub config_hint: String,
}

/// Execution-row marker — drives the connector glyph + color of one completed
/// `▌ Execution` row (apply/destroy/test). Mirrors the plan `ChangeMarker` set
/// but in past tense: the resource has been operated on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecMarker {
    /// `+` green — created.
    Created,
    /// `~` amber — updated.
    Updated,
    /// `-` red — deleted.
    Deleted,
    /// `±` amber — recreated.
    Recreated,
    /// `✗` red — the operation failed.
    Failed,
}

/// One completed `▌ Execution` row: a DAG connector (`├─`/`└─`), the resource,
/// the past-tense marker, optional changed fields (updates), and a duration.
#[derive(Debug, Clone)]
pub struct ExecRow {
    pub marker: ExecMarker,
    pub kind: String,
    pub name: String,
    /// Changed-field names for update rows (dim `[~a, ~b]` suffix).
    pub changed_fields: Vec<String>,
    pub duration_ms: u64,
    /// True when this is the last row → renders the `└─` connector, else `├─`.
    pub last: bool,
    /// Backend-assigned resource id from the create response (Terraform-style
    /// `[id=…]` dim suffix). `None` for deletes / responses without an id.
    pub id: Option<String>,
}

/// The `▌ Execution` section (final static render — post-run).
#[derive(Debug, Clone, Default)]
pub struct ExecutionSection {
    pub rows: Vec<ExecRow>,
}

/// One created-resource URL line in the apply summary.
#[derive(Debug, Clone)]
pub struct CreatedUrl {
    pub name: String,
    pub url: String,
}

/// Execution-screen footer outcome (apply / destroy / test).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecOutcome {
    /// All operations succeeded (`✓ apply: +N created · Xs`).
    Ok,
    /// One or more operations failed (`✗ apply failed · run <id> — wxctl debug`).
    Failed,
}

/// Execution-screen footer. Counts mirror `OperationSummary`; `urls` carry
/// created-resource links (apply success); `run_id` feeds the failure hint.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ExecFooter {
    pub outcome: ExecOutcome,
    /// `"apply"` | `"destroy"` | `"test"` — the verb in the footer line.
    pub command: String,
    pub created: usize,
    pub updated: usize,
    pub deleted: usize,
    /// Resources deliberately kept (`on_destroy: retain`). Rendered as `=N retained`
    /// so a retain-only destroy reads as a kept scope, not `no changes`.
    pub retained: usize,
    pub failed: usize,
    pub duration_ms: u64,
    /// Created-resource URLs, listed under the footer on the apply success path.
    pub urls: Vec<CreatedUrl>,
    /// Run id, shown on the failure footer (`run <id> — wxctl debug`).
    pub run_id: String,
}
