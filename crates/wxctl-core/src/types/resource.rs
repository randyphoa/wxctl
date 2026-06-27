use serde_json::Value;

// Re-export from wxctl-graph for backward compatibility (unchanged).
pub use wxctl_graph::{IStr, ResourceKey, istr};

// `RawResource` / `ValidatedResource` / `OnDestroyPolicy` now live in the wasm-safe
// `wxctl-schema` crate so the offline validator can share them. Re-exported here so
// every existing `wxctl_core::*` / `crate::types::*` import path resolves unchanged.
pub use wxctl_schema::resource::{OnDestroyPolicy, RawResource, ValidatedResource};

#[derive(Debug, Clone)]
pub struct RemoteResource {
    pub key: ResourceKey,
    pub data: Value,
    pub exists: bool,
}
