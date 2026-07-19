use crate::client::HttpClient;
use crate::types::{RemoteResource, ValidatedResource};
use anyhow::Result;
use std::future::Future;
use std::pin::Pin;

/// Sink for warn-level advisories raised during discovery. The reconciliation
/// pipeline passes a collector; `discover_all` pushes one entry per distinct
/// advisory. Object-safe + `Send + Sync` so it threads through the boxed, `Send`
/// discovery future. The typed `Advisory` shape is built by the pipeline's
/// collector from these four fields, so the shared `Advisory` type stays in
/// `wxctl-engine` and this crate carries no duplicate struct.
pub trait AdvisorySink: Send + Sync {
    /// Record one advisory: namespaced `code`, `"<kind>/<name>"` `resource`,
    /// human `message`, one-line `suggestion`.
    fn push(&self, code: &str, resource: &str, message: &str, suggestion: &str);
}

/// Discards every advisory. Used on discovery paths that do not surface advisories
/// (the single-result `discover` delegations).
pub struct NoOpAdvisorySink;

impl AdvisorySink for NoOpAdvisorySink {
    fn push(&self, _code: &str, _resource: &str, _message: &str, _suggestion: &str) {}
}

pub trait Reconciler: Send + Sync {
    fn discover<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<RemoteResource>> + Send + 'a>>;

    /// Discover all matching remote resources (for handling duplicates).
    /// Default implementation calls `discover()` and wraps the single result in a vec.
    /// `advisories` collects warn-level discovery advisories (R501); the default emits none.
    fn discover_all<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient, advisories: &'a dyn AdvisorySink) -> Pin<Box<dyn Future<Output = Result<Vec<RemoteResource>>> + Send + 'a>> {
        let _ = advisories;
        Box::pin(async move {
            let remote = self.discover(operation_id, resource, client).await?;
            if remote.exists { Ok(vec![remote]) } else { Ok(vec![]) }
        })
    }

    fn compare(&self, local: &ValidatedResource, remote: &RemoteResource) -> StateComparison;
}

#[derive(Debug, Clone)]
pub enum StateComparison {
    NoChange,
    Create,
    Update {
        fields: Vec<String>,
    },
    Delete,
    Recreate {
        /// Dotted path of the immutable field that differs.
        field: String,
        /// Local value rendered as a display string (`null` when absent).
        local_value: String,
        /// Remote value rendered as a display string (`null` when absent).
        remote_value: String,
    },
}
