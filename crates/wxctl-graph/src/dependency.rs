//! Dependency edge types and extraction utilities.

use crate::references::{extract_references_with_path, parse_reference};
use crate::types::{IStr, ResourceKey, istr};

/// Dependency edge from dependent -> dependency with field path tracking.
///
/// The `field_path` records which JSON field caused this dependency,
/// enabling detailed error messages and dependency visualization.
///
/// Uses `IStr` (Arc<str>) for all string fields for zero-cost cloning.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DependencyEdge {
    /// The resource that has the dependency (dependent).
    pub from: ResourceKey,
    /// The resource being depended on (prerequisite).
    pub to: ResourceKey,
    /// The JSON field path where the reference was found (e.g., "connection.catalog_id").
    pub field_path: IStr,
}

/// Extract all dependency edges from a JSON value with field paths.
///
/// Returns `DependencyEdge` structs that include the field path where
/// each reference was found. Uses linear search for deduplication which
/// is faster than HashSet for typical dependency counts (<50).
#[must_use]
#[inline]
pub fn extract_dependency_edges(from: &ResourceKey, value: &serde_json::Value) -> Vec<DependencyEdge> {
    let mut edges = Vec::new();

    extract_references_with_path(value, "", &mut |ref_str, path| {
        if let Some(to) = parse_reference(ref_str) {
            let field_path = istr(path);
            // Linear search is faster than HashSet for small collections (<50 items)
            let exists = edges.iter().any(|e: &DependencyEdge| e.to == to && e.field_path == field_path);
            if !exists {
                edges.push(DependencyEdge { from: from.clone(), to, field_path });
            }
        }
    });

    edges
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::type_complexity)]
    fn test_extract_dependency_edges() {
        let from = ResourceKey::new("asset", "a");
        // (label, json, expected edges as (to_kind, to_name, field_path))
        let cases: &[(&str, serde_json::Value, &[(&str, &str, &str)])] = &[
            // simple top-level ref → one edge, full target key + path captured
            ("simple", serde_json::json!({ "catalog_id": "${catalog.x}" }), &[("catalog", "x", "catalog_id")]),
            // nested object + array element refs → dotted / indexed paths
            ("nested", serde_json::json!({ "top": "${catalog.c1}", "nested": { "inner": "${connection.db}" }, "items": ["${asset.foo}"] }), &[("catalog", "c1", "top"), ("connection", "db", "nested.inner"), ("asset", "foo", "items[0]")]),
            // no refs → no edges
            ("no_refs", serde_json::json!({ "name": "plain", "count": 42 }), &[]),
            // same target via two distinct paths → two edges (path-keyed, not deduped by target)
            ("same_target_diff_paths", serde_json::json!({ "field1": "${catalog.x}", "field2": "${catalog.x}" }), &[("catalog", "x", "field1"), ("catalog", "x", "field2")]),
        ];
        for (label, json, expected) in cases {
            let edges = extract_dependency_edges(&from, json);
            assert_eq!(edges.len(), expected.len(), "edge count for {label}");
            // `from` is preserved on every edge.
            assert!(edges.iter().all(|e| &*e.from.kind == "asset" && &*e.from.name == "a"), "from key for {label}");
            for (to_kind, to_name, field_path) in *expected {
                assert!(edges.iter().any(|e| &*e.to.kind == *to_kind && &*e.to.name == *to_name && &*e.field_path == *field_path), "missing edge ({to_kind}.{to_name}, {field_path}) for {label}");
            }
        }
    }
}
