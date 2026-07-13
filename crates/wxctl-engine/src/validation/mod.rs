pub mod advisories;
pub mod pipeline;
pub mod types;

pub use pipeline::ValidationPipeline;

// The schema/dependency/cross_resource validators + their types now live in the
// wasm-safe `wxctl-schema` crate (single source shared with the remote MCP server).
// Re-export them so `super::schema::*` / `super::dependency::*` / `super::cross_resource::*`
// inside `pipeline.rs` continue to resolve.
pub use wxctl_schema::validation::{cross_resource, dependency, schema};
