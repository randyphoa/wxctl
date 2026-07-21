//! End-to-end gate for the `depends_on` ordering escape hatch.
//!
//! Credential-free: builds a real ResourceRegistry from the compiled schema set
//! and runs the engine ValidationPipeline with `client_factory = None`, then
//! asserts on the built ResourceSet's topological order (the exact graph the
//! DagExecutor consumes). No mock executor, no profile, no env mutation.

use std::sync::Arc;
use wxctl_core::{RawResource, ResourceKey, ResourceRegistry};
use wxctl_engine::{SchemaBasedReconciler, ValidationPipeline, ValidationResult};

/// Build a fully-populated registry from the compiled schema set, offline.
fn registry() -> Arc<ResourceRegistry> {
    let mut registry = ResourceRegistry::new();
    for schema in wxctl_schema::ir::RESOURCE_IR.values().copied() {
        let handler = wxctl_providers::get_handler(schema.resource.name);
        registry.register_from_schema(schema, handler, |_| Arc::new(SchemaBasedReconciler::new())).expect("register");
    }
    Arc::new(registry)
}

fn space(ref_name: &str, depends_on: &[&str]) -> RawResource {
    let mut data = serde_json::json!({ "ref_name": ref_name, "name": format!("{}-space-name", ref_name) });
    if !depends_on.is_empty() {
        data.as_object_mut().unwrap().insert("depends_on".to_string(), serde_json::json!(depends_on));
    }
    RawResource { kind: "space".into(), data }
}

// `category` is self-referential: its `parent_category` field declares
// `references: { resource: category, field: artifact_id }` (schemas/watsonx_data/category.yaml),
// and only `name` is required. So one category can carry an explicit
// `depends_on` edge to another while the other carries a schema-allowed
// `${category.<ref>.artifact_id}` reference edge back — a cycle that spans
// BOTH edge sources (the spec's central claim). `name` matches the field
// pattern `^[a-zA-Z0-9_\s-]+$`.
fn category(ref_name: &str, depends_on: &[&str], parent_category: Option<&str>) -> RawResource {
    let mut data = serde_json::json!({ "ref_name": ref_name, "name": format!("{}-category", ref_name) });
    let obj = data.as_object_mut().unwrap();
    if !depends_on.is_empty() {
        obj.insert("depends_on".to_string(), serde_json::json!(depends_on));
    }
    if let Some(parent) = parent_category {
        obj.insert("parent_category".to_string(), serde_json::json!(parent));
    }
    RawResource { kind: "category".into(), data }
}

/// Run the offline engine validation pipeline (no client_factory).
/// `skip_post_validate = true` since there is no client factory available
/// to run post-validate hooks (e.g. source_path hashing).
async fn validate(resources: &mut [RawResource]) -> ValidationResult {
    ValidationPipeline::new(registry(), None).validate("test-op", resources, true).await.expect("pipeline runs")
}

// AC1 + AC2 — apply orders prerequisite before dependent; destroy is the reverse.
#[tokio::test]
async fn ac1_ac2_ordering_and_destroy_reverse() {
    // b depends_on a, with NO ${a} value anywhere. Declare the DEPENDENT (b)
    // FIRST so insertion order is the OPPOSITE of the required apply order: a
    // topological sort that ignored the `depends_on` edge would return insertion
    // order [b, a] and fail the `pos_a < pos_b` assert below. The test therefore
    // strictly binds AC1 — it only passes because the edge reorders a before b.
    let mut resources = vec![space("b", &["space.a"]), space("a", &[])];
    let result = validate(&mut resources).await;
    let set = result.take_resource_set().expect("validation succeeds");

    let order = set.topological_order();
    let pos_a = order.iter().position(|&i| i == set.index_of(&ResourceKey::new("space", "a")).unwrap()).unwrap();
    let pos_b = order.iter().position(|&i| i == set.index_of(&ResourceKey::new("space", "b")).unwrap()).unwrap();
    // AC1: apply order — prerequisite a before dependent b.
    assert!(pos_a < pos_b, "AC1: expected space.a before space.b in apply order, got {:?}", order);
    // AC2: destroy order is apply reversed — dependent b before prerequisite a.
    let mut destroy = order.clone();
    destroy.reverse();
    let d_a = destroy.iter().position(|&i| i == set.index_of(&ResourceKey::new("space", "a")).unwrap()).unwrap();
    let d_b = destroy.iter().position(|&i| i == set.index_of(&ResourceKey::new("space", "b")).unwrap()).unwrap();
    assert!(d_b < d_a, "AC2: expected space.b before space.a in destroy order, got {:?}", destroy);
}

// AC3 + I2 — depends_on injects nothing: ValidatedResource.data is byte-identical
// with and without the directive and never carries a `depends_on` key.
#[tokio::test]
async fn ac3_i2_no_value_injected() {
    let mut with = vec![space("a", &[]), space("b", &["space.a"])];
    let mut without = vec![space("a", &[]), space("b", &[])];
    let set_with = validate(&mut with).await.take_resource_set().expect("valid");
    let set_without = validate(&mut without).await.take_resource_set().expect("valid");

    let b_with = set_with.get_by_key(&ResourceKey::new("space", "b")).unwrap();
    let b_without = set_without.get_by_key(&ResourceKey::new("space", "b")).unwrap();
    // AC3: no `depends_on` key reaches the validated data (→ never on the wire).
    assert!(b_with.data.get("depends_on").is_none(), "AC3: depends_on must be stripped from ValidatedResource.data");
    // AC3 + I2: data identical with vs without the directive → no state, no diff.
    assert_eq!(b_with.data, b_without.data, "AC3/I2: depends_on must not change the resource's data");
}

// AC4 (dangling) — a target absent from the config fails validation, naming the entry.
#[tokio::test]
async fn ac4_dangling_target_fails() {
    let mut resources = vec![space("b", &["space.ghost"])];
    let result = validate(&mut resources).await;
    // Collect owned messages BEFORE consuming `result` with take_resource_set
    // (errors() borrows; the borrow must not outlive the move).
    let msgs: Vec<String> = result.errors().iter().map(|e| e.error.to_string()).collect();
    assert!(msgs.iter().any(|m| m.contains("space.ghost")), "AC4: error must name the missing target space.ghost; got {:?}", msgs);
    assert!(result.take_resource_set().is_none(), "AC4: validation must fail (no resource set)");
}

// AC4 (malformed) — a non-`kind.ref_name` entry fails validation, naming the entry.
#[tokio::test]
async fn ac4_malformed_entry_fails() {
    let mut resources = vec![space("b", &["not_a_pair"])];
    let result = validate(&mut resources).await;
    assert!(result.errors().iter().any(|e| e.error.to_string().contains("not_a_pair")), "AC4: error must name the malformed entry");
}

// AC5 — a resource listing itself fails with a self-dependency error.
#[tokio::test]
async fn ac5_self_dependency_fails() {
    let mut resources = vec![space("b", &["space.b"])];
    let result = validate(&mut resources).await;
    assert!(result.errors().iter().any(|e| e.error.to_string().contains("lists itself")), "AC5: error must report a self-dependency; got {:?}", result.errors().iter().map(|e| e.error.to_string()).collect::<Vec<_>>());
}

// AC6 (mixed provenance — the spec's illustrated case: "A depends_on B while B
// references A"). category.a declares an EXPLICIT depends_on edge to category.b,
// while category.b carries a schema-allowed REFERENCE edge back to category.a via
// its `parent_category` field. The loop therefore spans one explicit edge and one
// reference-derived edge — proving the merged graph detects cycles regardless of
// edge provenance ("single graph, two edge sources, identical downstream").
#[tokio::test]
async fn ac6_mixed_provenance_cycle_fails_naming_path() {
    // a --(depends_on, explicit)--> b ; b --(${category.a.artifact_id}, reference)--> a
    let mut resources = vec![category("a", &["category.b"], None), category("b", &[], Some("${category.a.artifact_id}"))];
    let result = validate(&mut resources).await;
    let msgs: Vec<String> = result.errors().iter().map(|e| e.error.to_string()).collect();
    // The reference must NOT be rejected as an invalid dependency — category
    // legitimately references category, so it forms a real edge and the failure
    // must be the cycle, not a V005 InvalidDependency.
    assert!(msgs.iter().all(|m| !m.contains("Invalid dependency")), "AC6: ${{category.a}} is schema-allowed and must form a real edge, not a V005 InvalidDependency; got {:?}", msgs);
    assert!(msgs.iter().any(|m| m.contains("Circular dependency")), "AC6: expected a circular-dependency error across mixed edge types; got {:?}", msgs);
    assert!(msgs.iter().any(|m| m.contains("category.a") && m.contains("category.b")), "AC6: error must name the cycle path (category.a, category.b); got {:?}", msgs);
    assert!(result.take_resource_set().is_none(), "AC6: a cycle must fail validation (no resource set)");
}

// AC6 (pure depends_on loop — minimal literal-wording case, additive). Two
// explicit edges; reaches the same find_cycle path. Kept alongside the mixed
// form so AC6's binding covers both the spec's illustrated case and the
// degenerate pure case.
#[tokio::test]
async fn ac6_pure_depends_on_cycle_fails_naming_path() {
    // a depends_on b AND b depends_on a → cycle (a depends_on directive loop).
    let mut resources = vec![space("a", &["space.b"]), space("b", &["space.a"])];
    let result = validate(&mut resources).await;
    let msgs: Vec<String> = result.errors().iter().map(|e| e.error.to_string()).collect();
    assert!(msgs.iter().any(|m| m.contains("Circular dependency")), "AC6: expected a circular-dependency error; got {:?}", msgs);
    assert!(msgs.iter().any(|m| m.contains("space.a") && m.contains("space.b")), "AC6: error must name the cycle path (space.a, space.b); got {:?}", msgs);
}

// AC8 — depends_on is accepted on a kind with no per-schema declaration and is
// never flagged as an unknown field (engine surface; complements the Phase 1
// schema-layer test on kind `test`).
#[tokio::test]
async fn ac8_accepted_on_arbitrary_kind() {
    let mut resources = vec![space("a", &[]), space("b", &["space.a"])];
    let result = validate(&mut resources).await;
    // Collect owned messages BEFORE consuming `result` with take_resource_set.
    let msgs: Vec<String> = result.errors().iter().map(|e| e.error.to_string()).collect();
    assert!(msgs.iter().all(|m| !m.to_lowercase().contains("unknown field")), "AC8: depends_on must never be reported as an unknown field; got {:?}", msgs);
    assert!(result.take_resource_set().is_some(), "AC8: a valid depends_on must validate cleanly");
}
