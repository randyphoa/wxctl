//! DTOs + shaping + progress-bridge observers for the Phase 3 mutating tools
//! (`wxctl_apply`, `wxctl_destroy`, `wxctl_test`).
//!
//! Output is trimmed: apply/destroy return succeeded/failed/skipped keys + counts,
//! with the raw per-resource API `response` gated behind `verbose: true`; test returns
//! per-case pass/fail + per-turn outcome labels.
//!
//! The engine `ExecutionObserver` and the SDK `TestObserver` are **synchronous**
//! (`fn on_*(&self, ..)`), but MCP progress is sent via `Peer::notify_progress`, which
//! is **async**. The two observer types here bridge that gap: each callback pushes a
//! `ProgressEvent` onto a `tokio::sync::mpsc::unbounded_channel`; the server spawns a
//! task (see `crate::server`) that drains the receiver and awaits `notify_progress`.

use tokio::sync::mpsc::UnboundedSender;
use wxctl_engine::ExecutionObserver;
use wxctl_sdk::TestObserver;

/// A progress step to forward to the MCP client. `message` is a short human label;
/// `done` is a monotonically increasing completed-count for the `progress` field.
#[derive(Debug, Clone)]
pub struct ProgressEvent {
    pub done: f64,
    pub message: String,
}

/// Bridges the engine's synchronous `ExecutionObserver` callbacks to async MCP progress
/// by pushing `ProgressEvent`s onto an unbounded channel. A monotonic counter drives the
/// `done` field. A closed channel (drain task gone) is benign — sends are best-effort.
pub struct ProgressExecutionObserver {
    tx: UnboundedSender<ProgressEvent>,
    done: std::sync::atomic::AtomicU64,
}

impl ProgressExecutionObserver {
    pub fn new(tx: UnboundedSender<ProgressEvent>) -> Self {
        Self { tx, done: std::sync::atomic::AtomicU64::new(0) }
    }

    fn emit(&self, message: String) {
        let done = self.done.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
        let _ = self.tx.send(ProgressEvent { done: done as f64, message });
    }
}

impl ExecutionObserver for ProgressExecutionObserver {
    fn on_task_start(&self, key: &wxctl_core::ResourceKey) {
        let _ = self.tx.send(ProgressEvent { done: self.done.load(std::sync::atomic::Ordering::Relaxed) as f64, message: format!("start {}/{}", key.kind, key.name) });
    }

    fn on_task_complete(&self, key: &wxctl_core::ResourceKey, success: bool, _duration: std::time::Duration, _response: Option<&serde_json::Value>) {
        self.emit(format!("{} {}/{}", if success { "ok" } else { "failed" }, key.kind, key.name));
    }

    fn on_task_skipped(&self, key: &wxctl_core::ResourceKey, _reason: &str) {
        self.emit(format!("skipped {}/{}", key.kind, key.name));
    }

    fn on_task_error(&self, key: &wxctl_core::ResourceKey, error: &str) {
        let _ = self.tx.send(ProgressEvent { done: self.done.load(std::sync::atomic::Ordering::Relaxed) as f64, message: format!("error {}/{}: {error}", key.kind, key.name) });
    }
}

/// Bridges the SDK's synchronous `TestObserver` callbacks to async MCP progress.
pub struct ProgressTestObserver {
    tx: UnboundedSender<ProgressEvent>,
}

impl ProgressTestObserver {
    pub fn new(tx: UnboundedSender<ProgressEvent>) -> Self {
        Self { tx }
    }
}

impl TestObserver for ProgressTestObserver {
    fn on_test_start(&self, test_name: &str) {
        let _ = self.tx.send(ProgressEvent { done: 0.0, message: format!("start test {test_name}") });
    }

    fn on_test_complete(&self, test_name: &str, passed: bool, completed: usize, total: usize) {
        let _ = self.tx.send(ProgressEvent { done: completed as f64, message: format!("{}/{} {} {}", completed, total, if passed { "passed" } else { "failed" }, test_name) });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wxctl_core::ResourceKey;

    #[test]
    fn progress_observer_emits_on_complete() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let obs = ProgressExecutionObserver::new(tx);
        obs.on_task_complete(&ResourceKey::new("space", "a"), true, std::time::Duration::from_secs(1), None);
        let ev = rx.try_recv().expect("event sent");
        assert_eq!(ev.done, 1.0);
        assert!(ev.message.contains("ok space/a"));
    }
}
