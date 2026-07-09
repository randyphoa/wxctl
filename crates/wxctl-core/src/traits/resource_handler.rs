use crate::client::HttpClient;
use crate::registry::FieldDescriptor;
use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;

/// Outcome of a lifecycle hook execution
#[derive(Debug)]
pub enum HookOutcome {
    /// Hook completed, continue with default operation
    Continue,
    /// Hook handled the operation, response provided, skip default operation
    Handled(Value),
}

pub trait ResourceHandler: Send + Sync {
    /// Called before resource creation
    /// - resource: mutable payload to modify
    /// - fields: field descriptors for filtering (computed, local_only)
    /// - client: HTTP client for custom operations
    /// - endpoint: resolved create endpoint
    /// - operation_id: tracing correlation ID
    fn pre_create<'a>(&'a self, _resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async { Ok(HookOutcome::Continue) })
    }

    /// Called after successful resource creation
    fn post_create<'a>(&'a self, _resource: &'a Value, _response: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Called when the default create POST returns an error. Gives handlers
    /// a chance to recover idempotently when the failure is an
    /// "already-exists" conflict that reconciliation couldn't detect ahead
    /// of time. Returning `Some(existing)` converts the error into a
    /// successful create with `existing` as the response; `None` propagates
    /// the original error.
    fn recover_from_create_error<'a>(&'a self, _resource: &'a Value, _error: &'a anyhow::Error, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async { Ok(None) })
    }

    /// Called before resource update
    /// - current: current remote state
    /// - desired: mutable payload to modify
    /// - fields: field descriptors for filtering (computed, local_only)
    /// - client: HTTP client for custom operations
    /// - endpoint: resolved update endpoint
    /// - operation_id: tracing correlation ID
    fn pre_update<'a>(&'a self, _current: &'a Value, _desired: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async { Ok(HookOutcome::Continue) })
    }

    /// Called after successful resource update
    fn post_update<'a>(&'a self, _resource: &'a Value, _response: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Called before resource deletion
    fn pre_delete<'a>(&'a self, _resource: &'a Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async { Ok(HookOutcome::Continue) })
    }

    /// Called after successful resource deletion
    fn post_delete<'a>(&'a self, _resource: &'a Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Called after remote resource discovery during reconciliation.
    /// Allows handlers to enrich remote data before it's stored in the cache
    /// (e.g., convert tool UUID arrays to name-keyed maps for template resolution).
    /// `is_apply` is `true` when invoked from `wxctl apply`, `false` from
    /// `wxctl plan` — handlers can use this to gate side-effecting work
    /// (e.g., blocking on an in-flight job) that shouldn't run during planning.
    fn post_discover<'a>(&'a self, _remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }

    /// Called after validation, before reconciliation
    /// Allows handlers to enrich resource data for state comparison
    /// (e.g., compute source hashes for tools)
    /// - resource: mutable resource data to enrich
    /// - operation_id: tracing correlation ID
    fn post_validate<'a>(&'a self, _resource: &'a mut Value, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async { Ok(()) })
    }
}
