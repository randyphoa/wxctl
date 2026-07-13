use crate::client::HttpClient;
use crate::types::{RemoteResource, ValidatedResource};
use anyhow::Result;
use std::future::Future;
use std::pin::Pin;

pub trait Reconciler: Send + Sync {
    fn discover<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<RemoteResource>> + Send + 'a>>;

    /// Discover all matching remote resources (for handling duplicates)
    /// Default implementation calls discover() and wraps single result in a vec
    fn discover_all<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<Vec<RemoteResource>>> + Send + 'a>> {
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
