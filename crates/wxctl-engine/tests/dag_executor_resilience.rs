//! DagExecutor resilience gates.
//!
//! 1. A panicking spawned task must NOT hang the executor: the senders live in
//!    `ExecutionState`, so the result channel never closes — before the
//!    catch_unwind guard, a panicked task simply never sent its result and
//!    `collect_results` waited forever (default config has no total_timeout).
//! 2. Cancellation / total timeout must NOT discard partially-collected
//!    results: real cloud mutations completed before the cut must survive into
//!    `ExecutionResults` (run records, timeout message).
//!
//! Credential-free: operations carry `local: None`, so `create_clients` builds
//! no HTTP client and no request is ever issued; panics/cancels are injected
//! through the `ExecutionObserver` hooks.

use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use wxctl_core::{ClientFactory, ConcurrencyConfig, IndexGraph, ResourceKey, ResourceRegistry};
use wxctl_engine::{DagExecutor, ExecutionObserver, ExecutorConfig, Operation, OperationType, RuntimeIdStore};

/// A throwaway profile so `ClientFactory::new` succeeds offline. No client is
/// ever created from it (all test operations have `local: None`).
fn factory() -> Arc<ClientFactory> {
    let tmp = std::env::temp_dir().join(format!("wxctl-dag-resilience-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(&tmp, r#"{ "profiles": { "test-dag": { "deployment": "saas" } } }"#).expect("write tmp profile");
    let f = ClientFactory::new("test-dag", Some(tmp.to_str().unwrap()), &ConcurrencyConfig::default()).expect("factory new");
    let _ = std::fs::remove_file(&tmp);
    Arc::new(f)
}

fn op(name: &str, op_type: OperationType) -> Operation {
    Operation { key: ResourceKey::new("thing", name), op_type, local: None, remote: None }
}

/// Two-node graph where `b` depends on `a` (node order [a, b] to match the ops).
fn graph_b_depends_on_a() -> IndexGraph<ResourceKey> {
    let mut graph = IndexGraph::new();
    graph.add_node(ResourceKey::new("thing", "a"));
    graph.add_node(ResourceKey::new("thing", "b"));
    graph.add_edge(ResourceKey::new("thing", "b"), ResourceKey::new("thing", "a"));
    graph
}

struct PanicOnStart;
impl ExecutionObserver for PanicOnStart {
    fn on_task_start(&self, key: &ResourceKey) {
        if key.name.as_ref() == "a" {
            panic!("injected panic in task a");
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn panicking_task_fails_op_and_skips_dependents_without_hanging() {
    let executor = DagExecutor::with_observer(Arc::new(ResourceRegistry::new()), factory(), ExecutorConfig::default(), Arc::new(PanicOnStart));
    let ops = vec![op("a", OperationType::Create), op("b", OperationType::Create)];

    // Default config has total_timeout: None — without the panic guard this recv-loops forever.
    let results = tokio::time::timeout(Duration::from_secs(10), executor.execute("test-op-panic", ops, &graph_b_depends_on_a())).await.expect("executor must not hang on a panicking task").expect("executor returns results");

    assert_eq!(results.failed.len(), 1, "the panicking op must be reported as failed: {results:?}");
    assert_eq!(results.failed[0].key, ResourceKey::new("thing", "a"));
    let err = results.failed[0].error.as_deref().unwrap_or("");
    assert!(err.contains("task panicked") && err.contains("injected panic in task a"), "failure must carry the panic payload, got: {err}");
    assert_eq!(results.skipped, vec![ResourceKey::new("thing", "b")], "the dependent must be cascade-skipped");
    assert!(results.succeeded.is_empty());
    assert!(!results.cancelled);
}

/// Cancels the run when task `b` starts — by which point `a`'s Success has
/// already been collected (collect_results pushes the result BEFORE spawning
/// dependents), so `a` must survive into `succeeded` despite the cancel.
struct CancelOnStart {
    token: CancellationToken,
}
impl ExecutionObserver for CancelOnStart {
    fn on_task_start(&self, key: &ResourceKey) {
        if key.name.as_ref() == "b" {
            self.token.cancel();
            // Hold b briefly so the executor's `select!` loop observes the cancel
            // before b's (near-instant, `local: None`) result completes collection.
            // Without this, on a loaded runner the run can finish via the
            // `collect_future` arm before the biased `cancel.cancelled()` arm sees
            // the cross-thread store, leaving `cancelled` racily false (flaky CI).
            std::thread::sleep(Duration::from_millis(100));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_preserves_partially_collected_results() {
    let token = CancellationToken::new();
    let executor = DagExecutor::with_observer(Arc::new(ResourceRegistry::new()), factory(), ExecutorConfig::default(), Arc::new(CancelOnStart { token: token.clone() }));
    // `a` is a NoOp (fast-path success); `b` depends on it and triggers the cancel.
    let ops = vec![op("a", OperationType::NoOp), op("b", OperationType::Create)];

    let results = tokio::time::timeout(Duration::from_secs(10), executor.execute_with_cancel("test-op-cancel", ops, &graph_b_depends_on_a(), token, false, RuntimeIdStore::new())).await.expect("cancel must terminate the run").expect("executor returns results");

    assert!(results.cancelled, "run must be flagged cancelled");
    assert_eq!(results.succeeded.len(), 1, "a's completed result must survive cancellation: {results:?}");
    assert_eq!(results.succeeded[0].key, ResourceKey::new("thing", "a"));
    // b is accounted either as a real failure (cancelled mid-flight) or as skipped
    // (post-cancel accounting) depending on abort timing — never lost.
    assert!(results.contains_key(&ResourceKey::new("thing", "b")), "b must be accounted after cancel: {results:?}");
    assert_eq!(results.total_processed(), 2);
}

/// Blocks task `b` past the total timeout so the run times out after `a`
/// succeeded — the error must report the REAL counts, not "0 succeeded".
struct BlockOnStart;
impl ExecutionObserver for BlockOnStart {
    fn on_task_start(&self, key: &ResourceKey) {
        if key.name.as_ref() == "b" {
            std::thread::sleep(Duration::from_secs(3));
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn total_timeout_reports_real_counts() {
    let executor = DagExecutor::with_observer(Arc::new(ResourceRegistry::new()), factory(), ExecutorConfig::default().with_total_timeout(1), Arc::new(BlockOnStart));
    let ops = vec![op("a", OperationType::NoOp), op("b", OperationType::Create)];

    let err = tokio::time::timeout(Duration::from_secs(10), executor.execute("test-op-timeout", ops, &graph_b_depends_on_a())).await.expect("timeout must terminate the run").expect_err("total timeout must surface as an error");

    let msg = err.to_string();
    assert!(msg.contains("Total execution timeout"), "unexpected error: {msg}");
    assert!(msg.contains("1 succeeded"), "timeout message must report the real success count, got: {msg}");
}
