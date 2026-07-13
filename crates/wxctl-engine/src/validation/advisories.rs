//! The V505 bridge advisory producer. Warn-level, orphan-gated, validate-surface
//! only: a config that carries exactly one endpoint kind of a cross-service bridge
//! while the counterpart kind is absent, and whose anchoring resource is an orphan
//! (no reference or depends_on edges to or from any other config resource), earns a
//! non-blocking advisory pointing at the missing counterpart.
//!
//! This runs only where a validate surface assembles its result (CLI `validate`,
//! local MCP `wxctl_validate`), never inside the shared `ValidationPipeline`, so
//! plan/apply/destroy never emit advisories (spec I2).

use std::collections::HashSet;
use wxctl_core::ResourceKey;
use wxctl_schema::dependency_graph::orphan_bridge_opportunities;
use wxctl_schema::deployment::Deployment;

use super::types::{ValidationAdvisory, ValidationResult};

/// Scan a validated config for orphaned one-sided bridges and return one V505
/// advisory per (bridge, orphan resource). Empty on an invalid result (no resource
/// graph to analyze) or when no orphan anchors a one-sided bridge.
///
/// `deployment`: `Some(d)` uses `d` for bridge activation; `None` is the conservative
/// default (only bridges active on every deployment flavor).
pub fn bridge_advisories(result: &ValidationResult, deployment: Option<&Deployment>) -> Vec<ValidationAdvisory> {
    let resources = result.resources();
    if resources.is_empty() {
        return Vec::new();
    }

    let mut present_kinds: Vec<&str> = resources.iter().map(|r| r.key.kind.as_ref()).collect();
    present_kinds.sort_unstable();
    present_kinds.dedup();

    let opportunities = orphan_bridge_opportunities(&present_kinds, deployment);
    if opportunities.is_empty() {
        return Vec::new();
    }

    // A resource is depended-upon when it appears in some other resource's
    // dependencies (reference edges and depends_on are merged into `dependencies`
    // during validation).
    let depended_upon: HashSet<ResourceKey> = resources.iter().flat_map(|r| r.dependencies.iter().cloned()).collect();

    let mut advisories = Vec::new();
    // Dedup so one bridge never fires twice for the same anchoring resource (e.g. a
    // bridge that matches the present kind through two constraints).
    let mut seen: HashSet<(&'static str, ResourceKey)> = HashSet::new();
    for r in resources {
        let is_orphan = r.dependencies.is_empty() && !depended_upon.contains(&r.key);
        if !is_orphan {
            continue;
        }
        let kind = r.key.kind.as_ref();
        for opp in opportunities.iter().filter(|o| o.present_kind == kind) {
            if !seen.insert((opp.bridge_name, r.key.clone())) {
                continue;
            }
            let resource = format!("{}/{}", r.key.kind, r.key.name);
            let mappings = if opp.field_mappings.is_empty() {
                String::new()
            } else {
                let rendered: Vec<String> = opp.field_mappings.iter().map(|(sf, tf)| format!("{}.{} to {}.{}", opp.source_kind, sf, opp.target_kind, tf)).collect();
                format!(" (field mappings: {})", rendered.join(", "))
            };
            let message = format!("orphan resource: the '{}' bridge links '{}' to '{}', which is absent from this config{}", opp.bridge_name, opp.present_kind, opp.missing_kind, mappings);
            let suggestion = format!("Add a '{}' resource and wire it to this '{}', or run `wxctl compose paths` for the full linkage. If '{}' is intentionally standalone, ignore this advisory.", opp.missing_kind, opp.present_kind, opp.present_kind);
            advisories.push(ValidationAdvisory { code: wxctl_core::logging::error_codes::V505.to_string(), resource, message, suggestion });
        }
    }
    advisories
}
