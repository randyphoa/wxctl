//! Generic index-based graph with topological sort and cycle detection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::Hash;

/// Error returned when a cycle is detected in the graph.
#[derive(Debug, Clone, PartialEq)]
pub struct CycleError<N> {
    /// The nodes forming the cycle, in order.
    pub cycle: Vec<N>,
}

impl<N: std::fmt::Debug> std::fmt::Display for CycleError<N> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Cycle detected: {:?}", self.cycle)
    }
}

impl<N: std::fmt::Debug> std::error::Error for CycleError<N> {}

/// Generic index-based graph for efficient traversal.
///
/// All algorithms operate on indices rather than cloning nodes,
/// eliminating allocations in hot paths.
///
/// # Design Decisions
/// - Uses `Vec<usize>` for adjacency lists instead of `HashSet<usize>`
/// - `Vec.contains()` is O(n) but faster than HashSet for typical dependency counts (<20)
///   due to cache locality and no hashing overhead
#[derive(Debug, Clone)]
pub struct IndexGraph<N: Clone + Eq + Hash + std::fmt::Debug> {
    /// Nodes stored in insertion order
    nodes: Vec<N>,
    /// O(1) lookup from node to index
    node_to_idx: HashMap<N, usize>,
    /// Adjacency list: dependencies[idx] = indices of nodes that idx depends on.
    dependencies: Vec<Vec<usize>>,
}

impl<N: Clone + Eq + Hash + std::fmt::Debug> IndexGraph<N> {
    /// Create a new empty graph.
    #[inline]
    pub fn new() -> Self {
        Self { nodes: Vec::new(), node_to_idx: HashMap::new(), dependencies: Vec::new() }
    }

    /// Create a graph with pre-allocated capacity.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Self {
        Self { nodes: Vec::with_capacity(capacity), node_to_idx: HashMap::with_capacity(capacity), dependencies: Vec::with_capacity(capacity) }
    }

    /// Returns the number of nodes in the graph.
    #[inline]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    /// Returns true if the graph has no nodes.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Returns a reference to the node at the given index.
    #[inline]
    pub fn node(&self, idx: usize) -> &N {
        &self.nodes[idx]
    }

    /// Returns an iterator over dependency indices for the node at `idx`.
    #[inline]
    pub fn dependency_indices(&self, idx: usize) -> impl Iterator<Item = usize> + '_ {
        self.dependencies[idx].iter().copied()
    }

    /// Add a node, returning its index. No-op if already exists.
    #[inline]
    pub fn add_node(&mut self, node: N) -> usize {
        if let Some(&idx) = self.node_to_idx.get(&node) {
            return idx;
        }
        let idx = self.nodes.len();
        self.node_to_idx.insert(node.clone(), idx);
        self.nodes.push(node);
        self.dependencies.push(Vec::new());
        idx
    }

    /// Add an edge from `from` to `to` (from depends on to).
    /// Deduplicates edges using Vec.contains() which is O(n) but faster
    /// than HashSet for typical dependency counts (<20).
    #[inline]
    pub fn add_edge(&mut self, from: N, to: N) {
        let from_idx = self.add_node(from);
        let to_idx = self.add_node(to);
        if !self.dependencies[from_idx].contains(&to_idx) {
            self.dependencies[from_idx].push(to_idx);
        }
    }

    /// Add an edge by index. More efficient when indices are already known.
    #[inline]
    pub fn add_edge_by_idx(&mut self, from_idx: usize, to_idx: usize) {
        if !self.dependencies[from_idx].contains(&to_idx) {
            self.dependencies[from_idx].push(to_idx);
        }
    }

    /// Get the index of a node.
    #[inline]
    pub fn get_index(&self, node: &N) -> Option<usize> {
        self.node_to_idx.get(node).copied()
    }

    /// Check if a node exists in the graph.
    #[inline]
    pub fn contains(&self, node: &N) -> bool {
        self.node_to_idx.contains_key(node)
    }

    /// Get all nodes as a slice.
    #[inline]
    pub fn nodes_slice(&self) -> &[N] {
        &self.nodes
    }

    /// Get node at index (returns None if out of bounds).
    #[must_use]
    pub fn get_node(&self, idx: usize) -> Option<&N> {
        self.nodes.get(idx)
    }

    /// Create a new graph containing only nodes that satisfy the predicate.
    /// Edges between retained nodes are preserved.
    pub fn filter<F>(&self, predicate: F) -> Self
    where
        F: Fn(&N) -> bool,
    {
        let mut new_graph = Self::new();

        // First pass: add nodes that match predicate, build old->new index mapping
        let mut old_to_new: HashMap<usize, usize> = HashMap::new();
        for (old_idx, node) in self.nodes.iter().enumerate() {
            if predicate(node) {
                let new_idx = new_graph.add_node(node.clone());
                old_to_new.insert(old_idx, new_idx);
            }
        }

        // Second pass: add edges between retained nodes
        for (old_idx, deps) in self.dependencies.iter().enumerate() {
            if let Some(&new_from_idx) = old_to_new.get(&old_idx) {
                for &old_dep_idx in deps {
                    if let Some(&new_to_idx) = old_to_new.get(&old_dep_idx) {
                        new_graph.add_edge_by_idx(new_from_idx, new_to_idx);
                    }
                }
            }
        }

        new_graph
    }

    // ========================================================================
    // Topological Sort Algorithms
    // ========================================================================

    /// Computes topological order using Kahn's algorithm, returning indices.
    ///
    /// Returns node indices in dependency order: dependencies come before dependents.
    /// Returns `Err(CycleError)` with cycle indices if a cycle is detected.
    pub fn topological_sort_indices(&self) -> Result<Vec<usize>, CycleError<usize>> {
        let n = self.len();
        if n == 0 {
            return Ok(Vec::new());
        }

        let (mut in_degree, dependents) = self.build_in_degree_and_dependents();

        let mut queue: VecDeque<usize> = in_degree.iter().enumerate().filter(|&(_, &deg)| deg == 0).map(|(idx, _)| idx).collect();

        let mut result = Vec::with_capacity(n);

        while let Some(idx) = queue.pop_front() {
            result.push(idx);

            for &dependent_idx in &dependents[idx] {
                in_degree[dependent_idx] -= 1;
                if in_degree[dependent_idx] == 0 {
                    queue.push_back(dependent_idx);
                }
            }
        }

        if result.len() != n {
            let cycle = self.find_cycle_from_in_degree(&in_degree);
            Err(CycleError { cycle })
        } else {
            Ok(result)
        }
    }

    /// Computes topological order using Kahn's algorithm.
    ///
    /// Returns nodes in dependency order: dependencies come before dependents.
    pub fn topological_sort(&self) -> Result<Vec<N>, CycleError<N>> {
        self.topological_sort_indices().map(|indices| indices.into_iter().map(|i| self.nodes[i].clone()).collect()).map_err(|e| CycleError { cycle: e.cycle.into_iter().map(|i| self.nodes[i].clone()).collect() })
    }

    // ========================================================================
    // Cycle Detection
    // ========================================================================

    /// Detects a cycle in the graph using iterative DFS.
    ///
    /// Returns `Some(cycle)` if a cycle exists.
    #[must_use]
    pub fn find_cycle(&self) -> Option<Vec<N>> {
        self.find_cycle_indices().map(|indices| indices.into_iter().map(|i| self.nodes[i].clone()).collect())
    }

    /// Detects a cycle returning indices.
    #[must_use]
    pub fn find_cycle_indices(&self) -> Option<Vec<usize>> {
        let n = self.len();
        if n == 0 {
            return None;
        }

        let mut global_visited = HashSet::with_capacity(n);

        for start in 0..n {
            if global_visited.contains(&start) {
                continue;
            }

            if let Some(cycle) = self.dfs_find_cycle(start, &mut global_visited) {
                return Some(cycle);
            }
        }

        None
    }

    /// Checks if the graph has any cycles (faster than find_cycle).
    #[must_use]
    pub fn has_cycle(&self) -> bool {
        let n = self.len();
        if n == 0 {
            return false;
        }

        let mut global_visited = HashSet::with_capacity(n);

        for start in 0..n {
            if global_visited.contains(&start) {
                continue;
            }

            if self.dfs_has_cycle(start, &mut global_visited) {
                return true;
            }
        }

        false
    }

    // ========================================================================
    // Helpers
    // ========================================================================

    /// Build in-degree counts and dependents lists.
    ///
    /// Returns (in_degree, dependents) where:
    /// - in_degree[i] = number of dependencies for node i
    /// - dependents[i] = indices of nodes that depend on node i
    #[must_use]
    #[inline]
    pub fn build_in_degree_and_dependents(&self) -> (Vec<usize>, Vec<Vec<usize>>) {
        let n = self.len();
        let mut in_degree = vec![0usize; n];
        let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); n];

        for (idx, in_deg) in in_degree.iter_mut().enumerate() {
            for &dep_idx in &self.dependencies[idx] {
                *in_deg += 1;
                dependents[dep_idx].push(idx);
            }
        }

        (in_degree, dependents)
    }

    /// Find cycle starting from nodes with non-zero in-degree (after Kahn's detected cycle).
    fn find_cycle_from_in_degree(&self, in_degree: &[usize]) -> Vec<usize> {
        let start = in_degree.iter().position(|&deg| deg > 0).expect("should have non-zero in-degree when cycle detected");
        self.dfs_find_cycle(start, &mut HashSet::new()).unwrap_or_default()
    }

    /// Iterative DFS to find a cycle from a starting node.
    fn dfs_find_cycle(&self, start: usize, global_visited: &mut HashSet<usize>) -> Option<Vec<usize>> {
        let mut visited = HashSet::new();
        let mut path = Vec::new();
        let mut path_set = HashSet::new();
        let mut stack: Vec<(usize, usize)> = vec![(start, 0)];

        visited.insert(start);
        path.push(start);
        path_set.insert(start);

        while let Some((current, pos)) = stack.last_mut() {
            let deps = &self.dependencies[*current];
            if *pos < deps.len() {
                let next = deps[*pos];
                *pos += 1;

                if path_set.contains(&next) {
                    let cycle_start = path.iter().position(|&i| i == next).unwrap();
                    return Some(path[cycle_start..].to_vec());
                }

                if !visited.contains(&next) {
                    visited.insert(next);
                    path.push(next);
                    path_set.insert(next);
                    stack.push((next, 0));
                }
            } else {
                let node = path.pop().unwrap();
                path_set.remove(&node);
                stack.pop();
            }
        }

        global_visited.extend(visited);
        None
    }

    /// Iterative DFS to check for cycle existence (no path tracking).
    fn dfs_has_cycle(&self, start: usize, global_visited: &mut HashSet<usize>) -> bool {
        let mut visited = HashSet::new();
        let mut path_set = HashSet::new();
        let mut stack: Vec<(usize, usize, bool)> = vec![(start, 0, true)];

        while let Some((current, pos, entering)) = stack.last_mut() {
            if *entering {
                *entering = false;
                visited.insert(*current);
                path_set.insert(*current);
            }

            let deps = &self.dependencies[*current];
            if *pos < deps.len() {
                let next = deps[*pos];
                *pos += 1;

                if path_set.contains(&next) {
                    return true;
                }

                if !visited.contains(&next) {
                    stack.push((next, 0, true));
                }
            } else {
                path_set.remove(current);
                stack.pop();
            }
        }

        global_visited.extend(visited);
        false
    }
}

impl<N: Clone + Eq + Hash + std::fmt::Debug> Default for IndexGraph<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a graph from `(dependent, dependency)` edges (dependent depends on dependency).
    fn graph_from(edges: &[(i32, i32)]) -> IndexGraph<i32> {
        let mut g = IndexGraph::new();
        for &(from, to) in edges {
            g.add_edge(from, to);
        }
        g
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_topological_sort_orders_dependencies_first() {
        // (label, edges, ordering constraints as (must_come_before, after))
        let cases: &[(&str, &[(i32, i32)], &[(i32, i32)])] = &[
            // linear chain 0→1→2: 2 before 1 before 0
            ("linear", &[(0, 1), (1, 2)], &[(2, 1), (1, 0)]),
            // diamond A→B, A→C, B→D, C→D: D before B/C, B/C before A
            ("diamond", &[(1, 2), (1, 3), (2, 4), (3, 4)], &[(4, 2), (4, 3), (2, 1), (3, 1)]),
        ];
        for (label, edges, constraints) in cases {
            let graph = graph_from(edges);
            let order = graph.topological_sort().unwrap();
            let pos = |n: i32| order.iter().position(|x| *x == n).unwrap();
            for (before, after) in *constraints {
                assert!(pos(*before) < pos(*after), "{before} must precede {after} for {label}");
            }
        }
    }

    #[test]
    fn test_add_node_and_edge_dedup() {
        // add_node is idempotent on value: same value → same index, len unchanged.
        let mut graph = IndexGraph::new();
        let idx1 = graph.add_node(42);
        let idx2 = graph.add_node(42);
        assert_eq!(idx1, idx2);
        assert_eq!(graph.len(), 1);

        // add_edge deduplicates a repeated edge.
        let mut graph = IndexGraph::new();
        graph.add_edge(1, 2);
        graph.add_edge(1, 2);
        let dep_count: usize = graph.dependency_indices(graph.get_index(&1).unwrap()).count();
        assert_eq!(dep_count, 1);
    }

    #[test]
    fn test_cycle_detection_agrees_across_apis() {
        // Acyclic 1→2→3: sort ok, has_cycle false, find_cycle None.
        let acyclic = graph_from(&[(1, 2), (2, 3)]);
        assert!(acyclic.topological_sort().is_ok());
        assert!(!acyclic.has_cycle());
        assert!(acyclic.find_cycle().is_none());

        // Cyclic A↔B: sort errs, has_cycle true, find_cycle Some.
        let cyclic = graph_from(&[(1, 2), (2, 1)]);
        assert!(cyclic.topological_sort().is_err());
        assert!(cyclic.has_cycle());
        assert!(cyclic.find_cycle().is_some());
    }

    #[test]
    fn test_filter_preserves_surviving_edges_drops_removed() {
        // Diamond 1→2, 1→3, 2→4, 3→4; remove node 1.
        let graph = graph_from(&[(1, 2), (1, 3), (2, 4), (3, 4)]);
        let filtered = graph.filter(|n| *n != 1);
        assert_eq!(filtered.len(), 3);
        assert!(filtered.contains(&2) && filtered.contains(&3) && filtered.contains(&4));
        assert!(!filtered.contains(&1));
        // Edges among surviving nodes (2→4, 3→4) are preserved.
        let idx2 = filtered.get_index(&2).unwrap();
        let idx3 = filtered.get_index(&3).unwrap();
        let idx4 = filtered.get_index(&4).unwrap();
        assert!(filtered.dependency_indices(idx2).any(|i| i == idx4));
        assert!(filtered.dependency_indices(idx3).any(|i| i == idx4));

        // 0→1, 0→2; remove node 1 → 0's only remaining dependency is 2 (the 0→1 edge is dropped).
        let graph = graph_from(&[(0, 1), (0, 2)]);
        let filtered = graph.filter(|n| *n != 1);
        assert_eq!(filtered.len(), 2);
        assert!(!filtered.contains(&1));
        let idx0 = filtered.get_index(&0).unwrap();
        let deps: Vec<usize> = filtered.dependency_indices(idx0).collect();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0], filtered.get_index(&2).unwrap());
    }
}
