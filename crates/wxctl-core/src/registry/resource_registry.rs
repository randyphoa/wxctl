use super::descriptor::ResourceDescriptor;
use crate::schema::ResourceSchema;
use crate::traits::{Reconciler, ResourceHandler};
use anyhow::Result;
use std::collections::HashMap;
use std::sync::Arc;

pub struct ResourceRegistry {
    descriptors: HashMap<String, Arc<ResourceDescriptor>>,
    reconcilers: HashMap<String, Arc<dyn Reconciler>>,
    handlers: HashMap<String, Arc<dyn ResourceHandler>>,
}

impl Default for ResourceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self { descriptors: HashMap::new(), reconcilers: HashMap::new(), handlers: HashMap::new() }
    }

    /// Register a resource type from a schema with a custom reconciler factory.
    ///
    /// The `reconciler_factory` function takes the schema and produces a reconciler.
    /// This allows the reconciler implementation to live in a different crate.
    pub fn register_from_schema<F>(&mut self, schema: ResourceSchema, handler: Option<Arc<dyn ResourceHandler>>, reconciler_factory: F) -> Result<()>
    where
        F: FnOnce(ResourceSchema) -> Arc<dyn Reconciler>,
    {
        let name = schema.resource.name.clone();

        schema.validate()?;

        let descriptor = Arc::new(ResourceDescriptor::from_schema(&schema)?);
        let reconciler = reconciler_factory(schema);

        self.descriptors.insert(name.clone(), descriptor);
        self.reconcilers.insert(name.clone(), reconciler);

        if let Some(handler) = handler {
            self.handlers.insert(name, handler);
        }

        Ok(())
    }

    pub fn get_descriptor(&self, name: &str) -> Option<&Arc<ResourceDescriptor>> {
        self.descriptors.get(name)
    }

    pub fn get_reconciler(&self, name: &str) -> Option<&Arc<dyn Reconciler>> {
        self.reconcilers.get(name)
    }

    pub fn get_handler(&self, name: &str) -> Option<&Arc<dyn ResourceHandler>> {
        self.handlers.get(name)
    }

    pub fn all_descriptors(&self) -> impl Iterator<Item = &Arc<ResourceDescriptor>> {
        self.descriptors.values()
    }

    pub fn get_service(&self, kind: &str) -> Result<String> {
        self.descriptors.values().find(|d| d.kind == kind).map(|d| d.service.clone()).ok_or_else(|| anyhow::anyhow!("No resource found with kind: {}", kind))
    }
}
