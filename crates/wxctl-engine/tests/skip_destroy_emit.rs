//! Repro for glossary common-core lifecycle idempotency (destroy-emit of Skip kinds).
//!
//! A `DiscoveryMethod::Skip` kind in `Destroy` mode must emit its optimistic
//! `Delete` op even when a forward/nested `${...}` ref can't resolve at
//! destroy-plan time — so the handler's `pre_delete` (name-only contract) runs
//! and the resource isn't silently dropped (the live `rules_bulk` leak, Gap A).
//!
//! Credential-free: skip-discovery's `discover_all` returns empty WITHOUT any
//! HTTP call (schema_reconciler.rs DiscoveryMethod::Skip arm), so `reconcile`
//! never touches the network. The `ClientFactory` is built from a throwaway
//! temp profile purely so `get_or_create_client("common_core")` succeeds.

use std::sync::Arc;
use wxctl_core::{ClientFactory, ConcurrencyConfig, OnDestroyPolicy, ResourceKey, ResourceRegistry, ValidatedResource};
use wxctl_engine::{OperationType, ReconcileMode, ReconciliationPipeline, RuntimeIdStore, SchemaBasedReconciler};

/// Build a fully-populated registry from the compiled schema set, offline.
fn registry() -> Arc<ResourceRegistry> {
    let mut registry = ResourceRegistry::new();
    for schema in wxctl_providers::load_all_schemas().expect("schemas parse") {
        let handler = wxctl_providers::get_handler(&schema.resource.name);
        registry.register_from_schema(schema, handler, |_| Arc::new(SchemaBasedReconciler::new())).expect("register");
    }
    Arc::new(registry)
}

/// A throwaway SaaS profile with a `common_core` service block, so
/// `create_client("common_core")` succeeds offline (no token fetch happens —
/// skip-discovery makes no request).
fn factory() -> Arc<ClientFactory> {
    let tmp = std::env::temp_dir().join(format!("wxctl-skip-destroy-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(
        &tmp,
        r#"{
  "profiles": {
    "test-skip-destroy": {
      "deployment": "saas",
      "common_core": { "url": "https://example.invalid", "deployment": "saas", "apikey": "KEY-123" }
    }
  }
}"#,
    )
    .expect("write tmp profile");
    let cc = ConcurrencyConfig::default();
    let f = ClientFactory::new("test-skip-destroy", Some(tmp.to_str().unwrap()), &cc).expect("factory new");
    let _ = std::fs::remove_file(&tmp);
    Arc::new(f)
}

/// Build a `ValidatedResource` for the skip-discovery `rules` kind whose
/// `rules[].trigger` references a `business_term` that is NOT in the runtime
/// cache. The declared `dependencies` entry makes `check_dependencies` return
/// the missing dep, routing the resource into the `Deferred` reconcile arm —
/// the exact destroy-plan branch the live `rules_bulk` leak hits.
fn rules_resource(reg: &ResourceRegistry) -> ValidatedResource {
    let descriptor = reg.get_descriptor("rules").expect("rules kind registered (common_core)").clone();
    let data = serde_json::json!({
        "ref_name": "rules_bulk",
        "rules": [{
            "name": "e2e-pii-access",
            "trigger": ["$Asset.InferredClassification", "CONTAINS", ["${business_term.term_email.artifact_id}"]],
            "action": { "name": "Deny" }
        }]
    });
    ValidatedResource { key: ResourceKey::new("rules", "rules_bulk"), data, descriptor, dependencies: vec![ResourceKey::new("business_term", "term_email")], on_destroy: OnDestroyPolicy::Delete }
}

#[tokio::test]
async fn skip_discovery_destroy_emits_delete_despite_unresolvable_ref() {
    let reg = registry();
    let pipeline = ReconciliationPipeline::new(reg.clone(), factory());
    let cache = RuntimeIdStore::new();

    let resource = rules_resource(&reg);
    let plan = pipeline.reconcile("test-op", vec![resource], &cache, ReconcileMode::Destroy, false).await.expect("reconcile runs offline");

    assert!(plan.errors.is_empty(), "no reconciliation errors expected, got: {:?}", plan.errors);

    let rules_ops: Vec<&OperationType> = plan.operations.iter().filter(|op| op.key.kind.as_ref() == "rules" && op.key.name.as_ref() == "rules_bulk").map(|op| &op.op_type).collect();

    assert_eq!(rules_ops.len(), 1, "expected exactly one op for the skip-discovery rules resource, got: {rules_ops:?}");
    assert!(matches!(rules_ops[0], OperationType::Delete), "skip-discovery + Destroy + unresolvable ref must emit Delete (Gap A), got: {:?}", rules_ops[0]);
}
