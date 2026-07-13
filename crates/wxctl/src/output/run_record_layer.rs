//! The run-record tracing Layer + sink-install plumbing now lives in
//! `wxctl_core::logging::run_record` so the MCP server (a sibling crate that
//! cannot import this binary crate) can install a per-tool-call sink against the
//! same global subscriber. This module re-exports them so `main.rs` and
//! `commands/common.rs` keep their `crate::output::…` import paths unchanged.

pub use wxctl_core::logging::run_record::{RunRecordLayer, RunSinkGuard, finalize_active_run, install_run_sink, set_full_trace};
