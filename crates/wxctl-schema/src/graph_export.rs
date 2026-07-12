//! Pure-compute serializer for the compiled kind graph.
//!
//! Walks the `build.rs`-generated `RESOURCES` / `RESOURCE_EDGES` / `BRIDGES`
//! tables into a versioned, stably-ordered [`GraphDoc`]. Wasm-safe: no fs,
//! tokio, or network. Serialization to JSON is the caller's job (the private
//! `wxctl-graph-tools` bin).

use crate::dependency_graph::{BRIDGES, RESOURCE_EDGES, RESOURCES, UNSUPPORTED_ON, resource_catalog};
use serde::Serialize;
use std::collections::BTreeMap;

/// Export document schema version. Consumers assert this equals `1`.
pub const GRAPH_FORMAT_VERSION: u32 = 1;

/// The full serialized kind graph.
#[derive(Debug, Clone, Serialize)]
pub struct GraphDoc {
    pub format_version: u32,
    pub nodes: Vec<NodeRecord>,
    pub edges: Vec<EdgeRecord>,
    pub bridges: Vec<BridgeRecord>,
    /// Present only when a library dir is merged in (Phase 2). Omitted otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recipes: Option<Vec<RecipeRecord>>,
}

/// One resource kind (graph node).
#[derive(Debug, Clone, Serialize)]
pub struct NodeRecord {
    pub kind: String,
    pub service: String,
    pub topo_order: usize,
    pub unsupported_on: Vec<String>,
}

/// One reference (or containment) edge.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeRecord {
    pub from: String,
    pub to: String,
    pub field: String,
    /// `"reference"` or `"containment"`.
    pub mechanism: String,
    /// Derived requiredness, kept for compatibility (the viz badges key off this).
    pub required: bool,
    pub field_required: bool,
    pub ref_optional: bool,
    pub union_secondary: bool,
    pub require_ready: bool,
}

/// One cross-service bridge from `linkages.yaml`.
#[derive(Debug, Clone, Serialize)]
pub struct BridgeRecord {
    pub name: String,
    pub from: String,
    pub to: String,
    /// Always `"bridge"`.
    pub mechanism: String,
    /// `{ kind: { field: [allowed values] } }`.
    pub constraints: BTreeMap<String, BTreeMap<String, Vec<String>>>,
    pub field_mapping: Vec<FieldMap>,
    /// Deployment scope (`when.deployment`); `None` when always active.
    pub when: Option<String>,
}

/// One bridge field mapping.
#[derive(Debug, Clone, Serialize)]
pub struct FieldMap {
    pub source: String,
    pub target: String,
}

/// One recipe node (library block). Shared vocabulary — defined here, populated
/// only by the private bin from Phase 2 onward. Unused in Phase 1.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeRecord {
    pub name: String,
    pub path: String,
    pub provides: Vec<String>,
    pub requires: RecipeRequires,
    pub instantiates: Vec<String>,
    pub binding_app_ids: Vec<String>,
}

/// A recipe's `requires` contract: reference targets and env vars.
#[derive(Debug, Clone, Serialize)]
pub struct RecipeRequires {
    pub refs: Vec<String>,
    pub env: Vec<String>,
}

/// Serialize the compiled kind graph into a stably-ordered [`GraphDoc`].
///
/// Ordering (byte-identical re-export): nodes by `kind`, edges by
/// `(from, field, to)`, bridges by `name`; bridge constraints via `BTreeMap`.
/// Service is looked up from `RESOURCE_CATALOG` (kind == name for every schema).
pub fn export_graph() -> GraphDoc {
    let svc: BTreeMap<&str, &str> = resource_catalog().iter().map(|&(kind, service, _)| (kind, service)).collect();

    let mut nodes: Vec<NodeRecord> = (0..RESOURCES.len())
        .map(|i| {
            let name = RESOURCES[i].0;
            let raw = UNSUPPORTED_ON[i];
            let unsupported_on = if raw.is_empty() { Vec::new() } else { raw.split(", ").map(str::to_string).collect() };
            NodeRecord { kind: name.to_string(), service: svc.get(name).copied().unwrap_or("").to_string(), topo_order: i, unsupported_on }
        })
        .collect();
    nodes.sort_by(|a, b| a.kind.cmp(&b.kind));

    let mut edges: Vec<EdgeRecord> = Vec::new();
    for i in 0..RESOURCES.len() {
        let from = RESOURCES[i].0;
        for &(field, tidx, required, field_required, ref_optional, union_secondary, require_ready, containment) in RESOURCE_EDGES[i] {
            edges.push(EdgeRecord { from: from.to_string(), to: RESOURCES[tidx].0.to_string(), field: field.to_string(), mechanism: if containment { "containment" } else { "reference" }.to_string(), required, field_required, ref_optional, union_secondary, require_ready });
        }
    }
    edges.sort_by(|a, b| (&a.from, &a.field, &a.to).cmp(&(&b.from, &b.field, &b.to)));

    let mut bridges: Vec<BridgeRecord> = BRIDGES
        .iter()
        .map(|&(name, sidx, tidx, constraints, mappings, when)| {
            let mut cmap: BTreeMap<String, BTreeMap<String, Vec<String>>> = BTreeMap::new();
            for &(ridx, field, vals) in constraints {
                cmap.entry(RESOURCES[ridx].0.to_string()).or_default().insert(field.to_string(), vals.iter().map(|s| s.to_string()).collect());
            }
            BridgeRecord {
                name: name.to_string(),
                from: RESOURCES[sidx].0.to_string(),
                to: RESOURCES[tidx].0.to_string(),
                mechanism: "bridge".to_string(),
                constraints: cmap,
                field_mapping: mappings.iter().map(|&(source, target)| FieldMap { source: source.to_string(), target: target.to_string() }).collect(),
                when: if when.is_empty() { None } else { Some(when.to_string()) },
            }
        })
        .collect();
    bridges.sort_by(|a, b| a.name.cmp(&b.name));

    GraphDoc { format_version: GRAPH_FORMAT_VERSION, nodes, edges, bridges, recipes: None }
}
