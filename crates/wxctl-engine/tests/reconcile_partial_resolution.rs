//! Reconciliation-time PARTIAL ref resolution on the Deferred path (re-apply
//! NoChange fix).
//!
//! Live-proven gap (2026-07-05, SaaS + CP4D): one dependency with no discovered
//! state (an adopt-only kind planning Create) poisoned every transitive
//! downstream resource — deferral skipped template resolution wholesale, so even
//! refs to already-reconciled resources stayed `${...}`-templated,
//! `identity_paths_unresolved` tripped, discovery was skipped, and re-apply
//! blind-POSTed duplicates (`job` 400 "A job with the same name already
//! exists"). The fix resolves refs from the runtime store (topo order
//! guarantees deps reconcile first) before deciding whether discovery can run.
//!
//! Credential-free but NOT network-free: a loopback HTTP stub serves canned
//! list responses so the full list-and-match discovery path runs offline. The
//! profile uses `auth_type: zenapikey` (Software), which packs the token
//! locally — no external token exchange.

use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use wxctl_core::{ClientFactory, ConcurrencyConfig, OnDestroyPolicy, ResourceKey, ResourceRegistry, ValidatedResource};
use wxctl_engine::{OperationType, ReconcileMode, ReconciliationPipeline, RuntimeIdStore, SchemaBasedReconciler};

/// Minimal loopback HTTP/1.1 stub: answers each request with the canned JSON
/// body registered for its path (empty array when unregistered) and records
/// `"METHOD /path?query"` lines so tests can assert which discovery calls ran.
async fn spawn_stub(routes: HashMap<&'static str, Value>, log: Arc<Mutex<Vec<String>>>) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind loopback");
    let addr = listener.local_addr().expect("local addr");
    let routes: HashMap<String, Value> = routes.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else { break };
            let routes = routes.clone();
            let log = log.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 16384];
                let mut read = 0;
                loop {
                    let Ok(n) = sock.read(&mut buf[read..]).await else { return };
                    if n == 0 {
                        break;
                    }
                    read += n;
                    if buf[..read].windows(4).any(|w| w == b"\r\n\r\n") || read == buf.len() {
                        break;
                    }
                }
                let head = String::from_utf8_lossy(&buf[..read]);
                let first = head.lines().next().unwrap_or("");
                let mut parts = first.split_whitespace();
                let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
                log.lock().unwrap().push(format!("{method} {target}"));
                let path = target.split('?').next().unwrap_or("");
                let body = serde_json::to_string(routes.get(path).unwrap_or(&Value::Array(vec![]))).unwrap_or_default();
                let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    format!("http://{addr}")
}

/// Two synthetic list-and-get kinds on service `common_core`: `tpar` (the
/// upstream dependency) and `tchild` (Query-scoped by `${tpar...}`, plus a
/// `ghost_ref` body field referencing a kind that never reconciles — the
/// stand-in for an adopt-only dep with no discovered state).
fn registry() -> Arc<ResourceRegistry> {
    let parent_yaml = r#"
resource:
    name: tpar
    service: common_core
    kind: tpar
    version: v1
    api:
        base_path: /v1/tpars
        id_field: id
        list_endpoint: /v1/tpars
        get_endpoint: /v1/tpars/{id}
        create_method: POST
        delete_method: DELETE
        delete_endpoint: /v1/tpars/{id}
    schema:
        fields:
            - name: name
              type: string
              required: true
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        state_fields:
            - name
        update_strategy: recreate
"#;
    let child_yaml = r#"
resource:
    name: tchild
    service: common_core
    kind: tchild
    version: v1
    api:
        base_path: /v1/tchildren
        id_field: id
        list_endpoint: /v1/tchildren
        get_endpoint: /v1/tchildren/{id}
        create_method: POST
        delete_method: DELETE
        delete_endpoint: /v1/tchildren/{id}
    schema:
        fields:
            - name: name
              type: string
              required: true
            - name: parent_id
              type: string
              required: false
              location: Query
            - name: ghost_ref
              type: string
              required: false
    reconciliation:
        discovery:
            method: list_and_get
            id_source: id
        state_fields:
            - name
            - ghost_ref
        update_strategy: recreate
"#;
    let mut registry = ResourceRegistry::new();
    for yaml in [parent_yaml, child_yaml] {
        let schema = wxctl_schema::ir_support::compile_to_static_ir(yaml).expect("synthetic schema parses");
        registry.register_from_schema(schema, None, |_| Arc::new(SchemaBasedReconciler::new())).expect("register");
    }
    Arc::new(registry)
}

/// Throwaway Software profile whose `common_core` service points at the stub.
/// `zenapikey` packs `username:apikey` locally — no token endpoint is called.
fn factory(base_url: &str) -> Arc<ClientFactory> {
    let tmp = std::env::temp_dir().join(format!("wxctl-partial-res-{}.json", uuid::Uuid::new_v4()));
    std::fs::write(
        &tmp,
        format!(
            r#"{{
  "profiles": {{
    "test-partial-res": {{
      "deployment": "software-5.3.0",
      "common_core": {{ "url": "{base_url}", "deployment": "software-5.3.0", "auth_type": "zenapikey", "username": "alice", "apikey": "KEY-123" }}
    }}
  }}
}}"#
        ),
    )
    .expect("write tmp profile");
    let cc = ConcurrencyConfig::default();
    let f = ClientFactory::new("test-partial-res", Some(tmp.to_str().unwrap()), &cc).expect("factory new");
    let _ = std::fs::remove_file(&tmp);
    Arc::new(f)
}

fn parent_resource(reg: &ResourceRegistry) -> ValidatedResource {
    let descriptor = reg.get_descriptor("tpar").expect("tpar registered").clone();
    ValidatedResource { key: ResourceKey::new("tpar", "p1"), data: json!({"ref_name": "p1", "name": "parent-one"}), descriptor, dependencies: vec![], on_destroy: OnDestroyPolicy::Delete }
}

/// The child defers on `ghost.g1` (never reconciled — no discovered state),
/// while its identity-relevant Query scope `parent_id` references `tpar.p1`.
fn child_resource(reg: &ResourceRegistry) -> ValidatedResource {
    let descriptor = reg.get_descriptor("tchild").expect("tchild registered").clone();
    let data = json!({"ref_name": "c1", "name": "child-one", "parent_id": "${tpar.p1.id}", "ghost_ref": "${ghost.g1.id}"});
    ValidatedResource { key: ResourceKey::new("tchild", "c1"), data, descriptor, dependencies: vec![ResourceKey::new("tpar", "p1"), ResourceKey::new("ghost", "g1")], on_destroy: OnDestroyPolicy::Delete }
}

fn op_for<'a>(plan: &'a wxctl_engine::ReconciliationPlan, kind: &str, name: &str) -> &'a wxctl_engine::Operation {
    plan.operations.iter().find(|op| op.key.kind.as_ref() == kind && op.key.name.as_ref() == name).unwrap_or_else(|| panic!("expected an operation for {kind}.{name}, got: {:?}", plan.operations.iter().map(|o| (o.key.clone(), o.op_type.clone())).collect::<Vec<_>>()))
}

/// (a) From-scratch first apply: NO dependency has discovered state, so partial
/// resolution is an identity transform, the identity-relevant `parent_id` stays
/// templated, discovery is skipped (no HTTP call), and the decision remains the
/// blind Create (rendered `CreateUnchecked`) — byte-identical to pre-fix behavior.
#[tokio::test]
async fn first_apply_dep_undiscovered_keeps_create_unchecked_and_skips_discovery() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let base = spawn_stub(HashMap::new(), log.clone()).await;
    let reg = registry();
    let pipeline = ReconciliationPipeline::new(reg.clone(), factory(&base));
    let store = RuntimeIdStore::new();

    let plan = pipeline.reconcile("test-op-a", vec![child_resource(&reg)], &store, ReconcileMode::Apply, false).await.expect("reconcile runs");

    assert!(plan.errors.is_empty(), "no reconciliation errors expected, got: {:?}", plan.errors);
    assert!(matches!(op_for(&plan, "tchild", "c1").op_type, OperationType::Create), "first apply must still plan Create");
    let requests = log.lock().unwrap().clone();
    assert!(requests.iter().all(|r| !r.contains("/v1/tchildren")), "discovery must be SKIPPED while the identity-relevant parent_id ref is unresolved; requests: {requests:?}");
}

/// (b) Re-apply: the parent is discovered NoOp in the SAME pass, so the child's
/// `${tpar.p1.id}` resolves from the runtime store despite the ghost dep still
/// deferring — discovery runs with the resolved Query scope, matches by name,
/// and the child plans NoChange (the templated `ghost_ref` state field is
/// skipped by compare). Pre-fix this was CreateUnchecked → duplicate POST.
#[tokio::test]
async fn reapply_resolves_refs_from_same_pass_discovery_and_plans_nochange() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let routes = HashMap::from([("/v1/tpars", json!([{"id": "p-1", "name": "parent-one"}])), ("/v1/tchildren", json!([{"id": "c-1", "name": "child-one"}]))]);
    let base = spawn_stub(routes, log.clone()).await;
    let reg = registry();
    let pipeline = ReconciliationPipeline::new(reg.clone(), factory(&base));
    let store = RuntimeIdStore::new();

    let plan = pipeline.reconcile("test-op-b", vec![parent_resource(&reg), child_resource(&reg)], &store, ReconcileMode::Apply, false).await.expect("reconcile runs");

    assert!(plan.errors.is_empty(), "no reconciliation errors expected, got: {:?}", plan.errors);
    assert!(matches!(op_for(&plan, "tpar", "p1").op_type, OperationType::NoOp), "parent must plan NoOp");
    assert!(matches!(op_for(&plan, "tchild", "c1").op_type, OperationType::NoOp), "child must plan NoChange on re-apply even while the ghost dep defers, got: {:?}", op_for(&plan, "tchild", "c1").op_type);

    let requests = log.lock().unwrap().clone();
    assert!(requests.iter().any(|r| r.contains("/v1/tchildren") && r.contains("parent_id=p-1")), "child discovery must run with the parent ref RESOLVED from the same-pass store; requests: {requests:?}");
    assert!(store.contains(&ResourceKey::new("tchild", "c1")), "the discovered child must be cached for downstream refs");
}

/// (c) Mixed: parent discovered, ghost dep still deferring, and the child is
/// genuinely absent remotely — discovery runs (identity refs resolved) and
/// comes back empty, so the decision is a checked Create.
#[tokio::test]
async fn mixed_deps_absent_remote_runs_discovery_and_plans_create() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let routes = HashMap::from([("/v1/tpars", json!([{"id": "p-1", "name": "parent-one"}])), ("/v1/tchildren", json!([]))]);
    let base = spawn_stub(routes, log.clone()).await;
    let reg = registry();
    let pipeline = ReconciliationPipeline::new(reg.clone(), factory(&base));
    let store = RuntimeIdStore::new();

    let plan = pipeline.reconcile("test-op-c", vec![parent_resource(&reg), child_resource(&reg)], &store, ReconcileMode::Apply, false).await.expect("reconcile runs");

    assert!(plan.errors.is_empty(), "no reconciliation errors expected, got: {:?}", plan.errors);
    assert!(matches!(op_for(&plan, "tchild", "c1").op_type, OperationType::Create), "an absent remote must plan Create");
    let requests = log.lock().unwrap().clone();
    assert!(requests.iter().any(|r| r.contains("/v1/tchildren") && r.contains("parent_id=p-1")), "discovery must RUN (resolved identity refs) and observe the absence; requests: {requests:?}");
}
