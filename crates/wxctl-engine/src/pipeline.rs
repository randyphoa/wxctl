use anyhow::{Result, bail};
use std::collections::HashSet;
use std::sync::Arc;
use uuid::Uuid;
use wxctl_core::{ClientFactory, Config, DependencyEdge, IndexGraph, ResourceKey, ResourceRegistry, ValidatedResource};

use crate::context::RuntimeIdStore;
use crate::execution::{DagExecutor, ExecutionObserver, ExecutionResults, ExecutorConfig, NoOpObserver};
use crate::planning::types::CompiledPlan;
use crate::reconciliation::types::ReconciliationPlan;
use crate::reconciliation::{ReconcileMode, ReconciliationPipeline};
use crate::validation::ValidationPipeline;
use crate::validation::types::ValidationResult;
use tokio_util::sync::CancellationToken;

pub struct Pipeline {
    registry: Arc<ResourceRegistry>,
    client_factory: Arc<ClientFactory>,
    config: EngineConfig,
}

#[derive(Default)]
pub struct EngineConfig {
    pub dry_run: bool,
    pub executor: ExecutorConfig,
}

impl Pipeline {
    pub fn new(registry: Arc<ResourceRegistry>, client_factory: Arc<ClientFactory>) -> Self {
        Self { registry, client_factory, config: EngineConfig::default() }
    }

    pub fn client_factory(&self) -> &Arc<ClientFactory> {
        &self.client_factory
    }

    /// Expand spec-based tool resources (e.g., OpenAPI) into individual per-endpoint tools.
    fn expand_resources(config: &mut Config) -> Result<()> {
        config.resources = wxctl_providers::expand_openapi_resources(std::mem::take(&mut config.resources))?;
        Ok(())
    }

    async fn run_validation(&self, operation_id: &str, config: &mut Config) -> Result<(Vec<ValidatedResource>, IndexGraph<ResourceKey>, Vec<DependencyEdge>)> {
        Self::expand_resources(config)?;
        let validator = ValidationPipeline::new(self.registry.clone(), Some(self.client_factory.clone()));
        let validated = validator.validate(operation_id, &mut config.resources, false).await?;
        if !validated.is_valid() {
            let errors: Vec<String> = validated.errors().iter().map(|e| e.to_string()).collect();
            bail!("Validation failed:\n  {}", errors.join("\n  "));
        }
        validated.into_parts().ok_or_else(|| anyhow::anyhow!("Validation passed but resource_set is None"))
    }

    fn validate_services(&self, resources: &[ValidatedResource]) -> Result<()> {
        self.client_factory.validate_services(resources.iter().map(|r| (r.descriptor.service.as_str(), r.key.kind.as_ref())))
    }

    fn sort_topologically(resources: Vec<ValidatedResource>, graph: &IndexGraph<ResourceKey>) -> Result<Vec<ValidatedResource>> {
        let topo_indices = graph.topological_sort_indices().map_err(|_| anyhow::anyhow!("Unexpected cycle in dependency graph"))?;
        let mut indexed: Vec<_> = resources.into_iter().enumerate().collect();
        let mut order_map: Vec<usize> = vec![0; indexed.len()];
        for (order, &idx) in topo_indices.iter().enumerate() {
            order_map[idx] = order;
        }
        indexed.sort_by_key(|(i, _)| order_map[*i]);
        Ok(indexed.into_iter().map(|(_, r)| r).collect())
    }

    fn check_reconciliation_errors(plan: &ReconciliationPlan) -> Result<()> {
        if !plan.errors.is_empty() {
            let count = plan.errors.len();
            let details: Vec<String> = plan.errors.iter().map(|e| format!("{}/{}: {}", e.kind, e.name, e.error)).collect();
            anyhow::bail!("Reconciliation failed for {} resource{}:\n  {}", count, if count == 1 { "" } else { "s" }, details.join("\n  "));
        }
        Ok(())
    }

    async fn run_reconciliation_raw(&self, operation_id: &str, resources: Vec<ValidatedResource>, is_apply: bool, observer: Arc<dyn ExecutionObserver>) -> Result<ReconciliationPlan> {
        let reconciler = ReconciliationPipeline::with_observer(self.registry.clone(), self.client_factory.clone(), observer);
        let runtime_store = RuntimeIdStore::new();
        reconciler.reconcile(operation_id, resources, &runtime_store, ReconcileMode::Apply, is_apply).await
    }

    fn compile_plan(&self, operation_id: &str, reconciliation_plan: ReconciliationPlan) -> CompiledPlan {
        let span = tracing::info_span!(target: "wxctl::stage::planning", "planning", operation_id = %operation_id, resource_count = reconciliation_plan.operations.len());
        let _enter = span.enter();
        let plan = CompiledPlan { operations: reconciliation_plan.operations, advisories: reconciliation_plan.advisories };
        tracing::debug!(target: "wxctl::substage::planning", operation_id = %operation_id, "execution plan compiled");
        plan
    }

    pub async fn validate(&self, config: &mut Config) -> Result<ValidationResult> {
        self.validate_with(config, false).await
    }

    /// `validate`, but with explicit control over the `post_validate` hook. `skip_post_validate
    /// = true` runs schema/dependency/cross-resource checks only — the config-tier (pre-scaffold)
    /// validation the compose flow needs when source files do not yet exist. The CLI's
    /// `--skip-post-validate` and the MCP `wxctl_validate` `skip_post_validate` input both reach this.
    pub async fn validate_with(&self, config: &mut Config, skip_post_validate: bool) -> Result<ValidationResult> {
        let operation_id = Uuid::new_v4().to_string();
        Self::expand_resources(config)?;
        let validator = ValidationPipeline::new(self.registry.clone(), Some(self.client_factory.clone()));
        validator.validate(&operation_id, &mut config.resources, skip_post_validate).await
    }

    pub async fn plan(&self, config: &mut Config) -> Result<CompiledPlan> {
        let (_, plan, _) = self.plan_for_apply(config, false).await?;
        Ok(plan)
    }

    /// `plan` with a caller-supplied reconcile-aware observer, for the CLI `plan`
    /// command's live reconciliation counter. SDK/MCP keep using `plan` (no observer).
    pub async fn plan_with(&self, config: &mut Config, observer: Arc<dyn ExecutionObserver>) -> Result<CompiledPlan> {
        let (_, plan, _) = self.plan_for_apply_with(config, false, observer).await?;
        Ok(plan)
    }

    pub async fn apply(&self, config: &mut Config) -> Result<ExecutionResults> {
        let (operation_id, plan, graph) = self.plan_for_apply(config, true).await?;

        if self.config.dry_run {
            return Ok(ExecutionResults::dry_run(plan));
        }

        let executor = DagExecutor::new(self.registry.clone(), self.client_factory.clone(), self.config.executor.clone());
        executor.execute(&operation_id, plan.operations, &graph).await
    }

    pub async fn destroy(&self, config: &mut Config) -> Result<ExecutionResults> {
        let (operation_id, plan, graph, seed) = self.plan_for_destroy(config).await?;

        if self.config.dry_run {
            return Ok(ExecutionResults::dry_run(plan));
        }

        let executor = DagExecutor::new(self.registry.clone(), self.client_factory.clone(), self.config.executor.clone());
        executor.execute_destroy_seeded(&operation_id, plan.operations, &graph, seed).await
    }

    /// Apply with an injected observer + cancellation token, for callers (SDK/MCP) that
    /// stream progress and honor cancellation. Mirrors `apply` but builds the executor
    /// with `with_observer` + `execute_with_cancel` (the same seam the CLI uses inline).
    pub async fn apply_with(&self, config: &mut Config, observer: Arc<dyn ExecutionObserver>, cancel: CancellationToken) -> Result<ExecutionResults> {
        let (operation_id, plan, graph) = self.plan_for_apply(config, true).await?;
        if self.config.dry_run {
            return Ok(ExecutionResults::dry_run(plan));
        }
        let executor = DagExecutor::with_observer(self.registry.clone(), self.client_factory.clone(), self.config.executor.clone(), observer);
        executor.execute_with_cancel(&operation_id, plan.operations, &graph, cancel, false, RuntimeIdStore::new()).await
    }

    /// Destroy with an injected observer + cancellation token. Mirrors `destroy`, seeding
    /// the executor's runtime store from reconciliation so reverse-topo deletes resolve
    /// `__ref__*` enrichment (same as `destroy`'s `execute_destroy_seeded` path).
    pub async fn destroy_with(&self, config: &mut Config, observer: Arc<dyn ExecutionObserver>, cancel: CancellationToken) -> Result<ExecutionResults> {
        let (operation_id, plan, filtered_graph, seed) = self.plan_for_destroy(config).await?;
        if self.config.dry_run {
            return Ok(ExecutionResults::dry_run(plan));
        }
        let executor = DagExecutor::with_observer(self.registry.clone(), self.client_factory.clone(), self.config.executor.clone(), observer);
        executor.execute_with_cancel(&operation_id, plan.operations, &filtered_graph, cancel, true, seed).await
    }

    /// Run all correctness checks and return the operation_id, plan, and graph for external execution.
    /// Used by CLI to inject observers and control execution display.
    /// `is_apply` is `true` from `wxctl apply`, `false` from `wxctl plan` — handlers'
    /// `post_discover` use it to gate apply-only blocking work.
    pub async fn plan_for_apply(&self, config: &mut Config, is_apply: bool) -> Result<(String, CompiledPlan, IndexGraph<ResourceKey>)> {
        self.plan_for_apply_with(config, is_apply, Arc::new(NoOpObserver)).await
    }

    /// `plan_for_apply` with a caller-supplied reconcile-aware observer. The CLI passes
    /// the same `CliProgressObserver` it later hands to `DagExecutor::with_observer`, so
    /// the reconciliation stage and execution stage share one observer. SDK/MCP keep
    /// using `plan_for_apply` (default `NoOpObserver`), so they are unaffected.
    pub async fn plan_for_apply_with(&self, config: &mut Config, is_apply: bool, observer: Arc<dyn ExecutionObserver>) -> Result<(String, CompiledPlan, IndexGraph<ResourceKey>)> {
        let operation_id = Uuid::new_v4().to_string();
        let (resources, graph, edges) = self.run_validation(&operation_id, config).await?;
        // Real apply only: `is_apply` is false for `wxctl plan`, and `dry_run` short-circuits
        // execution — neither exercises the edges, so their records stay free of graph evidence.
        if is_apply && !self.config.dry_run {
            emit_graph_event(&operation_id, &edges);
        }
        self.validate_services(&resources)?;
        let resources = Self::sort_topologically(resources, &graph)?;
        let reconciliation_plan = self.run_reconciliation_raw(&operation_id, resources, is_apply, observer).await?;
        Self::check_reconciliation_errors(&reconciliation_plan)?;
        let plan = self.compile_plan(&operation_id, reconciliation_plan);
        Ok((operation_id, plan, graph))
    }

    /// Run all correctness checks for destroy and return the operation_id, plan, and filtered graph.
    pub async fn plan_for_destroy(&self, config: &mut Config) -> Result<(String, CompiledPlan, IndexGraph<ResourceKey>, RuntimeIdStore)> {
        self.plan_for_destroy_with(config, Arc::new(NoOpObserver)).await
    }

    /// `plan_for_destroy` with a caller-supplied reconcile-aware observer (CLI). The same
    /// observer is later handed to `DagExecutor::with_observer` for the execution stage.
    pub async fn plan_for_destroy_with(&self, config: &mut Config, observer: Arc<dyn ExecutionObserver>) -> Result<(String, CompiledPlan, IndexGraph<ResourceKey>, RuntimeIdStore)> {
        let operation_id = Uuid::new_v4().to_string();
        let (resources, graph, edges) = self.run_validation(&operation_id, config).await?;
        // Destroy always executes when reached (dry-run short-circuits earlier); guard it anyway.
        if !self.config.dry_run {
            emit_graph_event(&operation_id, &edges);
        }
        self.validate_services(&resources)?;
        // Discovery needs the forward DAG order so each resource's templates
        // resolve against the store populated by already-discovered parents;
        // execution still runs deletes leaves-first via DagExecutor::execute_destroy_seeded.
        let resources = Self::sort_topologically(resources, &graph)?;

        let reconciler = ReconciliationPipeline::with_observer(self.registry.clone(), self.client_factory.clone(), observer);
        let runtime_store = RuntimeIdStore::new();
        let reconciliation_plan = reconciler.reconcile(&operation_id, resources, &runtime_store, ReconcileMode::Destroy, false).await?;
        Self::check_reconciliation_errors(&reconciliation_plan)?;

        let operation_keys: HashSet<_> = reconciliation_plan.operations.iter().map(|op| op.key.clone()).collect();
        let filtered_graph = graph.filter(|key| operation_keys.contains(key));

        let plan = self.compile_plan(&operation_id, reconciliation_plan);
        // Reverse-topological delete tasks need parent data for `__ref__*`
        // enrichment before their parents themselves have been deleted; seed
        // the executor's store with the reconciliation cache so lookups hit.
        Ok((operation_id, plan, filtered_graph, runtime_store))
    }
}

/// Record the executed dependency edges into the run record for the knowledge
/// plane's run-evidence ingester (`wxctl-knowledge`). One INFO event, target
/// `wxctl::graph`; edges carry kinds, ref names, and field paths only — no
/// resource values or bodies — so the event is safe in concise (non-full-trace)
/// records and needs no redaction. Captured by the existing `RunRecordLayer`
/// (INFO passes its concise filter; target `wxctl::graph` matches the run-record
/// `wxctl=trace` EnvFilter) with no layer changes.
fn emit_graph_event(operation_id: &str, edges: &[DependencyEdge]) {
    let arr: Vec<serde_json::Value> = edges
        .iter()
        .map(|e| {
            serde_json::json!({
                "from_kind": e.from.kind.as_ref(),
                "from_name": e.from.name.as_ref(),
                "to_kind": e.to.kind.as_ref(),
                "to_name": e.to.name.as_ref(),
                "field": e.field_path.as_ref(),
            })
        })
        .collect();
    let edges_json = serde_json::Value::Array(arr).to_string();
    tracing::info!(target: "wxctl::graph", operation_id = %operation_id, edge_count = edges.len(), edges = %edges_json, "executed dependency edges");
}
