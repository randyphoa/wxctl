use super::errors::ExecutionError;
use super::operations;
use super::types::{ExecutionResult, ExecutionResults, ExecutorConfig};
use crate::context::RuntimeIdStore;
use crate::reconciliation::types::{Operation, OperationType};
use anyhow::{Result, anyhow};
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, info_span};
use wxctl_core::{ClientFactory, HttpClient, IndexGraph, ResourceKey, ResourceRegistry};

/// Channel buffer size multiplier relative to parallelism.
///
/// Set to 3x to prevent backpressure when tasks complete in bursts:
/// - 1x for in-flight tasks
/// - 1x for tasks completing simultaneously
/// - 1x for newly spawned dependent tasks
const CHANNEL_BUFFER_MULTIPLIER: usize = 3;

pub trait ExecutionObserver: Send + Sync {
    fn on_task_start(&self, _key: &ResourceKey) {}
    /// `response` carries the operation's API response (the create/update body), so
    /// observers can surface the backend-assigned resource id. `None` for deletes,
    /// skips, or responses without a body.
    fn on_task_complete(&self, _key: &ResourceKey, _success: bool, _duration: Duration, _response: Option<&serde_json::Value>) {}
    fn on_task_skipped(&self, _key: &ResourceKey, _reason: &str) {}
    fn on_task_error(&self, _key: &ResourceKey, _error: &str) {}

    /// Reconciliation began; `total` is the number of resources that will be
    /// processed sequentially. Default no-op so non-CLI consumers (SDK, MCP,
    /// tests, `NoOpObserver`) are unaffected.
    fn on_reconcile_start(&self, _total: usize) {}
    /// About to discover `key` against the backend (sequential, topological order).
    fn on_reconcile_resource_start(&self, _key: &ResourceKey) {}
    /// Discovery + enrichment for `key` resolved; advances the done counter.
    /// `success` is `false` on the discovery-error branch (display-only — the
    /// existing reconcile error-tolerance is unchanged).
    fn on_reconcile_resource_complete(&self, _key: &ResourceKey, _success: bool) {}
}

pub struct NoOpObserver;
impl ExecutionObserver for NoOpObserver {}

#[derive(Debug)]
enum TaskResult {
    Success { idx: usize, result: ExecutionResult },
    Failure { result: ExecutionResult },
    Skipped { key: ResourceKey },
}

/// Send a task result, logging at debug level if the channel is closed.
/// A closed channel means the executor was cancelled or shut down; dropping the
/// result is benign, but we log it so the cause of a missing result is traceable.
async fn send_result(state: &ExecutionState, result: TaskResult) {
    if let Err(e) = state.result_tx.send(result).await {
        tracing::debug!(
            target: "wxctl::substage::execution",
            operation_id = %state.operation_id,
            dropped_result = ?e.0,
            "result channel closed; task result dropped"
        );
    }
}

pub(super) struct ExecutionState {
    in_degree: Vec<AtomicUsize>,
    dependents: Vec<Vec<usize>>,
    failed: Vec<AtomicBool>,
    pub(super) operations: Vec<Operation>,
    pub(super) runtime_ids: RuntimeIdStore,
    pub(super) clients: HashMap<String, HttpClient>,
    pub(super) registry: Arc<ResourceRegistry>,
    pub(super) config: ExecutorConfig,
    cancel: CancellationToken,
    observer: Arc<dyn ExecutionObserver>,
    result_tx: mpsc::Sender<TaskResult>,
    pub(super) operation_id: Arc<str>,
}

pub struct DagExecutor {
    registry: Arc<ResourceRegistry>,
    client_factory: Arc<ClientFactory>,
    config: ExecutorConfig,
    observer: Arc<dyn ExecutionObserver>,
}

impl DagExecutor {
    pub fn new(registry: Arc<ResourceRegistry>, client_factory: Arc<ClientFactory>, config: ExecutorConfig) -> Self {
        Self { registry, client_factory, config, observer: Arc::new(NoOpObserver) }
    }

    pub fn with_observer(registry: Arc<ResourceRegistry>, client_factory: Arc<ClientFactory>, config: ExecutorConfig, observer: Arc<dyn ExecutionObserver>) -> Self {
        Self { registry, client_factory, config, observer }
    }

    pub async fn execute(&self, operation_id: &str, operations: Vec<Operation>, graph: &IndexGraph<ResourceKey>) -> Result<ExecutionResults> {
        self.execute_with_cancel(operation_id, operations, graph, CancellationToken::new(), false, RuntimeIdStore::new()).await
    }

    /// Destroy variant that pre-seeds the executor's runtime store with state
    /// collected during reconciliation. Required so reverse-topological delete
    /// tasks can look up their not-yet-deleted parents for `__ref__*`
    /// enrichment — the default empty store leaves e.g. s3_object's
    /// `pre_delete` unable to reach the linked s3_bucket.
    pub async fn execute_destroy_seeded(&self, operation_id: &str, operations: Vec<Operation>, graph: &IndexGraph<ResourceKey>, seed: RuntimeIdStore) -> Result<ExecutionResults> {
        self.execute_with_cancel(operation_id, operations, graph, CancellationToken::new(), true, seed).await
    }

    pub async fn execute_with_cancel(&self, operation_id: &str, operations: Vec<Operation>, graph: &IndexGraph<ResourceKey>, cancel: CancellationToken, for_destroy: bool, runtime_ids: RuntimeIdStore) -> Result<ExecutionResults> {
        let span = info_span!(
            target: "wxctl::stage::execution",
            "execution",
            operation_id = %operation_id,
            resource_count = operations.len(),
            for_destroy = for_destroy
        );

        async {
            let execution_start = Instant::now();
            let n = operations.len();
            if n == 0 {
                return Ok(ExecutionResults::empty());
            }

            let operations = reorder_operations_to_graph(operations, graph)?;
            let (in_degree_vec, dependents_vec) = build_execution_state(graph, for_destroy);
            let clients = self.create_clients(&operations)?;

            let (result_tx, result_rx) = mpsc::channel::<TaskResult>(self.config.parallelism * CHANNEL_BUFFER_MULTIPLIER);

            let state = Arc::new(ExecutionState {
                in_degree: in_degree_vec.into_iter().map(AtomicUsize::new).collect(),
                dependents: dependents_vec,
                failed: (0..n).map(|_| AtomicBool::new(false)).collect(),
                operations,
                runtime_ids,
                clients,
                registry: self.registry.clone(),
                config: self.config.clone(),
                cancel: cancel.clone(),
                observer: self.observer.clone(),
                result_tx,
                operation_id: Arc::from(operation_id),
            });

            let semaphore = Arc::new(Semaphore::new(self.config.parallelism));
            let mut join_set = JoinSet::new();

            let initial_count = spawn_ready_tasks(&state, &semaphore, &mut join_set, None);

            if initial_count == 0 {
                return Err(anyhow!("No resources ready to execute (cycle in dependencies?)"));
            }

            let collect_future = collect_results(result_rx, n, state.clone(), semaphore.clone(), &mut join_set);

            let total_timeout = self.config.total_timeout;

            let (mut results, timed_out) = if let Some(timeout) = total_timeout {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        (ExecutionResults {
                            cancelled: true,
                            ..ExecutionResults::with_capacity(n)
                        }, false)
                    }
                    result = tokio::time::timeout(timeout, collect_future) => {
                        match result {
                            Ok(Ok(r)) => (r, false),
                            Ok(Err(e)) => return Err(e),
                            Err(_) => {
                                cancel.cancel();
                                (ExecutionResults {
                                    cancelled: true,
                                    ..ExecutionResults::with_capacity(n)
                                }, true)
                            }
                        }
                    }
                }
            } else {
                tokio::select! {
                    biased;
                    _ = cancel.cancelled() => {
                        (ExecutionResults {
                            cancelled: true,
                            ..ExecutionResults::with_capacity(n)
                        }, false)
                    }
                    result = collect_future => (result?, false)
                }
            };

            join_set.abort_all();
            while join_set.join_next().await.is_some() {}

            if results.cancelled || timed_out {
                let accounted: HashSet<ResourceKey> = results.succeeded.iter().map(|r| r.key.clone()).chain(results.failed.iter().map(|r| r.key.clone())).chain(results.skipped.iter().cloned()).collect();
                for idx in 0..n {
                    let key = &state.operations[idx].key;
                    if !accounted.contains(key) {
                        state.observer.on_task_skipped(key, "execution cancelled or timed out");
                        results.skipped.push(key.clone());
                    }
                }
            }

            if timed_out {
                return Err(anyhow!("Total execution timeout ({:?}) exceeded; {} succeeded, {} failed, {} skipped", total_timeout.unwrap(), results.succeeded.len(), results.failed.len(), results.skipped.len()));
            }

            // Emit operation summary for LLM triage (single pass)
            let (mut created, mut updated, mut deleted, mut noop, mut retained, mut skipped_absent, mut skipped_deferred) = (0usize, 0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
            for r in &results.succeeded {
                match r.operation {
                    OperationType::Create => created += 1,
                    OperationType::Update { .. } => updated += 1,
                    OperationType::Delete => deleted += 1,
                    OperationType::NoOp => noop += 1,
                    OperationType::Recreate => created += 1,
                    OperationType::Retain => retained += 1,
                    OperationType::Skip { reason: crate::reconciliation::types::SkipReason::Absent } => skipped_absent += 1,
                    OperationType::Skip { reason: crate::reconciliation::types::SkipReason::Deferred } => skipped_deferred += 1,
                }
            }
            let failed = results.failed.len();
            let skipped = results.skipped.len();
            let total = results.total_processed();
            let error_codes_str = std::iter::repeat_n(wxctl_core::logging::error_codes::E001, failed).collect::<Vec<_>>().join(",");
            let duration_ms = execution_start.elapsed().as_millis() as u64;

            wxctl_core::log_summary!(operation_id, total, created, updated, deleted, noop, retained, failed, skipped, skipped_absent, skipped_deferred, duration_ms, &error_codes_str);

            Ok(results)
        }
        .instrument(span)
        .await
    }

    fn create_clients(&self, operations: &[Operation]) -> Result<HashMap<String, HttpClient>> {
        let services: HashSet<String> = operations.iter().filter_map(|op| op.local.as_ref().map(|l| l.descriptor.service.clone())).collect();

        let mut clients = HashMap::new();
        for service in services {
            let client = self.client_factory.create_client(&service)?;
            clients.insert(service, client);
        }
        Ok(clients)
    }
}

/// Build execution state for DAG execution.
///
/// Returns (in_degree, dependents) where:
/// - in_degree[i] = number of dependencies for node i
/// - dependents[i] = indices of nodes that depend on node i
///
/// When `for_destroy` is true, edge direction is reversed for destroy operations
/// (dependents must be deleted before their dependencies).
fn build_execution_state(graph: &IndexGraph<ResourceKey>, for_destroy: bool) -> (Vec<usize>, Vec<Vec<usize>>) {
    if !for_destroy {
        return graph.build_in_degree_and_dependents();
    }

    // Destroy reverses edge direction: dependents must be deleted before their
    // dependencies.
    let n = graph.len();
    let mut in_degree = vec![0usize; n];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

    for (idx, deps_out) in dependents.iter_mut().enumerate() {
        for dep_idx in graph.dependency_indices(idx) {
            debug_assert!(dep_idx < n, "Invalid dependency index {} >= {} for node {}", dep_idx, n, idx);
            in_degree[dep_idx] += 1;
            deps_out.push(dep_idx);
        }
    }

    (in_degree, dependents)
}

fn reorder_operations_to_graph(operations: Vec<Operation>, graph: &IndexGraph<ResourceKey>) -> Result<Vec<Operation>> {
    let n = operations.len();
    if n != graph.len() {
        return Err(anyhow!("Operation count ({}) doesn't match graph size ({})", n, graph.len()));
    }

    // Fast path: check if already ordered correctly
    let already_ordered = operations.iter().enumerate().all(|(i, op)| graph.get_node(i) == Some(&op.key));
    if already_ordered {
        return Ok(operations);
    }

    // Slow path: reorder via HashMap
    let mut op_map: HashMap<ResourceKey, Operation> = HashMap::with_capacity(n);
    for op in operations {
        use std::collections::hash_map::Entry;
        match op_map.entry(op.key.clone()) {
            Entry::Vacant(e) => {
                e.insert(op);
            }
            Entry::Occupied(e) => {
                return Err(anyhow!("Duplicate operation key: {:?}", e.key()));
            }
        }
    }

    let mut reordered = Vec::with_capacity(n);
    for idx in 0..n {
        let key = graph.get_node(idx).ok_or_else(|| anyhow!("Graph node {} not found", idx))?;
        let op = op_map.remove(key).ok_or_else(|| anyhow!("No operation for graph node {}: {:?}", idx, key))?;
        reordered.push(op);
    }

    Ok(reordered)
}

fn spawn_ready_tasks(state: &Arc<ExecutionState>, semaphore: &Arc<Semaphore>, join_set: &mut JoinSet<()>, completed_idx: Option<usize>) -> usize {
    let mut spawned = 0;

    let all_indices: Vec<usize>;
    let indices_to_check: &[usize] = if let Some(idx) = completed_idx {
        &state.dependents[idx]
    } else {
        all_indices = (0..state.operations.len()).collect();
        &all_indices
    };

    for &idx in indices_to_check {
        if state.failed[idx].load(Ordering::Acquire) {
            continue;
        }

        if state.in_degree[idx].load(Ordering::Acquire) == 0 && state.in_degree[idx].compare_exchange(0, usize::MAX, Ordering::AcqRel, Ordering::Acquire).is_ok() {
            spawned += 1;
            let state = state.clone();
            let semaphore = semaphore.clone();

            join_set.spawn(async move {
                execute_task(idx, state, semaphore).await;
            });
        }
    }

    spawned
}

async fn execute_task(idx: usize, state: Arc<ExecutionState>, semaphore: Arc<Semaphore>) {
    if state.cancel.is_cancelled() {
        let key = state.operations[idx].key.clone();
        state.observer.on_task_skipped(&key, "execution cancelled");
        send_result(&state, TaskResult::Skipped { key }).await;
        return;
    }

    let permit = match semaphore.acquire().await {
        Ok(permit) => permit,
        Err(_) => {
            let key = state.operations[idx].key.clone();
            state.observer.on_task_skipped(&key, "executor shutting down");
            send_result(&state, TaskResult::Skipped { key }).await;
            return;
        }
    };

    if state.failed[idx].load(Ordering::Acquire) {
        let key = state.operations[idx].key.clone();
        state.observer.on_task_skipped(&key, "dependency failed during scheduling");
        send_result(&state, TaskResult::Skipped { key }).await;
        return;
    }

    let planned_op = &state.operations[idx];
    let key = planned_op.key.clone();
    let start_time = Instant::now();

    if matches!(planned_op.op_type, OperationType::NoOp | OperationType::Retain | OperationType::Skip { .. }) {
        let op = planned_op;
        if let Some(remote) = &op.remote
            && remote.exists
        {
            state.runtime_ids.insert(op.key.clone(), remote.data.clone());
        }

        state.observer.on_task_start(&key);
        state.observer.on_task_complete(&key, true, start_time.elapsed(), op.remote.as_ref().map(|r| &r.data));

        send_result(&state, TaskResult::Success { idx, result: ExecutionResult { key: op.key.clone(), operation: op.op_type.clone(), success: true, error: None, response: op.remote.as_ref().map(|r| r.data.clone()), attempts: 0 } }).await;
        return;
    }

    state.observer.on_task_start(&key);

    let result = execute_with_timeout(planned_op, &state).await;
    let duration = start_time.elapsed();

    match result {
        Ok(exec_result) => {
            if let Some(ref response) = exec_result.response {
                state.runtime_ids.insert(exec_result.key.clone(), response.clone());
            }
            state.observer.on_task_complete(&key, true, duration, exec_result.response.as_ref());
            drop(permit);
            send_result(&state, TaskResult::Success { idx, result: exec_result }).await;
        }
        Err(exec_result) => {
            state.failed[idx].store(true, Ordering::Release);

            let error_msg = exec_result.error.as_deref().unwrap_or("Unknown error");

            wxctl_core::log_error_resource!(&state.operation_id, "execution", wxctl_core::logging::error_codes::E001, &exec_result.key.kind, &exec_result.key.name, error_msg, "Check the error message and fix the resource configuration");

            state.observer.on_task_error(&key, error_msg);
            state.observer.on_task_complete(&key, false, duration, exec_result.response.as_ref());
            drop(permit);
            propagate_failure(&state, idx).await;
            send_result(&state, TaskResult::Failure { result: exec_result }).await;
        }
    }
}

async fn execute_with_timeout<'a>(planned_op: &'a Operation, state: &'a ExecutionState) -> Result<ExecutionResult, ExecutionResult> {
    let config = &state.config;
    let key = &planned_op.key;

    if state.cancel.is_cancelled() {
        return Err(ExecutionResult { key: key.clone(), operation: planned_op.op_type.clone(), success: false, error: Some(ExecutionError::Cancelled.message()), response: None, attempts: 0 });
    }

    let result = tokio::time::timeout(config.operation_timeout, operations::execute_single_operation(planned_op, state)).await;

    match result {
        Ok(Ok(mut exec_result)) => {
            exec_result.attempts = 1;
            Ok(exec_result)
        }
        Ok(Err(e)) => {
            let exec_error = ExecutionError::from_anyhow(&e);
            Err(ExecutionResult { key: key.clone(), operation: planned_op.op_type.clone(), success: false, error: Some(exec_error.message()), response: None, attempts: 1 })
        }
        Err(_) => Err(ExecutionResult { key: key.clone(), operation: planned_op.op_type.clone(), success: false, error: Some(format!("Operation timed out after {:?}", config.operation_timeout)), response: None, attempts: 1 }),
    }
}

async fn propagate_failure(state: &ExecutionState, failed_idx: usize) {
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();
    visited.insert(failed_idx);

    for &dep_idx in &state.dependents[failed_idx] {
        if !visited.contains(&dep_idx) {
            queue.push_back(dep_idx);
        }
    }

    while let Some(dep_idx) = queue.pop_front() {
        if visited.contains(&dep_idx) {
            continue;
        }
        visited.insert(dep_idx);

        if !state.failed[dep_idx].swap(true, Ordering::AcqRel) {
            let in_deg = state.in_degree[dep_idx].load(Ordering::Acquire);
            if in_deg != usize::MAX {
                let key = state.operations[dep_idx].key.clone();
                let failed_dep_key = &state.operations[failed_idx].key;

                wxctl_core::log_error_cascade!(
                    &state.operation_id,
                    "execution",
                    wxctl_core::logging::error_codes::E004,
                    &key.kind,
                    &key.name,
                    &format!("Skipped: dependency '{}.{}' failed", failed_dep_key.kind, failed_dep_key.name),
                    &format!("Fix the upstream error for {} '{}' first", failed_dep_key.kind, failed_dep_key.name),
                    wxctl_core::logging::error_codes::E001
                );

                state.observer.on_task_skipped(&key, "dependency failed");
                send_result(state, TaskResult::Skipped { key }).await;
            }

            for &transitive_idx in &state.dependents[dep_idx] {
                if !visited.contains(&transitive_idx) {
                    queue.push_back(transitive_idx);
                }
            }
        }
    }
}

async fn collect_results(mut result_rx: mpsc::Receiver<TaskResult>, total: usize, state: Arc<ExecutionState>, semaphore: Arc<Semaphore>, join_set: &mut JoinSet<()>) -> Result<ExecutionResults> {
    let mut results = ExecutionResults::with_capacity(total);
    let mut processed = 0usize;

    while processed < total {
        match result_rx.recv().await {
            Some(TaskResult::Success { idx, result: exec_result }) => {
                results.succeeded.push(exec_result);
                processed += 1;

                for &dep_idx in &state.dependents[idx] {
                    if !state.failed[dep_idx].load(Ordering::Acquire) {
                        state.in_degree[dep_idx].fetch_sub(1, Ordering::AcqRel);
                    }
                }

                spawn_ready_tasks(&state, &semaphore, join_set, Some(idx));
            }
            Some(TaskResult::Failure { result: exec_result }) => {
                results.failed.push(exec_result);
                processed += 1;
            }
            Some(TaskResult::Skipped { key }) => {
                results.skipped.push(key);
                processed += 1;
            }
            None => {
                let mut unaccounted = Vec::new();
                let accounted: HashSet<ResourceKey> = results.succeeded.iter().map(|r| r.key.clone()).chain(results.failed.iter().map(|r| r.key.clone())).chain(results.skipped.iter().cloned()).collect();
                for idx in 0..state.operations.len() {
                    let key = &state.operations[idx].key;
                    if !accounted.contains(key) {
                        if state.failed[idx].load(Ordering::Acquire) {
                            results.skipped.push(key.clone());
                            processed += 1;
                        } else {
                            // Task was either in-progress (in_deg == MAX) or blocked
                            // Either way, channel closed unexpectedly
                            unaccounted.push(key.clone());
                        }
                    }
                }

                if !unaccounted.is_empty() {
                    return Err(anyhow!("Channel closed unexpectedly: processed {}/{}, {} unaccounted tasks (possible panic): {:?}", processed, total, unaccounted.len(), unaccounted.iter().take(5).collect::<Vec<_>>()));
                }
                break;
            }
        }
    }

    Ok(results)
}
