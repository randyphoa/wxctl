//! Phase 4 engine surface (spec 2026-07-11-validate-linkage-diagnostics): the shared
//! ValidationPipeline emits the dangling-ref V005 with its transitive chain, and the
//! validate-surface bridge_advisories producer is orphan-gated. Credential-free:
//! real registry from the compiled schema set, client_factory = None, no network.
//!
//! Harness mirrors `depends_on_e2e.rs`: a fully-populated `ResourceRegistry` built
//! from `wxctl_providers::load_all_schemas` + `get_handler`, driven through the real
//! `ValidationPipeline` with `client_factory = None` and `skip_post_validate = true`
//! (no client factory available to run post-validate hooks offline).

use std::str::FromStr;
use std::sync::Arc;
use wxctl_core::{RawResource, ResourceRegistry};
use wxctl_engine::{SchemaBasedReconciler, ValidationPipeline, ValidationResult, bridge_advisories};
use wxctl_schema::deployment::Deployment;

/// Build a fully-populated registry from the compiled schema set, offline.
fn registry() -> Arc<ResourceRegistry> {
    let mut registry = ResourceRegistry::new();
    for schema in wxctl_providers::load_all_schemas().expect("schemas parse") {
        let handler = wxctl_providers::get_handler(&schema.resource.name);
        registry.register_from_schema(schema, handler, |_| Arc::new(SchemaBasedReconciler::new())).expect("register");
    }
    Arc::new(registry)
}

/// Run the offline engine validation pipeline (no client_factory).
async fn validate(resources: &mut [RawResource]) -> ValidationResult {
    ValidationPipeline::new(registry(), None).validate("test-op", resources, true).await.expect("pipeline runs")
}

fn res(kind: &str, data: serde_json::Value) -> RawResource {
    RawResource { kind: kind.into(), data }
}

fn agent(ref_name: &str) -> serde_json::Value {
    serde_json::json!({ "ref_name": ref_name, "name": ref_name, "display_name": ref_name, "description": "d", "instructions": "i", "llm": "groq/openai/gpt-oss-120b", "style": "default" })
}

fn ccc(ref_name: &str) -> serde_json::Value {
    serde_json::json!({ "ref_name": ref_name, "name": ref_name, "datasource_type": "postgres", "properties": { "host": "h" } })
}

// AC1 (+AC5 chain) — the pipeline emits V005 for a dangling reference, with the chain.
#[tokio::test]
async fn pipeline_emits_v005_with_chain_for_dangling_ref() {
    let mut resources = vec![res(
        "model_tracking",
        serde_json::json!({
            "ref_name": "mt", "model": "${wml_model.absent}", "model_entry": "entry-literal",
            "model_entry_catalog_id": "cat123", "space_id": "space-literal"
        }),
    )];
    let result = validate(&mut resources).await;
    assert!(!result.is_valid(), "dangling ref must fail validation");
    let v005 = result.errors().iter().find(|e| e.error.to_string().contains("WXCTL-V005")).expect("a V005 error");
    let s = v005.error.suggestion();
    assert!(s.contains("`wml_model` resource with `ref_name: absent`"), "add-resource element: {s}");
    assert!(s.contains("`autoai_experiment` (referenced by `wml_model.experiment`)"), "chain hop 1: {s}");
    assert!(s.contains("`data_asset` (referenced by `autoai_experiment.training_data`)"), "chain hop 2: {s}");
}

// AC6 — orphan common_core_connection yields two V505 advisories naming orchestrate_connection.
#[tokio::test]
async fn bridge_advisories_orphan_ccc_two_v505() {
    let mut resources = vec![res("agent", agent("a6")), res("common_core_connection", ccc("db6"))];
    let result = validate(&mut resources).await;
    assert!(result.is_valid(), "orphan ccc config must be valid (advisories never change valid)");
    let adv = bridge_advisories(&result, None);
    assert_eq!(adv.len(), 2, "expected two V505 advisories, got {:?}", adv);
    for a in &adv {
        assert_eq!(a.code, "WXCTL-V505");
        assert_eq!(a.resource, "common_core_connection/db6");
        assert!(a.message.contains("orchestrate_connection"), "advisory must name the missing counterpart: {}", a.message);
    }
}

// AC7 — the same ccc referenced by another resource (depends_on) yields no advisory.
#[tokio::test]
async fn bridge_advisories_none_when_ccc_depended_upon() {
    let mut a = agent("a7");
    a.as_object_mut().unwrap().insert("depends_on".into(), serde_json::json!(["common_core_connection.db7"]));
    let mut resources = vec![res("common_core_connection", ccc("db7")), res("agent", a)];
    let result = validate(&mut resources).await;
    assert!(result.is_valid());
    assert!(bridge_advisories(&result, None).is_empty(), "a non-orphan ccc must produce no advisory");
}

// AC8 — deployment is threaded into the advisory computation.
#[tokio::test]
async fn bridge_advisories_thread_deployment() {
    let mut resources = vec![res("agent", agent("a8")), res("common_core_connection", ccc("db8"))];
    let result = validate(&mut resources).await;
    let none = bridge_advisories(&result, None).len();
    let saas = bridge_advisories(&result, Some(&Deployment::Saas)).len();
    let sw = bridge_advisories(&result, Some(&Deployment::from_str("software-5.3.0").unwrap())).len();
    assert_eq!(none, saas, "conservative default must match saas for always-active bridges");
    assert!(sw <= saas, "software must not add advisories beyond saas");
    assert_eq!(none, 2);
}
