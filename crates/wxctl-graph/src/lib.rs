//! Centralized graph algorithms for dependency management.
//!
//! This crate provides `IndexGraph<N>` - an index-based graph with built-in
//! algorithms for topological sorting and cycle detection. All algorithms
//! operate on indices rather than cloning nodes, minimizing allocations.

mod dependency;
mod index_graph;
mod references;
mod resource_set;
mod types;

pub use dependency::{DependencyEdge, extract_dependency_edges};
pub use index_graph::{CycleError, IndexGraph};
pub use references::{ParsedReference, extract_references, extract_references_with_path, parse_reference, parse_reference_with_path};
pub use resource_set::{Resource, ResourceSet, ResourceSetBuilder};
pub use types::{IStr, ResourceKey, istr};

/// Type alias for dependency graphs keyed by ResourceKey
pub type DependencyGraph = IndexGraph<ResourceKey>;
