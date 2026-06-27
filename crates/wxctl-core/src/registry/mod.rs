pub mod filters;
pub mod resource_registry;

/// Re-export of the descriptor model, now owned by the wasm-safe `wxctl-schema` crate.
/// `pub use ...descriptor` keeps the `crate::registry::descriptor::*` module path
/// (used by `client/factory.rs`); the flat names keep `crate::registry::FieldDescriptor`
/// (used by `registry/filters.rs`) and the top-level `wxctl_core::ResourceDescriptor`.
pub use resource_registry::ResourceRegistry;
pub use wxctl_schema::descriptor;
pub use wxctl_schema::descriptor::{Endpoints, FieldDescriptor, ResourceDescriptor};
