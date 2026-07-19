//! Run-record artifact: manifest + flat event log written under `~/.wxctl/runs/<run_id>/`.
//!
//! Pure types + filesystem IO, plus the `RunRecordLayer` tracing Layer and global sink
//! slot (`install_run_sink` / `RunSinkGuard` / `set_full_trace` / `finalize_active_run`).
//! The Layer lives here (rather than in the `wxctl` binary) so sibling crates such as
//! `wxctl-mcp` can install per-tool-call sinks against the same global subscriber.
//! Observability never breaks the command: every write is best-effort and flips
//! `record_incomplete` on failure rather than propagating an error.

use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// One entry in the manifest error index.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestError {
    pub code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// Summary counters mirrored from `log_summary!`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RunCounts {
    pub total: u64,
    pub created: u64,
    pub updated: u64,
    pub deleted: u64,
    pub noop: u64,
    pub retained: u64,
    pub failed: u64,
    pub skipped: u64,
}

/// `manifest.json` schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub command: String,
    pub args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<String>,
    pub config_paths: Vec<String>,
    pub started: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finished: Option<String>,
    /// `success` | `failed` | `aborted`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<String>,
    pub counts: RunCounts,
    pub errors: Vec<ManifestError>,
    pub full_trace: bool,
    pub record_incomplete: bool,
}

/// Generate a `run_id`: `YYYYMMDD-HHMMSS-<command>-<6hex>`.
/// Time-formatted without an extra date crate: derive UTC Y/M/D/H/M/S from the
/// unix epoch via a civil-from-days conversion (Howard Hinnant's algorithm).
pub fn generate_run_id(command: &str) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = now.as_secs();
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    // 6 hex chars of entropy from sub-second nanos + pid.
    let entropy = (now.subsec_nanos() as u64).wrapping_mul(0x9E37_79B9).wrapping_add(std::process::id() as u64);
    let hex = format!("{:06x}", entropy & 0xFF_FFFF);
    let cmd = command.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>();
    format!("{y:04}{mo:02}{d:02}-{h:02}{mi:02}{s:02}-{cmd}-{hex}")
}

/// (year, month, day, hour, min, sec) UTC from unix seconds.
fn civil_from_epoch(secs: u64) -> (i64, u32, u32, u32, u32, u32) {
    let days = (secs / 86_400) as i64;
    let rem = (secs % 86_400) as u32;
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // days since 1970-01-01 -> civil date (Hinnant).
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let mo = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let y = if mo <= 2 { y + 1 } else { y };
    (y, mo, d, h, mi, s)
}

/// RFC3339-ish UTC timestamp `YYYY-MM-DDTHH:MM:SSZ` for manifest started/finished.
pub fn utc_now_string() -> String {
    let secs = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs();
    let (y, mo, d, h, mi, s) = civil_from_epoch(secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z")
}

/// Root of the runs tree: `WXCTL_RUNS_DIR`, else `~/.wxctl/runs`.
pub fn runs_root() -> PathBuf {
    if let Ok(dir) = std::env::var("WXCTL_RUNS_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".wxctl").join("runs")
}

/// Keep at most N run directories (oldest pruned). `WXCTL_RUNS_KEEP`, default 50.
pub fn retention_keep() -> usize {
    std::env::var("WXCTL_RUNS_KEEP").ok().and_then(|v| v.parse().ok()).unwrap_or(50)
}

/// Grace window: a run dir without a finalized `manifest.json` is treated as
/// in-progress — and never pruned — until it has gone untouched for this long.
/// This is what stops one process's retention sweep from deleting another
/// process's still-writing run dir out from under it (which loses the record and
/// emits a spurious "manifest write failed" warning). Generous on purpose: the
/// cost of keeping a genuinely-abandoned (e.g. SIGKILL'd) run around a bit
/// longer is one stale dir; the cost of deleting a live one is real.
const IN_PROGRESS_GRACE_SECS: u64 = 3600;

/// True if `dir` looks like a still-running command's run dir: no finalized
/// `manifest.json` yet, and the dir or its `events.jsonl` was modified within
/// `IN_PROGRESS_GRACE_SECS`. Such dirs are excluded from pruning.
fn run_in_progress(dir: &Path) -> bool {
    // A finalized run always has manifest.json (written at `finalize`); it is
    // safe to prune regardless of age.
    if dir.join("manifest.json").is_file() {
        return false;
    }
    let now = SystemTime::now();
    let fresh = |p: &Path| fs::metadata(p).and_then(|m| m.modified()).ok().and_then(|t| now.duration_since(t).ok()).is_some_and(|age| age.as_secs() < IN_PROGRESS_GRACE_SECS);
    fresh(dir) || fresh(&dir.join("events.jsonl"))
}

/// Prune oldest run dirs so at most `retention_keep()` remain. Best-effort; errors ignored.
/// Run dirs sort lexicographically by their timestamp prefix, so name-sort == age-sort.
pub fn prune_runs(root: &Path) {
    let Ok(entries) = fs::read_dir(root) else { return };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    let keep = retention_keep();
    if dirs.len() <= keep {
        return;
    }
    dirs.sort();
    // Delete oldest-first down to `keep`, but never an in-progress run: a
    // concurrent process may still be writing it. Skipping in-progress dirs may
    // leave more than `keep` behind temporarily; they get pruned once finalized.
    let mut to_remove = dirs.len() - keep;
    for dir in dirs {
        if to_remove == 0 {
            break;
        }
        if run_in_progress(&dir) {
            continue;
        }
        let _ = fs::remove_dir_all(&dir);
        to_remove -= 1;
    }
}

/// Active per-command artifact writer + manifest accumulator.
///
/// `events.jsonl` is appended line-by-line as events arrive through a
/// `BufWriter` (one syscall per buffer, not per event) and flushed on
/// `finalize` — the durability point, reached on success, failure, panic, and
/// ctrl-c via `finalize_active_run`. The manifest is held in memory and
/// written on `finalize`. All IO is best-effort: a failure sets
/// `record_incomplete` and emits one stderr warning, never an error.
pub struct RunSink {
    dir: PathBuf,
    events_file: Mutex<Option<BufWriter<File>>>,
    manifest: Mutex<RunManifest>,
    warned: Mutex<bool>,
}

impl RunSink {
    /// Create the run dir, open `events.jsonl`, seed the manifest. Returns `None`
    /// only if even directory creation fails (caller continues without a record).
    pub fn new(manifest: RunManifest) -> Option<Self> {
        let root = runs_root();
        prune_runs(&root);
        let dir = root.join(&manifest.run_id);
        let dir = if fs::create_dir_all(&dir).is_ok() {
            dir
        } else {
            let alt = std::env::temp_dir().join("wxctl-runs").join(&manifest.run_id);
            if fs::create_dir_all(&alt).is_err() {
                eprintln!("warning: could not create run-record dir {}: falling back to null sink", dir.display());
                return None;
            }
            alt
        };
        let events_file = OpenOptions::new().create(true).write(true).truncate(true).open(dir.join("events.jsonl")).ok().map(BufWriter::new);
        Some(Self { dir, events_file: Mutex::new(events_file), manifest: Mutex::new(manifest), warned: Mutex::new(false) })
    }

    /// A no-op sink for the case where no artifact dir could be created. All
    /// writes warn-once and no-op; honors "observability never breaks the command".
    pub fn null() -> Self {
        let manifest =
            RunManifest { run_id: "null".into(), command: String::new(), args: vec![], profile: None, deployment: None, config_paths: vec![], started: utc_now_string(), finished: None, outcome: None, counts: RunCounts::default(), errors: vec![], full_trace: false, record_incomplete: true };
        Self { dir: PathBuf::new(), events_file: Mutex::new(None), manifest: Mutex::new(manifest), warned: Mutex::new(false) }
    }

    fn warn_once(&self, msg: &str) {
        let mut w = self.warned.lock().expect("run-record warn mutex");
        if !*w {
            eprintln!("warning: run-record {msg}");
            *w = true;
        }
        self.manifest.lock().expect("run-record manifest mutex").record_incomplete = true;
    }

    /// Append one already-serialized JSON object as a line.
    pub fn write_event(&self, line: &str) {
        let mut guard = self.events_file.lock().expect("run-record events mutex");
        let Some(file) = guard.as_mut() else {
            self.warn_once("events file unavailable");
            return;
        };
        if writeln!(file, "{line}").is_err() {
            self.warn_once("event write failed");
        }
    }

    /// Record a manifest error-index entry (called from error events).
    pub fn add_error(&self, err: ManifestError) {
        self.manifest.lock().expect("run-record manifest mutex").errors.push(err);
    }

    /// Merge summary counters into the manifest (called from summary events).
    pub fn set_counts(&self, counts: RunCounts) {
        self.manifest.lock().expect("run-record manifest mutex").counts = counts;
    }

    /// Record the run's deployment (`saas` / `software`) once the profile is resolved.
    /// The manifest is created before profile load, so this lands late but before finalize.
    pub fn set_deployment(&self, deployment: Option<String>) {
        self.manifest.lock().expect("run-record manifest mutex").deployment = deployment;
    }

    /// Set outcome + finished timestamp, flush buffered events, and write
    /// `manifest.json`. Idempotent. This is the events durability point — the
    /// panic/ctrl-c paths reach it via `finalize_active_run` before exit.
    pub fn finalize(&self, outcome: &str) {
        // Flush events before taking the manifest lock (warn_once locks the
        // manifest mutex, which is not reentrant).
        if let Some(w) = self.events_file.lock().expect("run-record events mutex").as_mut()
            && w.flush().is_err()
        {
            self.warn_once("event flush failed");
        }
        let mut m = self.manifest.lock().expect("run-record manifest mutex");
        if m.finished.is_none() {
            m.finished = Some(utc_now_string());
            m.outcome = Some(outcome.to_string());
        }
        // Null sink: dir is empty PathBuf — skip filesystem write silently.
        if self.dir.as_os_str().is_empty() {
            return;
        }
        // Re-ensure the dir exists: a concurrent retention sweep may have pruned
        // it while this (possibly long-running) command was executing. The
        // in-progress guard in `prune_runs` makes this rare, but recreating here
        // is cheap and keeps the manifest write from failing spuriously.
        let _ = fs::create_dir_all(&self.dir);
        let path = self.dir.join("manifest.json");
        match serde_json::to_string_pretty(&*m) {
            Ok(json) => {
                if fs::write(&path, json).is_err() {
                    drop(m);
                    self.warn_once("manifest write failed");
                }
            }
            Err(_) => {
                drop(m);
                self.warn_once("manifest serialize failed");
            }
        }
    }

    /// Absolute path to this run's artifact directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

// ---------------------------------------------------------------------------
// Tracing Layer + global sink slot (moved from wxctl binary so sibling crates
// such as wxctl-mcp can install per-tool-call sinks without importing the
// binary crate).
// ---------------------------------------------------------------------------

use parking_lot::Mutex as PkMutex;
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// Minimal field visitor — private copy so wxctl-core has no dep on the
/// binary crate's `output::field_visitor`.
#[derive(Default)]
struct FieldCollector {
    fields: Vec<(String, String)>,
}

impl FieldCollector {
    fn get(&self, key: &str) -> Option<&str> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
    }
}

impl tracing::field::Visit for FieldCollector {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.fields.push((field.name().to_string(), format!("{:?}", value)));
    }
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.fields.push((field.name().to_string(), value.to_string()));
    }
    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.fields.push((field.name().to_string(), value.to_string()));
    }
    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        self.fields.push((field.name().to_string(), value.to_string()));
    }
    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.fields.push((field.name().to_string(), value.to_string()));
    }
}

static CURRENT_SINK: OnceLock<PkMutex<Option<Arc<RunSink>>>> = OnceLock::new();

fn slot() -> &'static PkMutex<Option<Arc<RunSink>>> {
    CURRENT_SINK.get_or_init(|| PkMutex::new(None))
}

fn current_sink() -> Option<Arc<RunSink>> {
    slot().lock().clone()
}

/// RAII guard that clears the active sink slot on drop.
pub struct RunSinkGuard;

impl Drop for RunSinkGuard {
    fn drop(&mut self) {
        *slot().lock() = None;
    }
}

/// Install the active run sink for the current command. Returns a guard that
/// clears the slot on drop.
pub fn install_run_sink(sink: Arc<RunSink>) -> RunSinkGuard {
    *slot().lock() = Some(sink);
    RunSinkGuard
}

/// Finalize the currently-installed run sink (panic/ctrl-c best-effort path).
pub fn finalize_active_run(outcome: &str) {
    if let Some(sink) = current_sink() {
        sink.finalize(outcome);
    }
}

/// Set the deployment on the active run record, if one is installed (no-op otherwise).
pub fn set_active_run_deployment(deployment: Option<String>) {
    if let Some(sink) = current_sink() {
        sink.set_deployment(deployment);
    }
}

/// Whether the active command is in full-trace mode. Stored alongside the sink
/// in a parallel slot to avoid threading a flag through every event.
static FULL_TRACE: OnceLock<PkMutex<bool>> = OnceLock::new();

fn full_trace_slot() -> &'static PkMutex<bool> {
    FULL_TRACE.get_or_init(|| PkMutex::new(false))
}

pub fn set_full_trace(on: bool) {
    *full_trace_slot().lock() = on;
}

fn full_trace() -> bool {
    *full_trace_slot().lock()
}

/// Span-path of an event's parent chain, e.g. `run>execution>create space.dev`.
fn span_path<S>(ctx: &Context<'_, S>, event: &Event<'_>) -> Option<String>
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    let leaf = ctx.event_span(event)?;
    let names: Vec<String> = leaf.scope().from_root().map(|s| s.name().to_string()).collect();
    if names.is_empty() {
        return None;
    }
    Some(names.join(">"))
}

/// Fourth tracing layer: captures spans + events into the per-command run artifact.
///
/// Same global-slot pattern as `OutputCollectorLayer`: `install_run_sink` populates
/// a global slot for a command's lifetime; the guard clears it on drop.
#[derive(Default)]
pub struct RunRecordLayer;

impl<S> Layer<S> for RunRecordLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(sink) = current_sink() else { return };
        let target = event.metadata().target();
        let level = *event.metadata().level();
        let is_error = target.starts_with("wxctl::error");
        let is_summary = target.starts_with("wxctl::summary");

        // Concise default: drop trace/debug internals and successful-request bodies
        // unless full-trace. Errors + summaries always recorded. HTTP exchange lines
        // for *successful* requests are dropped in concise mode (they carry bodies);
        // failed exchanges surface via the wxctl::error event, which is always kept.
        let is_http = target.starts_with("wxctl::substage::http");
        if !full_trace() && !is_error && !is_summary {
            use tracing::Level;
            if is_http {
                return; // success-exchange bodies only; failures come through wxctl::error
            }
            if level == Level::TRACE || level == Level::DEBUG {
                return;
            }
        }

        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);

        // Build the flat event JSON object.
        let mut obj = serde_json::Map::new();
        obj.insert("ts".into(), utc_now_string().into());
        obj.insert("level".into(), level.to_string().into());
        obj.insert("target".into(), target.into());
        if let Some(path) = span_path(&ctx, event) {
            obj.insert("span".into(), path.into());
        }
        // `src` on errors always; on every event under full-trace.
        if (is_error || full_trace())
            && let (Some(file), Some(line)) = (event.metadata().file(), event.metadata().line())
        {
            obj.insert("src".into(), format!("{file}:{line}").into());
        }
        for (k, v) in &visitor.fields {
            obj.insert(k.clone(), serde_json::Value::String(v.clone()));
        }
        if let Ok(line) = serde_json::to_string(&serde_json::Value::Object(obj)) {
            sink.write_event(&line);
        }

        // Manifest side-channels.
        if is_error {
            sink.add_error(ManifestError {
                code: visitor.get("error_code").unwrap_or("UNKNOWN").to_string(),
                resource: match (visitor.get("resource_type"), visitor.get("resource_name")) {
                    (Some(t), Some(n)) => Some(format!("{t}.{n}")),
                    _ => None,
                },
                message: visitor.get("message").unwrap_or("").to_string(),
                fix: visitor.get("fix").map(|s| s.to_string()),
            });
        }
        if is_summary {
            let g = |k: &str| visitor.get(k).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            sink.set_counts(RunCounts { total: g("total"), created: g("created"), updated: g("updated"), deleted: g("deleted"), noop: g("noop"), retained: g("retained"), failed: g("failed"), skipped: g("skipped") });
        }
    }

    fn on_close(&self, _id: Id, _ctx: Context<'_, S>) {
        // Stage durations are emitted as the existing stage spans' own events via
        // the collector; the run record captures them as events. No-op here keeps
        // the layer lean — span open/close timing lands via wxctl::stage events.
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_id_shape() {
        let id = generate_run_id("apply");
        // YYYYMMDD-HHMMSS-apply-XXXXXX
        let parts: Vec<&str> = id.split('-').collect();
        assert_eq!(parts.len(), 4, "id = {id}");
        assert_eq!(parts[0].len(), 8);
        assert_eq!(parts[1].len(), 6);
        assert_eq!(parts[2], "apply");
        assert_eq!(parts[3].len(), 6);
        assert!(parts[3].chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn civil_epoch_zero_is_1970() {
        assert_eq!(civil_from_epoch(0), (1970, 1, 1, 0, 0, 0));
    }

    #[test]
    fn sink_writes_events_and_manifest() {
        let _env = crate::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-runs-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("WXCTL_RUNS_DIR", &tmp) };
        let manifest = RunManifest {
            run_id: generate_run_id("apply"),
            command: "apply".into(),
            args: vec!["-f".into(), "x.yaml".into()],
            profile: Some("itz".into()),
            deployment: None,
            config_paths: vec!["x.yaml".into()],
            started: utc_now_string(),
            finished: None,
            outcome: None,
            counts: RunCounts::default(),
            errors: vec![],
            full_trace: false,
            record_incomplete: false,
        };
        let sink = RunSink::new(manifest).unwrap();
        sink.write_event(r#"{"ts":"t","level":"INFO","target":"wxctl::stage","span":"run","msg":"x"}"#);
        sink.add_error(ManifestError { code: "E001".into(), resource: Some("space.dev".into()), message: "boom".into(), fix: Some("retry".into()) });
        sink.finalize("failed");
        let dir = sink.dir().to_path_buf();
        let events = fs::read_to_string(dir.join("events.jsonl")).unwrap();
        assert_eq!(events.lines().count(), 1);
        let mtext = fs::read_to_string(dir.join("manifest.json")).unwrap();
        let parsed: RunManifest = serde_json::from_str(&mtext).unwrap();
        assert_eq!(parsed.outcome.as_deref(), Some("failed"));
        assert_eq!(parsed.errors.len(), 1);
        assert!(parsed.finished.is_some());
        let _ = fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var("WXCTL_RUNS_DIR") };
    }

    /// `set_deployment` (and its active-run counterpart's underlying sink method) lands
    /// the run's deployment scope in the persisted manifest — the fix for run-record
    /// manifests hardcoding `deployment: None` regardless of the profile in use.
    #[test]
    fn sink_set_deployment_lands_in_manifest() {
        let _env = crate::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-runs-test-deployment-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("WXCTL_RUNS_DIR", &tmp) };
        let manifest = RunManifest {
            run_id: generate_run_id("apply"),
            command: "apply".into(),
            args: vec!["-f".into(), "x.yaml".into()],
            profile: Some("itz".into()),
            deployment: None,
            config_paths: vec!["x.yaml".into()],
            started: utc_now_string(),
            finished: None,
            outcome: None,
            counts: RunCounts::default(),
            errors: vec![],
            full_trace: false,
            record_incomplete: false,
        };
        let sink = RunSink::new(manifest).unwrap();
        sink.set_deployment(Some("saas".into()));
        sink.finalize("success");
        let dir = sink.dir().to_path_buf();
        let mtext = fs::read_to_string(dir.join("manifest.json")).unwrap();
        assert!(mtext.contains("\"deployment\": \"saas\""), "manifest.json should carry the recorded deployment, got: {mtext}");
        let parsed: RunManifest = serde_json::from_str(&mtext).unwrap();
        assert_eq!(parsed.deployment.as_deref(), Some("saas"));
        let _ = fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var("WXCTL_RUNS_DIR") };
    }

    #[test]
    fn prune_keeps_newest() {
        let _env = crate::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-prune-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        for name in ["20200101-000000-apply-aaaaaa", "20210101-000000-apply-bbbbbb", "20220101-000000-apply-cccccc"] {
            let d = tmp.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("manifest.json"), "{}").unwrap(); // finalized → prunable
        }
        unsafe { std::env::set_var("WXCTL_RUNS_KEEP", "2") };
        prune_runs(&tmp);
        let remaining: Vec<String> = fs::read_dir(&tmp).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(remaining.len(), 2);
        assert!(!remaining.iter().any(|n| n.starts_with("20200101")), "oldest pruned");
        let _ = fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var("WXCTL_RUNS_KEEP") };
    }

    #[test]
    fn prune_skips_in_progress() {
        let _env = crate::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-prune-inflight-{}", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        // Two finalized dirs (have manifest.json) plus one in-progress dir that
        // is the OLDEST (no manifest, mtime = now). A naive oldest-first prune
        // would delete the in-progress dir; the guard must keep it and evict a
        // finalized dir in its place — this is the concurrent-apply race.
        for name in ["20210101-000000-apply-bbbbbb", "20220101-000000-apply-cccccc"] {
            let d = tmp.join(name);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join("manifest.json"), "{}").unwrap();
        }
        let inflight = "20200101-000000-apply-aaaaaa";
        fs::create_dir_all(tmp.join(inflight)).unwrap(); // no manifest.json → in-progress
        unsafe { std::env::set_var("WXCTL_RUNS_KEEP", "2") };
        prune_runs(&tmp);
        let remaining: Vec<String> = fs::read_dir(&tmp).unwrap().filter_map(|e| e.ok()).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        assert!(remaining.iter().any(|n| n == inflight), "in-progress run must survive pruning: {remaining:?}");
        assert_eq!(remaining.len(), 2, "one finalized dir pruned in its place: {remaining:?}");
        let _ = fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var("WXCTL_RUNS_KEEP") };
    }
}
