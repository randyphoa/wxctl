//! Read one run artifact into a typed model. The on-disk format is owned by
//! `crate::logging::run_record`; we re-read `runs_root()` and parse line-by-line.
//! All fields in `events.jsonl` are strings (the `FieldCollector` records every
//! value as a string), so `context`/`error_chain` are JSON-encoded strings the
//! bundle builder decodes lazily.

use crate::logging::run_record::{RunManifest, runs_root};
use anyhow::{Context, Result, anyhow};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

/// One parsed line of `events.jsonl`. The envelope keys (`ts`/`level`/`target`/
/// `span`/`src`) are surfaced directly; all other recorded fields land in `fields`.
#[derive(Debug, Clone)]
pub struct EventLine {
    pub ts: String,
    pub level: String,
    pub target: String,
    pub span: Option<String>,
    pub src: Option<String>,
    /// All remaining string fields (e.g. `error_code`, `message`, `fix`, `cause`,
    /// `resource_type`, `resource_name`, `field_path`, `context`, `error_chain`,
    /// `trace_id`, `decision`, `reason`, `depends_on_type`, …).
    pub fields: BTreeMap<String, String>,
}

impl EventLine {
    fn from_object(obj: &Map<String, Value>) -> Self {
        let take_str = |k: &str| obj.get(k).and_then(Value::as_str).map(str::to_string);
        let mut fields = BTreeMap::new();
        for (k, v) in obj {
            if matches!(k.as_str(), "ts" | "level" | "target" | "span" | "src") {
                continue;
            }
            // Values are strings in practice; fall back to compact JSON for any non-string.
            let s = v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string());
            fields.insert(k.clone(), s);
        }
        Self { ts: take_str("ts").unwrap_or_default(), level: take_str("level").unwrap_or_default(), target: take_str("target").unwrap_or_default(), span: take_str("span"), src: take_str("src"), fields }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.fields.get(key).map(String::as_str)
    }

    pub fn is_error(&self) -> bool {
        self.target.starts_with("wxctl::error")
    }

    pub fn is_decision(&self) -> bool {
        self.target.starts_with("wxctl::decision")
    }

    pub fn is_dependency(&self) -> bool {
        self.target.starts_with("wxctl::dependency")
    }
}

/// Build an `EventLine` from a JSON object value. `#[cfg(test)]` — its only
/// consumer is the bundle builder's tests. Gated here to keep clippy clean
/// under `-D warnings` in non-test builds.
#[cfg(test)]
pub(crate) fn event_line_from_value(v: &Value) -> EventLine {
    match v {
        Value::Object(obj) => EventLine::from_object(obj),
        _ => EventLine { ts: String::new(), level: String::new(), target: String::new(), span: None, src: None, fields: BTreeMap::new() },
    }
}

/// A fully-loaded run artifact: the manifest plus every parsed event line, in file order.
#[derive(Debug, Clone)]
pub struct RunArtifact {
    pub run_id: String,
    pub dir: PathBuf,
    pub manifest: RunManifest,
    pub events: Vec<EventLine>,
}

impl RunArtifact {
    /// Error events in file order.
    pub fn errors(&self) -> impl Iterator<Item = &EventLine> {
        self.events.iter().filter(|e| e.is_error())
    }
}

/// One row in `wxctl runs list`: enough to choose a run without loading its events.
#[derive(Debug, Clone)]
pub struct RunSummary {
    pub run_id: String,
    pub command: String,
    pub started: String,
    /// `success` | `failed` | `aborted` | `unknown` (no/partial manifest).
    pub outcome: String,
    pub error_count: usize,
}

/// Path to a run directory under `runs_root()`.
fn run_dir(run_id: &str) -> PathBuf {
    runs_root().join(run_id)
}

/// Load one artifact by `run_id`. Errors actionably when the dir or manifest is
/// missing/corrupt — never panics (spec §Error Handling). A missing `events.jsonl`
/// yields an empty event list rather than an error (manifest still diagnosable).
pub fn load_artifact(run_id: &str) -> Result<RunArtifact> {
    let dir = run_dir(run_id);
    if !dir.is_dir() {
        return Err(anyhow!("no run record found for '{run_id}' under {}", runs_root().display()));
    }
    let manifest_path = dir.join("manifest.json");
    let manifest_text = std::fs::read_to_string(&manifest_path).with_context(|| format!("run '{run_id}' has no readable manifest.json (run may have aborted before finalize): {}", manifest_path.display()))?;
    let manifest: RunManifest = serde_json::from_str(&manifest_text).with_context(|| format!("run '{run_id}' manifest.json is corrupt"))?;

    let events_text = std::fs::read_to_string(dir.join("events.jsonl")).unwrap_or_default();
    let mut events = Vec::new();
    for line in events_text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        // Skip unparsable lines rather than failing the whole load — a truncated
        // tail (aborted run) must not block diagnosis of the lines that did land.
        if let Ok(Value::Object(obj)) = serde_json::from_str::<Value>(line) {
            events.push(EventLine::from_object(&obj));
        }
    }
    Ok(RunArtifact { run_id: run_id.to_string(), dir, manifest, events })
}

/// List all runs newest-first. Run dir names sort lexically by their timestamp
/// prefix (same invariant `prune_runs` relies on), so a reverse name-sort is
/// age-descending. Best-effort: a dir with an unreadable/corrupt manifest still
/// appears with `outcome: "unknown"` so the user can still target it.
pub fn list_runs() -> Vec<RunSummary> {
    let root = runs_root();
    let Ok(entries) = std::fs::read_dir(&root) else { return Vec::new() };
    let mut dirs: Vec<PathBuf> = entries.filter_map(|e| e.ok()).map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    dirs.reverse();
    dirs.into_iter()
        .map(|dir| {
            let run_id = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            match std::fs::read_to_string(dir.join("manifest.json")).ok().and_then(|t| serde_json::from_str::<RunManifest>(&t).ok()) {
                Some(m) => RunSummary { run_id, command: m.command, started: m.started, outcome: m.outcome.unwrap_or_else(|| "unknown".to_string()), error_count: m.errors.len() },
                None => RunSummary { run_id, command: String::new(), started: String::new(), outcome: "unknown".to_string(), error_count: 0 },
            }
        })
        .collect()
}

/// The most recent run whose outcome is `failed` or `aborted`. `None` when there
/// is no such run (caller turns this into an actionable "no failed runs" message).
pub fn find_latest_failed() -> Option<String> {
    list_runs().into_iter().find(|r| r.outcome == "failed" || r.outcome == "aborted").map(|r| r.run_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::run_record::{RunCounts, RunManifest, RunSink, generate_run_id, utc_now_string};

    /// Seed an artifact dir under a temp `WXCTL_RUNS_DIR` and assert the reader
    /// round-trips the manifest, error events, and the latest-failed selector.
    #[test]
    fn loads_manifest_and_events_and_finds_latest_failed() {
        let _env = crate::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-diag-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("WXCTL_RUNS_DIR", &tmp) };

        let run_id = generate_run_id("apply");
        let manifest = RunManifest {
            run_id: run_id.clone(),
            command: "apply".into(),
            args: vec![],
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
        sink.write_event(r#"{"ts":"t","level":"INFO","target":"wxctl::decision","span":"run>reconciliation","resource_type":"space","resource_name":"dev","decision":"create","reason":"absent"}"#);
        sink.write_event(r#"{"ts":"t","level":"ERROR","target":"wxctl::error","span":"run>execution","src":"crates/x.rs:1","error_code":"WXCTL-H001","resource_type":"space","resource_name":"dev","message":"HTTP 404","fix":"check the instance guid","context":"{\"request_body\":{\"name\":\"dev\"},\"response_body\":{\"error\":\"not found\"}}"}"#);
        sink.finalize("failed");
        drop(sink);

        let art = load_artifact(&run_id).unwrap();
        assert_eq!(art.manifest.command, "apply");
        assert_eq!(art.events.len(), 2);
        let err = art.errors().next().unwrap();
        assert_eq!(err.get("error_code"), Some("WXCTL-H001"));
        assert_eq!(err.src.as_deref(), Some("crates/x.rs:1"));
        assert!(err.get("context").unwrap().contains("not found"));
        assert_eq!(find_latest_failed().as_deref(), Some(run_id.as_str()));

        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::remove_var("WXCTL_RUNS_DIR") };
    }

    #[test]
    fn missing_run_is_actionable_error_not_panic() {
        let _env = crate::test_env_lock();
        let tmp = std::env::temp_dir().join(format!("wxctl-diag-missing-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        unsafe { std::env::set_var("WXCTL_RUNS_DIR", &tmp) };
        let err = load_artifact("nope-does-not-exist").unwrap_err();
        assert!(err.to_string().contains("no run record found"));
        unsafe { std::env::remove_var("WXCTL_RUNS_DIR") };
    }
}
