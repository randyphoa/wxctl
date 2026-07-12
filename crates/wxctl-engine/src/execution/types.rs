use crate::planning::types::CompiledPlan;
use crate::reconciliation::types::OperationType;
use serde_json::Value;
use std::time::Duration;
use wxctl_core::ResourceKey;

#[derive(Debug, Clone)]
pub struct ExecutionResult {
    pub key: ResourceKey,
    pub operation: OperationType,
    pub success: bool,
    pub error: Option<String>,
    pub response: Option<Value>,
    pub attempts: u32,
}

#[derive(Debug, Default)]
pub struct ExecutionResults {
    pub succeeded: Vec<ExecutionResult>,
    pub failed: Vec<ExecutionResult>,
    pub skipped: Vec<ResourceKey>,
    pub cancelled: bool,
}

impl ExecutionResults {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn with_capacity(n: usize) -> Self {
        Self { succeeded: Vec::with_capacity(n), failed: Vec::new(), skipped: Vec::new(), cancelled: false }
    }

    pub fn dry_run(plan: CompiledPlan) -> Self {
        let succeeded = plan.operations.into_iter().map(|op| ExecutionResult { key: op.key, operation: op.op_type, success: true, error: None, response: None, attempts: 0 }).collect();

        Self { succeeded, failed: Vec::new(), skipped: Vec::new(), cancelled: false }
    }

    pub fn has_failures(&self) -> bool {
        !self.failed.is_empty()
    }

    pub fn total_processed(&self) -> usize {
        self.succeeded.len() + self.failed.len() + self.skipped.len()
    }

    /// Whether `key` already landed in any outcome bucket (succeeded, failed, or skipped).
    pub fn contains_key(&self, key: &ResourceKey) -> bool {
        self.succeeded.iter().any(|r| &r.key == key) || self.failed.iter().any(|r| &r.key == key) || self.skipped.contains(key)
    }
}

/// Configuration for the DAG executor
#[derive(Debug, Clone)]
pub struct ExecutorConfig {
    /// Maximum concurrent operations across all services
    pub parallelism: usize,
    /// Timeout for each operation (includes all HttpClient retry attempts)
    pub operation_timeout: Duration,
    /// Optional total execution timeout for entire DAG
    pub total_timeout: Option<Duration>,
}

impl Default for ExecutorConfig {
    fn default() -> Self {
        Self { parallelism: 10, operation_timeout: Duration::from_secs(900), total_timeout: None }
    }
}

impl ExecutorConfig {
    pub fn new(parallelism: usize, timeout_secs: u64) -> Self {
        Self { parallelism, operation_timeout: Duration::from_secs(timeout_secs), ..Default::default() }
    }

    pub fn with_total_timeout(mut self, secs: u64) -> Self {
        self.total_timeout = Some(Duration::from_secs(secs));
        self
    }
}
