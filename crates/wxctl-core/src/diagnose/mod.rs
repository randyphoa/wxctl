//! Run-record diagnosis: read one artifact (`manifest.json` + `events.jsonl`) into a
//! typed model, classify each error, match `docs/troubleshoot/` entries, and render
//! an agent-ready bundle (markdown + JSON). Pure — reads the filesystem, emits no
//! tracing events, and never mutates an artifact. Shared by the CLI (Phase 2) and,
//! later, the MCP server (Phase 3). The artifact format is owned by
//! `crate::logging::run_record`; this module only consumes it.

mod artifact;
mod bundle;
mod triage;
mod troubleshoot;

pub use artifact::{EventLine, RunArtifact, RunSummary, find_latest_failed, list_runs, load_artifact};
pub use bundle::{DiagnosisBundle, ErrorBlock, Exchange, build_bundle};
pub use triage::{TriageClass, classify};
pub use troubleshoot::{TroubleshootMatch, match_troubleshoot};
