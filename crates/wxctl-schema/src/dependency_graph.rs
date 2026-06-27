//! Compile-time generated dependency graph with zero-allocation filtering.
//!
//! This module provides access to resource dependency information that is
//! computed at compile time from YAML schemas. Uses phf for O(1) lookups
//! and index-based dependencies for efficient graph traversal.
//!
//! # Architecture
//!
//! - `SchemaDependencyGraph`: Static schema-level dependencies (compile-time)
//! - `wxctl_core::DependencyGraph` (aka `IndexGraph<ResourceKey>`): Runtime resource dependencies

use crate::deployment::{Deployment, DeploymentConstraintList};
use std::collections::{HashMap, HashSet, VecDeque};
#[allow(unused_imports)]
use std::str::FromStr;

// Include generated static data (phf map, resources with index-based deps)
include!(concat!(env!("OUT_DIR"), "/dependency_graph_generated.rs"));

/// Metadata for a single resource type.
#[derive(Debug, Clone, Copy)]
pub struct ResourceData {
    pub name: &'static str,
    pub required_fields: &'static [&'static str],
    pub optional_fields: &'static [&'static str],
    /// Dependency indices into RESOURCES array
    pub dependency_indices: &'static [usize],
}

/// A dependency edge from a field to a target resource type.
///
/// Each edge records which field creates the dependency and whether
/// that field is required. This enables conditional edge activation:
/// required edges always activate, optional edges only activate when
/// the field is present in the user's configuration.
#[derive(Debug, Clone, Copy)]
pub struct EdgeInfo {
    /// The field name on the source resource that creates this dependency.
    pub field_name: &'static str,
    /// Index of the target (prerequisite) resource in RESOURCES array.
    pub target_index: usize,
    /// Whether the field is required (always activates) or optional (conditional).
    pub required: bool,
}

/// A fully-resolved path through the dependency graph.
#[derive(Debug)]
pub struct ResolvedPath {
    pub name: String,
    /// True on exactly one path per `enumerate_paths` call: the structurally
    /// smallest (fewest resources), ties broken by bridge order.
    pub recommended: bool,
    pub resources: Vec<ResolvedResource>,
    pub edges: Vec<ResolvedEdge>,
}

/// A resource in a resolved path with its constraint values.
#[derive(Debug, Clone)]
pub struct ResolvedResource {
    pub kind: &'static str,
    pub constraints: Vec<Constraint>,
    pub added_by: Option<String>,
    pub field_mappings: Vec<(&'static str, String)>,
}

/// A constraint on a resource field within a path.
///
/// A single allowed value renders as `value:`; multiple allowed values
/// collapse into `one_of:` (the caller picks one). This replaces the old
/// per-value path explosion with one structural path carrying the choice set.
#[derive(Debug, Clone, PartialEq)]
pub struct Constraint {
    pub name: &'static str,
    /// Allowed values. Length 1 → serialized as `value`; length > 1 → `one_of`.
    pub values: Vec<&'static str>,
}

impl Constraint {
    pub fn single(name: &'static str, value: &'static str) -> Self {
        Constraint { name, values: vec![value] }
    }
    pub fn one_of(name: &'static str, values: Vec<&'static str>) -> Self {
        Constraint { name, values }
    }
    /// True when there is exactly one allowed value.
    pub fn is_single(&self) -> bool {
        self.values.len() == 1
    }
}

/// An edge in a resolved path.
#[derive(Debug, Clone)]
pub struct ResolvedEdge {
    pub source: &'static str,
    pub target: &'static str,
    pub edge_type: EdgeType,
    pub field: String,
}

/// Type of dependency edge.
#[derive(Debug, Clone, PartialEq)]
pub enum EdgeType {
    /// Schema-level reference
    Reference,
    /// Cross-service bridge from linkages.yaml
    Bridge(String),
}

/// O(1) lookup: returns index of resource in RESOURCES array.
#[inline]
pub fn resource_index(name: &str) -> Option<usize> {
    RESOURCE_INDEX.get(name).copied()
}

/// Returns the topological order of all resources.
#[inline]
pub fn topological_order() -> &'static [&'static str] {
    TOPOLOGICAL_ORDER
}

/// Returns resource data by index. Panics if out of bounds.
#[inline]
pub fn get_resource_by_index(idx: usize) -> ResourceData {
    let (name, req, opt, deps) = RESOURCES[idx];
    ResourceData { name, required_fields: req, optional_fields: opt, dependency_indices: deps }
}

/// Returns resource data by name, or None if not found. O(1) lookup.
#[inline]
pub fn get_resource(name: &str) -> Option<ResourceData> {
    resource_index(name).map(get_resource_by_index)
}

/// Returns raw dependency edge tuples for a resource by index.
#[inline]
fn get_edge_tuples_by_index(idx: usize) -> &'static [(&'static str, usize, bool)] {
    RESOURCE_EDGES[idx]
}

/// Returns dependency edge tuples for a resource by name.
/// Each tuple is `(field_name, target_resource_index, field_is_required)`.
#[inline]
pub fn get_edges(name: &str) -> Option<&'static [(&'static str, usize, bool)]> {
    resource_index(name).map(get_edge_tuples_by_index)
}

/// Check whether any value in the list is a `${target_kind.xxx}` template reference.
///
/// A value references a target kind if it matches the pattern `${kind.name}`
/// or `${kind.name.field.path}` where `kind` equals the target resource type.
fn values_reference_kind(values: &[&str], target_kind: &str) -> bool {
    let prefix = format!("${{{target_kind}.");
    values.iter().any(|v| v.starts_with(&prefix) && v.ends_with('}'))
}

/// Returns the compiled resource catalog: (kind, service, description).
#[inline]
pub fn resource_catalog() -> &'static [(&'static str, &'static str, &'static str)] {
    RESOURCE_CATALOG
}

/// Returns prompt authoring notes for a kind (empty slice if none declared).
#[inline]
pub fn resource_prompt_notes(kind: &str) -> &'static [&'static str] {
    RESOURCE_PROMPT_NOTES.iter().find(|(k, _)| *k == kind).map(|(_, notes)| *notes).unwrap_or(&[])
}

/// Returns the deployment flavors (`saas`, `software`) a kind supports.
///
/// Derived from the kind's `UNSUPPORTED_ON` constraint (parallel to `RESOURCES`,
/// looked up by `resource_index`). A flavor is supported unless the kind's
/// `unsupported_on` constraint list matches a deployment of that flavor.
/// Returns an empty vec for an unknown kind.
pub fn deployment_support(kind: &str) -> Vec<&'static str> {
    let Some(idx) = resource_index(kind) else {
        return Vec::new();
    };
    let raw = UNSUPPORTED_ON[idx];
    let unsupported = DeploymentConstraintList::from_str(raw).unwrap_or_default();

    // Representative concrete deployments, one per flavor. The software version
    // is arbitrary — today's constraints are flavor-only, and a `software-X`
    // constraint still matches any concrete software version at flavor level.
    let saas = Deployment::Saas;
    let software = Deployment::from_str("software-5.3.0").expect("valid representative deployment");

    let mut flavors: Vec<&'static str> = Vec::with_capacity(2);
    if !unsupported.matches(&saas) {
        flavors.push("saas");
    }
    if !unsupported.matches(&software) {
        flavors.push("software");
    }
    flavors
}

/// Formats the resource catalog as a markdown table.
pub fn resource_catalog_markdown() -> String {
    use std::fmt::Write;
    let mut out = String::new();
    writeln!(out, "| Kind | Service | Description |").unwrap();
    writeln!(out, "|------|---------|-------------|").unwrap();
    for &(kind, service, desc) in RESOURCE_CATALOG {
        writeln!(out, "| {} | {} | {} |", kind, service, desc).unwrap();
    }
    out
}

/// Returns true when a bridge should be active for the given deployment.
///
/// A bridge is suppressed when:
/// - Either endpoint kind lists the active deployment in its `UNSUPPORTED_ON` constraints.
/// - The bridge's `when_deployment` string is non-empty and does not match the deployment.
///
/// An empty `when_deployment` (the common case) means always active.
pub(crate) fn bridge_active_for(bridge: &BridgeTuple, deployment: &Deployment) -> bool {
    let (_, source_idx, target_idx, _, _, when_deployment) = *bridge;
    for idx in [source_idx, target_idx] {
        let raw = UNSUPPORTED_ON[idx];
        if raw.is_empty() {
            continue;
        }
        if let Ok(list) = DeploymentConstraintList::from_str(raw)
            && list.matches(deployment)
        {
            return false;
        }
    }
    if when_deployment.is_empty() {
        return true;
    }
    match DeploymentConstraintList::from_str(when_deployment) {
        Ok(list) => list.is_empty() || list.matches(deployment),
        Err(_) => true, // tolerate malformed `when:` rather than silently suppress
    }
}

/// A compile-time schema dependency graph with filtering support.
///
/// Uses indices into the static `RESOURCES` array instead of
/// allocating strings. Provides O(1) index lookups.
///
/// Use this for schema-level dependencies (resource type relationships);
/// use `wxctl_core::DependencyGraph` for runtime resource instance dependencies.
#[derive(Debug)]
pub struct SchemaDependencyGraph {
    /// Included resource indices in topological order
    included_ordered: Vec<usize>,
    /// Set of included indices for O(1) membership checks
    included_set: HashSet<usize>,
}

impl SchemaDependencyGraph {
    /// Create a filtered graph containing only the requested resources
    /// and their transitive dependencies.
    pub fn new(requested: &[&str]) -> Self {
        let mut included_set = HashSet::with_capacity(requested.len() * 2);

        for name in requested {
            if let Some(idx) = resource_index(name) {
                Self::collect_dependencies(idx, &mut included_set);
            }
        }

        // Build ordered list by filtering TOPOLOGICAL_ORDER
        let included_ordered: Vec<usize> = TOPOLOGICAL_ORDER.iter().filter_map(|name| resource_index(name)).filter(|idx| included_set.contains(idx)).collect();

        Self { included_ordered, included_set }
    }

    /// Recursively collect a resource and all its dependencies by index.
    fn collect_dependencies(idx: usize, included: &mut HashSet<usize>) {
        if !included.insert(idx) {
            return; // Already processed
        }

        // Direct index access - no string lookup
        let (_, _, _, dep_indices) = RESOURCES[idx];
        for &dep_idx in dep_indices {
            Self::collect_dependencies(dep_idx, included);
        }
    }

    /// Iterate over resource names in topological order (filtered).
    #[inline]
    pub fn iter_order(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.included_ordered.iter().map(|&idx| RESOURCES[idx].0)
    }

    /// Get resource data if it's included in the filter. O(1) lookup.
    #[inline]
    pub fn get(&self, name: &str) -> Option<ResourceData> {
        let idx = resource_index(name)?;
        if self.included_set.contains(&idx) { Some(get_resource_by_index(idx)) } else { None }
    }

    /// Check if a resource is included in the filter by index.
    #[inline]
    pub fn contains_idx(&self, idx: usize) -> bool {
        self.included_set.contains(&idx)
    }

    /// Check if a resource is included in the filter by name.
    #[inline]
    pub fn contains(&self, name: &str) -> bool {
        resource_index(name).is_some_and(|idx| self.included_set.contains(&idx))
    }

    /// Returns the number of included resources.
    #[inline]
    pub fn len(&self) -> usize {
        self.included_ordered.len()
    }

    /// Returns true if no resources are included.
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.included_ordered.is_empty()
    }

    /// Compute the transitive dependency closure from a partial specification
    /// with **conditional edge activation** based on field values.
    ///
    /// Unlike `new()` which includes ALL transitive dependencies unconditionally,
    /// this method activates edges selectively using three rules:
    ///
    /// 1. **Reference detection**: If the field value contains a `${kind.name}`
    ///    template reference to the target resource type, the edge activates.
    ///    Plain string values (e.g., built-in model names) do NOT activate the edge,
    ///    even if the field is required in the schema.
    /// 2. **Optional field presence**: Optional fields only activate when present
    ///    in user config with a reference value.
    /// 3. **Computed dependencies**: For resources not in the user's config
    ///    (scaffolded dependencies), all required edges activate and optional
    ///    edges are skipped.
    ///
    /// # Arguments
    /// - `targets`: Resource type names to compute closure for
    /// - `field_values`: Map from resource kind → field_name → list of string values.
    ///   If `None`, behaves like `new()` (all edges activated).
    ///
    /// # Algorithm
    /// Matches COMPUTE_CLOSURE from the IP disclosure:
    /// 1. Initialize closure with target indices
    /// 2. BFS from targets
    /// 3. For each node, iterate edges: activate based on reference detection
    /// 4. Build induced subgraph filtered to TOPOLOGICAL_ORDER
    pub fn compute_closure(targets: &[&str], field_values: Option<&HashMap<&str, HashMap<&str, Vec<&str>>>>) -> Self {
        let mut included_set = HashSet::with_capacity(targets.len() * 2);
        let mut queue = VecDeque::with_capacity(targets.len() * 2);

        // Seed with target resource indices
        for name in targets {
            if let Some(idx) = resource_index(name)
                && included_set.insert(idx)
            {
                queue.push_back(idx);
            }
        }

        // BFS with conditional edge activation
        while let Some(idx) = queue.pop_front() {
            let resource_name = RESOURCES[idx].0;
            let edge_tuples = get_edge_tuples_by_index(idx);

            for &(field_name, target_idx, required) in edge_tuples {
                // Skip self-references (e.g., agent.collaborators → agent)
                if target_idx == idx {
                    continue;
                }

                let target_kind = RESOURCES[target_idx].0;

                let activate = match field_values {
                    None => true, // No config provided → activate all (backward compat)
                    Some(config) => {
                        match config.get(resource_name) {
                            Some(fields) => {
                                // Resource is in user config — check field values
                                match fields.get(field_name) {
                                    Some(values) => {
                                        // Field is set — activate only if any value
                                        // is a ${target_kind.xxx} reference
                                        values_reference_kind(values, target_kind)
                                    }
                                    None => {
                                        // Field not set — activate if required
                                        // (partial spec: user hasn't filled it yet
                                        // but the schema mandates this dependency)
                                        required
                                    }
                                }
                            }
                            None => {
                                // Computed dependency (not in user config) —
                                // activate required edges, skip optional
                                required
                            }
                        }
                    }
                };

                if activate && included_set.insert(target_idx) {
                    queue.push_back(target_idx);
                }
            }
        }

        // Filter TOPOLOGICAL_ORDER to included set
        let included_ordered: Vec<usize> = TOPOLOGICAL_ORDER.iter().filter_map(|name| resource_index(name)).filter(|idx| included_set.contains(idx)).collect();

        Self { included_ordered, included_set }
    }

    /// Returns activated edges for a resource in this closure.
    /// Only returns edges whose targets are included in the closure.
    pub fn activated_edges(&self, name: &str) -> Vec<EdgeInfo> {
        let Some(idx) = resource_index(name) else {
            return Vec::new();
        };
        if !self.included_set.contains(&idx) {
            return Vec::new();
        }

        get_edge_tuples_by_index(idx).iter().filter(|&&(_, target_idx, _)| self.included_set.contains(&target_idx)).map(|&(field_name, target_index, required)| EdgeInfo { field_name, target_index, required }).collect()
    }

    /// Compute closure from kind names, then expand with bridge endpoints.
    ///
    /// After computing the initial schema closure (required edges only),
    /// checks for bridges where one endpoint is in the closure and adds
    /// the other endpoint (plus its transitive deps). Repeats until stable.
    /// Bridges inactive for `deployment` are skipped during expansion.
    pub fn compute_closure_from_kinds(kinds: &[&str]) -> Self {
        Self::compute_closure_from_kinds_with_config(kinds, None, &Deployment::Saas)
    }

    /// Like `compute_closure_from_kinds`, but uses provided field values for
    /// conditional edge activation. When a partial config has actual values
    /// (e.g. `llm: ${model.foo}`), those values activate the reference edge.
    /// Fields absent from the map fall back to required-only activation.
    pub fn compute_closure_from_kinds_with_config(kinds: &[&str], field_values: Option<&HashMap<&str, HashMap<&str, Vec<&str>>>>, deployment: &Deployment) -> Self {
        let mut current_kinds: Vec<&str> = kinds.to_vec();
        // Some(&empty) rather than None: None activates ALL edges unconditionally,
        // while Some(&empty) activates only required edges for resources absent
        // from the config — needed here so optional edges are suppressed.
        let empty_config: HashMap<&str, HashMap<&str, Vec<&str>>> = HashMap::new();
        let config = field_values.unwrap_or(&empty_config);

        // Pre-compute which bridge endpoints were explicitly requested.
        // Only these bridges are eligible for expansion.
        let explicit_indices: HashSet<usize> = kinds.iter().filter_map(|k| resource_index(k)).collect();

        loop {
            let closure = Self::compute_closure(&current_kinds, Some(config));
            let mut new_kinds = Vec::new();

            for bridge in BRIDGES {
                if !bridge_active_for(bridge, deployment) {
                    continue;
                }
                let &(_, source_idx, target_idx, _, _, _) = bridge;
                if !explicit_indices.contains(&source_idx) && !explicit_indices.contains(&target_idx) {
                    continue;
                }

                let has_source = closure.included_set.contains(&source_idx);
                let has_target = closure.included_set.contains(&target_idx);
                let source_kind = RESOURCES[source_idx].0;
                let target_kind = RESOURCES[target_idx].0;

                if has_source && !has_target && !current_kinds.contains(&target_kind) {
                    new_kinds.push(target_kind);
                } else if !has_source && has_target && !current_kinds.contains(&source_kind) {
                    new_kinds.push(source_kind);
                }
            }

            if new_kinds.is_empty() {
                return closure;
            }
            current_kinds.extend(new_kinds);
        }
    }

    /// Find all bridges where both source and target are in the closure.
    /// Bridges inactive for `deployment` are excluded.
    pub fn find_bridges(&self, deployment: &Deployment) -> Vec<&'static BridgeTuple> {
        BRIDGES.iter().filter(|b| bridge_active_for(b, deployment)).filter(|&&(_, source_idx, target_idx, _, _, _)| self.included_set.contains(&source_idx) && self.included_set.contains(&target_idx)).collect()
    }

    /// Enumerate fully-resolved paths — one per **structural** shape (distinct bridge).
    ///
    /// Multi-value bridge constraints collapse into a single `Constraint::one_of`
    /// rather than exploding into one path per value; single-value constraints
    /// become `Constraint::single`. `recommended` is set on the path with the
    /// fewest resources (ties broken by bridge order, i.e. the order paths are
    /// pushed here, which follows the compile-time `BRIDGES` array).
    pub fn enumerate_paths(&self, bridges: &[&'static BridgeTuple], original_kinds: &[&str]) -> Vec<ResolvedPath> {
        if bridges.is_empty() {
            // No bridges — single path with all resources, no constraints.
            let resources: Vec<ResolvedResource> = self.iter_order().map(|kind| ResolvedResource { kind, constraints: Vec::new(), added_by: None, field_mappings: Vec::new() }).collect();
            let edges = self.collect_reference_edges();
            return vec![ResolvedPath { name: "default".to_string(), recommended: true, resources, edges }];
        }

        let mut paths = Vec::new();

        for &&(bridge_name, source_idx, target_idx, constraints, field_mappings, _when_deployment) in bridges {
            let source_kind = RESOURCES[source_idx].0;
            let target_kind = RESOURCES[target_idx].0;

            // Build resource list in topological order. Each resource collects the
            // constraints that apply to it (collapsed to single/one_of) and, for the
            // bridge target, the field mappings from the source.
            let resources: Vec<ResolvedResource> = self
                .iter_order()
                .map(|kind| {
                    let mut res_constraints = Vec::new();
                    for &(c_res_idx, c_field, c_values) in constraints {
                        if RESOURCES[c_res_idx].0 != kind {
                            continue;
                        }
                        if c_values.len() == 1 {
                            res_constraints.push(Constraint::single(c_field, c_values[0]));
                        } else {
                            res_constraints.push(Constraint::one_of(c_field, c_values.to_vec()));
                        }
                    }

                    let added_by = if original_kinds.contains(&kind) { None } else { Some(format!("bridge:{}", bridge_name)) };

                    let mut fmappings = Vec::new();
                    if kind == target_kind {
                        for &(src_field, tgt_field) in field_mappings {
                            fmappings.push((tgt_field, format!("{}.{}", source_kind, src_field)));
                        }
                    }

                    ResolvedResource { kind, constraints: res_constraints, added_by, field_mappings: fmappings }
                })
                .collect();

            let mut edges = self.collect_reference_edges();
            edges.push(ResolvedEdge { source: source_kind, target: target_kind, edge_type: EdgeType::Bridge(bridge_name.to_string()), field: String::new() });

            paths.push(ResolvedPath { name: bridge_name.to_string(), recommended: false, resources, edges });
        }

        // Recommend the fewest-resource path; ties → first (bridge order).
        if let Some(best) = paths.iter().enumerate().min_by_key(|(i, p)| (p.resources.len(), *i)).map(|(i, _)| i) {
            paths[best].recommended = true;
        }

        paths
    }

    /// Collect all reference edges within the closure.
    fn collect_reference_edges(&self) -> Vec<ResolvedEdge> {
        let mut edges = Vec::new();
        for &idx in &self.included_ordered {
            let source = RESOURCES[idx].0;
            for &(field_name, target_idx, _required) in get_edge_tuples_by_index(idx) {
                if self.included_set.contains(&target_idx) && target_idx != idx {
                    let target = RESOURCES[target_idx].0;
                    edges.push(ResolvedEdge { source, target, edge_type: EdgeType::Reference, field: field_name.to_string() });
                }
            }
        }
        edges
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Render the compiled dependency graph to a deterministic, review-friendly
    /// string. Everything is sorted by name so the snapshot is stable regardless
    /// of `build.rs` codegen order (schema `read_dir` order and `HashSet` dep
    /// iteration are both non-deterministic; sorting here neutralizes that).
    fn render_graph_snapshot() -> String {
        use std::fmt::Write;
        let mut out = String::new();
        writeln!(out, "# wxctl-schema dependency graph snapshot (structural drift guard).").unwrap();
        writeln!(out, "# Regenerate after an intentional change, then review the diff:").unwrap();
        writeln!(out, "#   WXCTL_REGEN_GRAPH_GOLDEN=1 cargo test -p wxctl-schema -- graph_snapshot").unwrap();
        writeln!(out).unwrap();

        let mut names: Vec<&str> = TOPOLOGICAL_ORDER.to_vec();
        names.sort_unstable();
        writeln!(out, "## resources ({})", names.len()).unwrap();
        for name in &names {
            let idx = resource_index(name).expect("name from TOPOLOGICAL_ORDER must resolve");
            writeln!(out, "{name}").unwrap();
            let (_, _, _, dep_idxs) = RESOURCES[idx];
            let mut deps: Vec<&str> = dep_idxs.iter().map(|&i| RESOURCES[i].0).collect();
            deps.sort_unstable();
            for d in &deps {
                writeln!(out, "  dep {d}").unwrap();
            }
            let mut edges: Vec<(String, &str, bool)> = RESOURCE_EDGES[idx].iter().map(|&(field, tidx, req)| (field.to_string(), RESOURCES[tidx].0, req)).collect();
            edges.sort();
            for (field, target, req) in &edges {
                writeln!(out, "  edge {} -> {} ({})", field, target, if *req { "required" } else { "optional" }).unwrap();
            }
        }
        writeln!(out).unwrap();

        let mut bridges: Vec<&BridgeTuple> = BRIDGES.iter().collect();
        bridges.sort_by_key(|b| b.0);
        writeln!(out, "## bridges ({})", bridges.len()).unwrap();
        for &&(bname, sidx, tidx, constraints, mappings, when) in &bridges {
            let when_s = if when.is_empty() { "always" } else { when };
            writeln!(out, "{}: {} -> {} when={}", bname, RESOURCES[sidx].0, RESOURCES[tidx].0, when_s).unwrap();
            let mut cs: Vec<String> = constraints.iter().map(|&(ridx, field, vals)| format!("  constraint {}.{} in [{}]", RESOURCES[ridx].0, field, vals.join(", "))).collect();
            cs.sort();
            for c in &cs {
                writeln!(out, "{c}").unwrap();
            }
            let mut ms: Vec<String> = mappings.iter().map(|&(s, t)| format!("  map {s} -> {t}")).collect();
            ms.sort();
            for m in &ms {
                writeln!(out, "{m}").unwrap();
            }
        }
        out
    }

    /// Drift guard: the compiled graph must match the committed golden snapshot.
    /// A diff here means a schema/linkage edit changed the dependency structure —
    /// intended changes are accepted by regenerating the golden (see message).
    #[test]
    fn graph_snapshot_matches_golden() {
        let actual = render_graph_snapshot();
        let golden_path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/dependency_graph.golden");
        if std::env::var("WXCTL_REGEN_GRAPH_GOLDEN").is_ok() {
            std::fs::write(golden_path, &actual).expect("write golden snapshot");
            return;
        }
        // Normalize line endings: git may check out the golden file with CRLF on
        // Windows, whereas the generated snapshot always uses LF (writeln!).
        let expected = std::fs::read_to_string(golden_path).unwrap_or_default().replace("\r\n", "\n");
        assert_eq!(actual, expected, "dependency graph drifted from the committed golden snapshot.\nIf this change is intentional, regenerate and review the diff:\n    WXCTL_REGEN_GRAPH_GOLDEN=1 cargo test -p wxctl-schema -- graph_snapshot");
    }

    #[test]
    fn test_find_bridges_endpoint_presence() {
        // Both endpoints explicitly requested → at least one expected bridge found.
        let both = SchemaDependencyGraph::compute_closure_from_kinds(&["orchestrate_connection", "common_core_connection"]);
        let bridges = both.find_bridges(&Deployment::Saas);
        assert!(!bridges.is_empty(), "Should find at least one bridge");
        let names: Vec<&str> = bridges.iter().map(|b| b.0).collect();
        assert!(names.contains(&"database_access") || names.contains(&"object_storage_access"), "Expected database_access or object_storage_access bridge, got: {:?}", names);

        // Only one endpoint requested → bridge expansion adds the missing endpoint, bridges still found.
        let expanded = SchemaDependencyGraph::compute_closure_from_kinds(&["common_core_connection"]);
        assert!(expanded.contains("orchestrate_connection"), "Bridge expansion should add orchestrate_connection");
        assert!(!expanded.find_bridges(&Deployment::Saas).is_empty(), "Should find bridges after expansion");

        // No bridge links catalog↔category → empty.
        let none = SchemaDependencyGraph::compute_closure_from_kinds(&["catalog", "category"]);
        assert!(none.find_bridges(&Deployment::Saas).is_empty(), "Should find no bridges between catalog and category");
    }

    #[test]
    fn test_compute_closure_from_kinds() {
        let closure = SchemaDependencyGraph::compute_closure_from_kinds(&["agent"]);
        assert!(closure.contains("agent"));
    }

    #[test]
    fn test_enumerate_paths_collapses_multivalue_to_one_of() {
        // database_access constrains common_core_connection.datasource_type to 5
        // values. The old behavior was 5 paths; the new behavior is one structural
        // path per bridge with a single one_of constraint.
        let input_kinds = &["orchestrate_connection", "common_core_connection"];
        let closure = SchemaDependencyGraph::compute_closure_from_kinds(input_kinds);
        let bridges = closure.find_bridges(&Deployment::Saas);
        let paths = closure.enumerate_paths(&bridges, input_kinds);

        // One path per distinct bridge (database_access + object_storage_access).
        assert_eq!(paths.len(), bridges.len(), "Expected one path per bridge, got {}", paths.len());

        let db = paths.iter().find(|p| p.name == "database_access").expect("database_access path");
        let cc = db.resources.iter().find(|r| r.kind == "common_core_connection").expect("common_core_connection resource");
        let dst = cc.constraints.iter().find(|c| c.name == "datasource_type").expect("datasource_type constraint");
        assert!(!dst.is_single(), "datasource_type should collapse to one_of, not a single value");
        assert!(dst.values.contains(&"postgres") && dst.values.contains(&"netezza"), "one_of should list all datasource values, got {:?}", dst.values);

        // The orchestrate_connection.connection_type single-value constraint stays single.
        let oc = db.resources.iter().find(|r| r.kind == "orchestrate_connection").expect("orchestrate_connection resource");
        let ct = oc.constraints.iter().find(|c| c.name == "connection_type").expect("connection_type constraint");
        assert!(ct.is_single(), "connection_type should stay a single value");
        assert_eq!(ct.values, vec!["key_value_creds"]);
    }

    #[test]
    fn test_enumerate_paths_marks_one_recommended() {
        let input_kinds = &["orchestrate_connection", "common_core_connection"];
        let closure = SchemaDependencyGraph::compute_closure_from_kinds(input_kinds);
        let bridges = closure.find_bridges(&Deployment::Saas);
        let paths = closure.enumerate_paths(&bridges, input_kinds);

        let recommended: Vec<&str> = paths.iter().filter(|p| p.recommended).map(|p| p.name.as_str()).collect();
        assert_eq!(recommended.len(), 1, "Exactly one path must be recommended, got {:?}", recommended);

        // The recommended path must have the minimum resource count.
        let min_len = paths.iter().map(|p| p.resources.len()).min().unwrap();
        let rec = paths.iter().find(|p| p.recommended).unwrap();
        assert_eq!(rec.resources.len(), min_len, "Recommended path must be the fewest-resource path");
    }

    #[test]
    fn test_simple_kinds_yield_lone_recommended_default_path() {
        // [tool, knowledge_base, agent] has no explicitly-requested bridge endpoint,
        // so no bridge expansion: exactly one "default" path, marked recommended, and
        // no cross-service bridge resources pulled in.
        let kinds = &["tool", "knowledge_base", "agent"];
        let closure = SchemaDependencyGraph::compute_closure_from_kinds(kinds);
        let bridges = closure.find_bridges(&Deployment::Saas);
        let paths = closure.enumerate_paths(&bridges, kinds);
        assert_eq!(paths.len(), 1, "Simple kinds without connections should produce exactly 1 path, got {}", paths.len());
        assert_eq!(paths[0].name, "default");
        assert!(paths[0].recommended, "the lone default path must be recommended");
        assert!(!closure.contains("common_core_connection"), "common_core_connection should not be pulled in for simple kinds");
        assert!(!closure.contains("catalog"), "catalog should not be pulled in for simple kinds");
    }

    #[test]
    fn test_resource_catalog_contains_all_resources() {
        let catalog = resource_catalog();
        assert!(catalog.len() >= 5, "Catalog should contain at least 5 resources, got {}", catalog.len());
        let kinds: Vec<&str> = catalog.iter().map(|&(k, _, _)| k).collect();
        assert!(kinds.contains(&"agent"), "Catalog should contain 'agent'");
        assert!(kinds.contains(&"tool"), "Catalog should contain 'tool'");
    }

    #[test]
    fn test_prompt_notes_accessor() {
        // `tool` declares prompt.notes in its schema; unknown kinds return empty.
        assert!(!resource_prompt_notes("tool").is_empty(), "tool should have prompt notes");
        assert!(resource_prompt_notes("definitely_not_a_kind").is_empty());
    }

    #[test]
    fn test_conditional_edge_activation_without_config() {
        // Without config values, only required *reference* edges activate; optional
        // edges and optional-reference-on-required-field edges stay dormant.

        // tool.binding.python.connections is optional (required=false) → not followed:
        // closure of ['tool'] is just tool itself.
        let tool = SchemaDependencyGraph::compute_closure_from_kinds(&["tool"]);
        let kinds: Vec<&str> = tool.iter_order().collect();
        assert_eq!(kinds, vec!["tool"], "Closure of ['tool'] should only contain tool itself, got: {:?}", kinds);

        // agent.llm is a required field but its reference to model is optional
        // (accepts literal strings) → neither model nor its transitive deps pull in.
        let agent = SchemaDependencyGraph::compute_closure_from_kinds(&["agent"]);
        assert!(agent.contains("agent"));
        assert!(!agent.contains("model"), "model should not be pulled in — agent.llm reference is optional");
        assert!(!agent.contains("orchestrate_connection"), "orchestrate_connection should not be pulled in transitively");

        // But when model IS explicitly requested, its required connection_id edge
        // still pulls in orchestrate_connection.
        let model = SchemaDependencyGraph::compute_closure_from_kinds(&["model"]);
        assert!(model.contains("model"));
        assert!(model.contains("orchestrate_connection"), "orchestrate_connection should be pulled in via required model.connection_id edge");
    }

    #[test]
    fn test_bridges_fire_when_endpoint_explicitly_requested() {
        // When a bridge endpoint IS explicitly requested, bridges should expand.
        let kinds = &["tool", "knowledge_base", "agent", "orchestrate_connection"];
        let closure = SchemaDependencyGraph::compute_closure_from_kinds(kinds);

        assert!(closure.contains("common_core_connection"), "Bridge should expand when orchestrate_connection is explicit");

        let bridges = closure.find_bridges(&Deployment::Saas);
        assert!(!bridges.is_empty(), "Should find bridges when endpoint is explicit");

        let paths = closure.enumerate_paths(&bridges, kinds);
        assert_eq!(paths.len(), bridges.len(), "One structural path per bridge, got {}", paths.len());
        assert_eq!(paths.iter().filter(|p| p.recommended).count(), 1, "exactly one recommended path");
    }

    #[test]
    fn test_enumerate_paths_marks_added_by_for_bridge_expanded_resources() {
        let input_kinds = &["common_core_connection"];
        let closure = SchemaDependencyGraph::compute_closure_from_kinds(input_kinds);
        let bridges = closure.find_bridges(&Deployment::Saas);
        let paths = closure.enumerate_paths(&bridges, input_kinds);
        assert!(!paths.is_empty());
        for path in &paths {
            let oc = path.resources.iter().find(|r| r.kind == "orchestrate_connection");
            if let Some(r) = oc {
                assert!(r.added_by.is_some(), "orchestrate_connection should have added_by set");
            }
        }
    }

    #[test]
    fn test_deployment_support_labels() {
        // Kinds with no `unsupported_on` are supported everywhere.
        assert_eq!(deployment_support("agent"), vec!["saas", "software"]);
        // The 5 governance kinds re-homed to common_core dropped `unsupported_on`,
        // so they are supported on both saas and software.
        for kind in ["category", "business_term", "business_terms", "rule", "rules"] {
            assert_eq!(deployment_support(kind), vec!["saas", "software"], "{kind} should be supported on saas+software");
        }
        // Unknown kind yields an empty list.
        assert!(deployment_support("does_not_exist").is_empty());
    }

    #[test]
    fn test_find_bridges_respects_deployment_flavor() {
        // No watsonx_orchestrate↔common_core bridge is software-suppressed today,
        // so assert the deployment is threaded by confirming bridge sets are
        // computed per-deployment without panicking and that a software flavor
        // never *adds* bridges relative to saas.
        let kinds = &["orchestrate_connection", "common_core_connection"];
        let closure = SchemaDependencyGraph::compute_closure_from_kinds_with_config(kinds, None, &Deployment::from_str("software-5.3.0").unwrap());
        let saas_closure = SchemaDependencyGraph::compute_closure_from_kinds(kinds);
        let sw_bridges = closure.find_bridges(&Deployment::from_str("software-5.3.0").unwrap());
        let saas_bridges = saas_closure.find_bridges(&Deployment::Saas);
        assert!(sw_bridges.len() <= saas_bridges.len(), "software must not activate more bridges than saas");
    }
}
