//! Resource collection with embedded dependency graph.

use crate::dependency::{DependencyEdge, extract_dependency_edges};
use crate::index_graph::{CycleError, IndexGraph};
use crate::types::ResourceKey;

/// Trait for resources that can be stored in a ResourceSet.
pub trait Resource {
    /// Get the resource key (kind + name).
    fn key(&self) -> &ResourceKey;

    /// Get the resource data as JSON for dependency extraction.
    fn data(&self) -> &serde_json::Value;

    /// Get pre-extracted dependencies (if available).
    /// Returns an empty slice by default.
    fn dependencies(&self) -> &[ResourceKey] {
        &[]
    }
}

/// Resource collection with dependency graph.
///
/// `ResourceSet` owns resources and wraps an `IndexGraph<ResourceKey>`
/// for efficient graph operations.
///
/// # Construction
///
/// Use `ResourceSetBuilder` for flexible construction,
/// or use convenience constructors:
/// - `ResourceSet::new()` - Extract dependencies from JSON
/// - `ResourceSet::from_graph()` - Use existing graph (avoids rebuilding)
#[derive(Debug)]
pub struct ResourceSet<R: Resource> {
    /// Resources stored in insertion order.
    resources: Vec<R>,
    /// Graph for topology operations. Node indices align with resource indices.
    graph: IndexGraph<ResourceKey>,
    /// All dependency edges with field paths (for error messages/visualization).
    edges: Vec<DependencyEdge>,
}

impl<R: Resource> ResourceSet<R> {
    /// Build a ResourceSet by extracting dependencies from JSON data.
    ///
    /// Extracts dependencies from each resource's data using `${kind.name}`
    /// references, builds the dependency graph, and verifies no circular
    /// dependencies exist.
    ///
    /// Returns an error if a circular dependency is detected.
    pub fn new(resources: Vec<R>) -> Result<Self, CycleError<ResourceKey>> {
        ResourceSetBuilder::new(resources).build()
    }

    /// Build a ResourceSet from an existing graph (avoids rebuilding).
    ///
    /// This is the most efficient constructor when the graph has already been
    /// built and validated (e.g., in the validation pipeline).
    ///
    /// # Arguments
    /// * `resources` - Resources (order should match graph node order)
    /// * `graph` - Pre-built dependency graph
    /// * `edges` - Dependency edges with field path information
    ///
    /// # Panics (debug only)
    /// Panics if resources contain cycles (indicates caller bug).
    pub fn from_graph(resources: Vec<R>, graph: IndexGraph<ResourceKey>, edges: Vec<DependencyEdge>) -> Self {
        #[cfg(debug_assertions)]
        {
            debug_assert!(!graph.has_cycle(), "from_graph called with cyclic graph - caller bug");
        }

        Self { resources, graph, edges }
    }

    /// Build a ResourceSet using pre-extracted dependencies.
    ///
    /// Uses `Resource::dependencies()` instead of re-parsing JSON.
    /// This is more efficient when validation already extracted dependencies.
    ///
    /// Returns an error if a circular dependency is detected.
    pub fn from_validated(resources: Vec<R>) -> Result<Self, CycleError<ResourceKey>> {
        ResourceSetBuilder::new(resources).use_preextracted_deps().build()
    }

    /// Build a ResourceSet from validation with preserved dependency edges.
    ///
    /// This is the preferred constructor when edges with field paths are needed
    /// for error messages or visualization.
    ///
    /// # Arguments
    /// * `resources` - Topologically sorted resources
    /// * `edges` - Dependency edges with field path information
    ///
    /// # Panics (debug only)
    /// Panics if resources contain cycles (indicates validation bug).
    pub fn from_validation(resources: Vec<R>, edges: Vec<DependencyEdge>) -> Self {
        ResourceSetBuilder::new(resources).with_edges(edges).use_preextracted_deps().skip_cycle_check().build().expect("from_validation should not fail with skip_cycle_check")
    }

    /// Build a ResourceSet from already topologically sorted resources.
    ///
    /// Skips cycle detection since the caller guarantees resources are sorted.
    /// Use this when validation pipeline already performed topological sort.
    ///
    /// # Panics (debug only)
    /// Panics if resources contain cycles (indicates caller bug).
    pub fn from_sorted(resources: Vec<R>) -> Self {
        ResourceSetBuilder::new(resources).use_preextracted_deps().skip_cycle_check().build().expect("from_sorted should not fail with skip_cycle_check")
    }

    /// Get a resource by index. O(1).
    #[must_use]
    #[inline]
    pub fn get(&self, idx: usize) -> &R {
        &self.resources[idx]
    }

    /// Get a mutable reference to a resource by index. O(1).
    #[inline]
    pub fn get_mut(&mut self, idx: usize) -> &mut R {
        &mut self.resources[idx]
    }

    /// Get a resource by key. O(1).
    #[must_use]
    #[inline]
    pub fn get_by_key(&self, key: &ResourceKey) -> Option<&R> {
        self.graph.get_index(key).map(|i| &self.resources[i])
    }

    /// Get the index of a resource by key. O(1).
    #[must_use]
    #[inline]
    pub fn index_of(&self, key: &ResourceKey) -> Option<usize> {
        self.graph.get_index(key)
    }

    /// Check if a key exists. O(1).
    #[must_use]
    #[inline]
    pub fn contains(&self, key: &ResourceKey) -> bool {
        self.graph.contains(key)
    }

    /// Get dependencies of a resource by index. O(1).
    #[inline]
    pub fn dependencies(&self, idx: usize) -> impl Iterator<Item = usize> + '_ {
        self.graph.dependency_indices(idx)
    }

    /// Get all dependency edges with field paths.
    #[must_use]
    #[inline]
    pub fn edges(&self) -> &[DependencyEdge] {
        &self.edges
    }

    /// Returns the number of resources.
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.resources.len()
    }

    /// Returns true if empty.
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.resources.is_empty()
    }

    /// Compute topological order (single list of indices).
    #[must_use]
    #[inline]
    pub fn topological_order(&self) -> Vec<usize> {
        self.graph.topological_sort_indices().expect("no cycles after construction")
    }

    /// Iterate over all resources.
    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = &R> {
        self.resources.iter()
    }

    /// Get the underlying slice of resources.
    #[must_use]
    #[inline]
    pub fn as_slice(&self) -> &[R] {
        &self.resources
    }

    /// Get a reference to the underlying graph.
    #[must_use]
    #[inline]
    pub fn graph(&self) -> &IndexGraph<ResourceKey> {
        &self.graph
    }

    /// Consume and return all parts.
    pub fn into_parts(self) -> (Vec<R>, IndexGraph<ResourceKey>, Vec<DependencyEdge>) {
        (self.resources, self.graph, self.edges)
    }
}

// ============================================================================
// IntoIterator implementations for ResourceSet
// ============================================================================

impl<R: Resource> IntoIterator for ResourceSet<R> {
    type Item = R;
    type IntoIter = std::vec::IntoIter<R>;

    fn into_iter(self) -> Self::IntoIter {
        self.resources.into_iter()
    }
}

impl<'a, R: Resource> IntoIterator for &'a ResourceSet<R> {
    type Item = &'a R;
    type IntoIter = std::slice::Iter<'a, R>;

    fn into_iter(self) -> Self::IntoIter {
        self.resources.iter()
    }
}

impl<'a, R: Resource> IntoIterator for &'a mut ResourceSet<R> {
    type Item = &'a mut R;
    type IntoIter = std::slice::IterMut<'a, R>;

    fn into_iter(self) -> Self::IntoIter {
        self.resources.iter_mut()
    }
}

// ============================================================================
// ResourceSetBuilder: Fluent API for ResourceSet construction
// ============================================================================

/// Builder for `ResourceSet` with flexible construction options.
pub struct ResourceSetBuilder<R: Resource> {
    resources: Vec<R>,
    edges: Vec<DependencyEdge>,
    graph: Option<IndexGraph<ResourceKey>>,
    use_preextracted_deps: bool,
    skip_cycle_check: bool,
}

impl<R: Resource> ResourceSetBuilder<R> {
    /// Create a new builder with the given resources.
    pub fn new(resources: Vec<R>) -> Self {
        Self { resources, edges: Vec::new(), graph: None, use_preextracted_deps: false, skip_cycle_check: false }
    }

    /// Use pre-extracted dependency edges (for error messages/visualization).
    pub fn with_edges(mut self, edges: Vec<DependencyEdge>) -> Self {
        self.edges = edges;
        self
    }

    /// Use an existing graph instead of building one.
    ///
    /// When provided, skips graph building entirely.
    pub fn with_graph(mut self, graph: IndexGraph<ResourceKey>) -> Self {
        self.graph = Some(graph);
        self
    }

    /// Use `Resource::dependencies()` instead of extracting from JSON.
    ///
    /// More efficient when validation already extracted dependencies.
    pub fn use_preextracted_deps(mut self) -> Self {
        self.use_preextracted_deps = true;
        self
    }

    /// Skip cycle detection (caller guarantees no cycles).
    ///
    /// Use when resources are known to be valid (e.g., from validation pipeline).
    /// In debug builds, still asserts no cycles exist.
    pub fn skip_cycle_check(mut self) -> Self {
        self.skip_cycle_check = true;
        self
    }

    /// Build the `ResourceSet`.
    ///
    /// Returns an error if a circular dependency is detected (unless `skip_cycle_check`).
    pub fn build(self) -> Result<ResourceSet<R>, CycleError<ResourceKey>> {
        let graph = if let Some(g) = self.graph {
            // Use provided graph
            g
        } else {
            // Build graph
            let mut graph = IndexGraph::with_capacity(self.resources.len());

            // Add all nodes first
            for resource in &self.resources {
                graph.add_node(resource.key().clone());
            }

            // Add edges
            if self.use_preextracted_deps {
                // Use pre-extracted dependencies from Resource trait
                for resource in &self.resources {
                    for dep_key in resource.dependencies() {
                        if graph.contains(dep_key) {
                            graph.add_edge(resource.key().clone(), dep_key.clone());
                        }
                    }
                }
            } else {
                // Extract from JSON
                for resource in &self.resources {
                    let resource_edges = extract_dependency_edges(resource.key(), resource.data());
                    for edge in resource_edges {
                        if graph.contains(&edge.to) {
                            graph.add_edge(resource.key().clone(), edge.to.clone());
                        }
                    }
                }
            }

            graph
        };

        // Cycle check
        if self.skip_cycle_check {
            #[cfg(debug_assertions)]
            {
                debug_assert!(!graph.has_cycle(), "ResourceSetBuilder: skip_cycle_check used but graph has cycles");
            }
        } else if let Some(cycle) = graph.find_cycle() {
            return Err(CycleError { cycle });
        }

        Ok(ResourceSet { resources: self.resources, graph, edges: self.edges })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ResourceKey;

    /// Minimal test resource implementing `Resource`.
    #[derive(Debug, Clone)]
    struct TestResource {
        key: ResourceKey,
        data: serde_json::Value,
        deps: Vec<ResourceKey>,
    }

    impl TestResource {
        fn new(kind: &str, name: &str, data: serde_json::Value) -> Self {
            Self { key: ResourceKey::new(kind, name), data, deps: Vec::new() }
        }

        fn with_deps(kind: &str, name: &str, data: serde_json::Value, deps: Vec<ResourceKey>) -> Self {
            Self { key: ResourceKey::new(kind, name), data, deps }
        }
    }

    impl Resource for TestResource {
        fn key(&self) -> &ResourceKey {
            &self.key
        }

        fn data(&self) -> &serde_json::Value {
            &self.data
        }

        fn dependencies(&self) -> &[ResourceKey] {
            &self.deps
        }
    }

    #[test]
    fn test_constructors_order_dependency_before_dependent() {
        // Both constructors must place catalog.c1 before asset.a1; they differ only in
        // where the dependency comes from: `new` extracts it from the JSON `${...}` ref,
        // `from_validated` trusts the pre-extracted `dependencies()` (JSON has no ref).
        let new_set = || {
            let r1 = TestResource::new("catalog", "c1", serde_json::json!({}));
            let r2 = TestResource::new("asset", "a1", serde_json::json!({ "catalog_id": "${catalog.c1}" }));
            ResourceSet::new(vec![r1, r2]).unwrap()
        };
        let from_validated_set = || {
            let r1 = TestResource::with_deps("catalog", "c1", serde_json::json!({}), vec![]);
            // No refs in JSON — deps come from the trait.
            let r2 = TestResource::with_deps("asset", "a1", serde_json::json!({}), vec![ResourceKey::new("catalog", "c1")]);
            ResourceSet::from_validated(vec![r1, r2]).unwrap()
        };
        for (label, set) in [("new (JSON-extracted deps)", new_set()), ("from_validated (pre-extracted deps)", from_validated_set())] {
            assert_eq!(set.len(), 2, "len for {label}");
            let order = set.topological_order();
            let idx_c1 = set.index_of(&ResourceKey::new("catalog", "c1")).unwrap();
            let idx_a1 = set.index_of(&ResourceKey::new("asset", "a1")).unwrap();
            let pos_c1 = order.iter().position(|&i| i == idx_c1).unwrap();
            let pos_a1 = order.iter().position(|&i| i == idx_a1).unwrap();
            assert!(pos_c1 < pos_a1, "catalog.c1 must come before asset.a1 for {label}");
        }
    }

    #[test]
    fn test_cyclic_resources_error() {
        let r1 = TestResource::new(
            "catalog",
            "a",
            serde_json::json!({
                "ref": "${catalog.b}"
            }),
        );
        let r2 = TestResource::new(
            "catalog",
            "b",
            serde_json::json!({
                "ref": "${catalog.a}"
            }),
        );
        assert!(ResourceSet::new(vec![r1, r2]).is_err());
    }

    #[test]
    fn test_topological_order_valid() {
        // Diamond: a1→c1, a1→conn1, c1→base, conn1→base
        let base = TestResource::new("catalog", "base", serde_json::json!({}));
        let c1 = TestResource::new(
            "catalog",
            "c1",
            serde_json::json!({
                "parent": "${catalog.base}"
            }),
        );
        let conn1 = TestResource::new(
            "connection",
            "conn1",
            serde_json::json!({
                "catalog": "${catalog.base}"
            }),
        );
        let a1 = TestResource::new(
            "asset",
            "a1",
            serde_json::json!({
                "catalog_id": "${catalog.c1}",
                "connection_id": "${connection.conn1}"
            }),
        );

        let set = ResourceSet::new(vec![base, c1, conn1, a1]).unwrap();
        let order = set.topological_order();

        // For every edge, dependency must come before dependent
        let graph = set.graph();
        for (idx, _) in order.iter().enumerate() {
            let node_idx = order[idx];
            for dep_idx in graph.dependency_indices(node_idx) {
                let dep_pos = order.iter().position(|&i| i == dep_idx).unwrap();
                let node_pos = order.iter().position(|&i| i == node_idx).unwrap();
                assert!(dep_pos < node_pos, "dependency at idx {} should come before dependent at idx {}", dep_idx, node_idx);
            }
        }
    }

    #[test]
    fn test_key_accessors_hit_and_miss() {
        let r1 = TestResource::new("catalog", "c1", serde_json::json!({}));
        let r2 = TestResource::new("asset", "a1", serde_json::json!({}));
        let set = ResourceSet::new(vec![r1, r2]).unwrap();
        let c1 = ResourceKey::new("catalog", "c1");
        let a1 = ResourceKey::new("asset", "a1");
        let missing = ResourceKey::new("catalog", "nope");

        // get_by_key: present returns the resource, absent returns None.
        assert_eq!(&*set.get_by_key(&c1).unwrap().key().kind, "catalog");
        assert!(set.get_by_key(&missing).is_none());

        // index_of resolves a key to an index that round-trips through get().
        let idx = set.index_of(&a1).unwrap();
        assert_eq!(set.get(idx).key(), &a1);

        // contains: present vs absent.
        assert!(set.contains(&c1));
        assert!(!set.contains(&missing));
    }
}
