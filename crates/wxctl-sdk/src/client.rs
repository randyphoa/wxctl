use crate::error::Result;
use std::sync::Arc;
use wxctl_core::{ClientFactory, ConcurrencyConfig, Config, ResourceRegistry};
use wxctl_engine::{CompiledPlan, ExecutionObserver, ExecutionResults, Pipeline, SchemaBasedReconciler, ValidationResult};

pub struct WxctlClient {
    pipeline: Pipeline,
    concurrency_config: wxctl_core::ConcurrencyConfig,
}

impl WxctlClient {
    pub fn new(profile: &str, profile_path: Option<&str>) -> Result<Self> {
        let mut registry = ResourceRegistry::new();
        let schemas = wxctl_providers::load_all_schemas()?;

        for schema in schemas {
            let handler = wxctl_providers::get_handler(&schema.resource.name);
            registry.register_from_schema(schema, handler, |_| Arc::new(SchemaBasedReconciler::new()))?;
        }

        let concurrency_config = ConcurrencyConfig::from_env();
        let client_factory = ClientFactory::new(profile, profile_path, &concurrency_config)?;

        Ok(Self { pipeline: Pipeline::new(Arc::new(registry), Arc::new(client_factory)), concurrency_config })
    }

    pub async fn validate(&self, config: &mut Config) -> Result<ValidationResult> {
        self.validate_with(config, false).await
    }

    /// `validate` with explicit `post_validate` control. `skip_post_validate = true` runs the
    /// offline-equivalent checks only (no source-file-existence / handler enrichment), matching
    /// the CLI's `--skip-post-validate`. The MCP `wxctl_validate` tool's `skip_post_validate`
    /// input reaches this.
    pub async fn validate_with(&self, config: &mut Config, skip_post_validate: bool) -> Result<ValidationResult> {
        Ok(self.pipeline.validate_with(config, skip_post_validate).await?)
    }

    pub async fn plan(&self, config: &mut Config) -> Result<CompiledPlan> {
        Ok(self.pipeline.plan(config).await?)
    }

    pub async fn apply(&self, config: &mut Config) -> Result<ExecutionResults> {
        Ok(self.pipeline.apply(config).await?)
    }

    pub async fn destroy(&self, config: &mut Config) -> Result<ExecutionResults> {
        Ok(self.pipeline.destroy(config).await?)
    }

    pub async fn apply_with(&self, config: &mut Config, observer: Arc<dyn ExecutionObserver>, cancel: tokio_util::sync::CancellationToken) -> Result<ExecutionResults> {
        Ok(self.pipeline.apply_with(config, observer, cancel).await?)
    }

    pub async fn destroy_with(&self, config: &mut Config, observer: Arc<dyn ExecutionObserver>, cancel: tokio_util::sync::CancellationToken) -> Result<ExecutionResults> {
        Ok(self.pipeline.destroy_with(config, observer, cancel).await?)
    }

    pub async fn test(&self, config: &mut Config) -> Result<crate::testing::TestResults> {
        self.test_with_observer(config, std::sync::Arc::new(crate::testing::NoOpTestObserver)).await
    }

    pub async fn test_with_observer(&self, config: &mut Config, observer: std::sync::Arc<dyn crate::testing::TestObserver>) -> Result<crate::testing::TestResults> {
        let concurrency_limit = self.concurrency_config.global_limit;
        Ok(crate::testing::run_tests(config, &self.pipeline, concurrency_limit, observer).await?)
    }
}
