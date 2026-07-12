use super::config::ConcurrencyConfig;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};

pub struct CapacityManager {
    global: Arc<Semaphore>,
    per_service: HashMap<String, Arc<Semaphore>>,
}

pub struct ScopedPermit {
    _global: OwnedSemaphorePermit,
    _service: Option<OwnedSemaphorePermit>,
}

impl CapacityManager {
    pub fn new(config: &ConcurrencyConfig) -> Self {
        let global = Arc::new(Semaphore::new(config.global_limit));

        let mut per_service = HashMap::new();
        for (service, limit) in &config.service_limits {
            per_service.insert(service.clone(), Arc::new(Semaphore::new(*limit)));
        }

        Self { global, per_service }
    }

    pub async fn acquire<'a>(&'a self, service: &'a str) -> Result<ScopedPermit, AcquireError> {
        // Per-service permit first: a capped service queues on its own semaphore
        // without holding a global permit, so it cannot exhaust the global pool
        // and starve other services.
        let service_permit = if let Some(sem) = self.per_service.get(service) { Some(sem.clone().acquire_owned().await?) } else { None };

        let global_permit = self.global.clone().acquire_owned().await?;

        Ok(ScopedPermit { _global: global_permit, _service: service_permit })
    }

    pub fn available(&self) -> usize {
        self.global.available_permits()
    }
}
