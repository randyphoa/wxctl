//! Scripted MCP client smoke test for the four diagnose tools (runs_list, run_get,
//! run_events_query, run_diagnose). In-process rmcp client + server over a duplex
//! pair (compose_smoke.rs pattern — no child process, no profile, no network: the
//! diagnose tools are pure reads of the artifact tree). Seeds a failed run under a
//! temp WXCTL_RUNS_DIR first, then drives the tools end-to-end. Reaching serve
//! also proves every output schema is object-rooted (rmcp panics at router build
//! otherwise — mcp-server-startup-fix.md).

use rmcp::model::{CallToolRequestParams, ClientInfo};
use rmcp::{ServiceExt, serve_client};
use serde_json::json;
use wxctl_core::logging::run_record::{ManifestError, RunCounts, RunManifest, RunSink, generate_run_id, utc_now_string};
use wxctl_mcp::WxctlMcpServer;

async fn connect() -> rmcp::service::RunningService<rmcp::RoleClient, ClientInfo> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = WxctlMcpServer::new("nonexistent-profile-for-smoke", None, false, false);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server serve");
        let _ = running.waiting().await;
    });
    serve_client(ClientInfo::default(), client_io).await.expect("client serve")
}

fn seed_failed_run(runs_dir: &std::path::Path) -> String {
    unsafe { std::env::set_var("WXCTL_RUNS_DIR", runs_dir) };
    let run_id = generate_run_id("apply");
    let manifest = RunManifest {
        run_id: run_id.clone(),
        command: "apply".into(),
        args: vec!["mcp:apply".into()],
        profile: Some("smoke".into()),
        deployment: None,
        config_paths: vec!["inline".into()],
        started: utc_now_string(),
        finished: None,
        outcome: None,
        counts: RunCounts::default(),
        errors: vec![],
        full_trace: false,
        record_incomplete: false,
    };
    let sink = RunSink::new(manifest).unwrap();
    sink.write_event(r#"{"ts":"t","level":"INFO","target":"wxctl::decision","span":"run>reconciliation","resource_type":"space","resource_name":"dev","decision":"create","reason":"absent"}"#);
    sink.write_event(r#"{"ts":"t","level":"ERROR","target":"wxctl::error","span":"run>execution","src":"crates/x.rs:1","error_code":"WXCTL-H001","resource_type":"space","resource_name":"dev","message":"HTTP 404 not found","fix":"check the instance guid","context":"{\"request_body\":{\"name\":\"dev\"},\"response_body\":{\"error\":\"not found\"}}"}"#);
    sink.add_error(ManifestError { code: "WXCTL-H001".into(), resource: Some("space.dev".into()), message: "HTTP 404 not found".into(), fix: Some("check the instance guid".into()) });
    sink.finalize("failed");
    drop(sink);
    run_id
}

#[tokio::test]
async fn diagnose_tools_smoke() {
    // Seed under a unique temp dir (integration test runs in its own process, so the env var is safe).
    let tmp = tempfile::tempdir().unwrap();
    let run_id = seed_failed_run(tmp.path());

    let client = connect().await;

    // 1. tools/list contains all four diagnose tools.
    let tools = client.list_all_tools().await.expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for n in ["runs_list", "run_get", "run_events_query", "run_diagnose"] {
        assert!(names.contains(&n), "missing tool {n}; got {names:?}");
    }

    // 2. runs_list {} → the seeded run with outcome=failed is present.
    let r = client.call_tool(CallToolRequestParams::new("runs_list").with_arguments(json!({}).as_object().cloned().unwrap())).await.expect("runs_list");
    assert_ne!(r.is_error, Some(true), "runs_list errored: {r:?}");
    let runs = r.structured_content.as_ref().and_then(|v| v.get("runs")).and_then(|v| v.as_array()).expect("runs array");
    let seeded = runs.iter().find(|row| row.get("run_id").and_then(|v| v.as_str()) == Some(run_id.as_str())).expect("seeded run not found in runs_list");
    assert_eq!(seeded.get("outcome").and_then(|v| v.as_str()), Some("failed"), "outcome mismatch: {seeded:?}");

    // 3. run_get {run_id} → manifest: command=="apply", errors len 1.
    let r = client.call_tool(CallToolRequestParams::new("run_get").with_arguments(json!({"run_id": run_id}).as_object().cloned().unwrap())).await.expect("run_get");
    assert_ne!(r.is_error, Some(true), "run_get errored: {r:?}");
    let manifest = r.structured_content.as_ref().expect("structured_content");
    assert_eq!(manifest.get("command").and_then(|v| v.as_str()), Some("apply"), "command mismatch: {manifest:?}");
    let errors = manifest.get("errors").and_then(|v| v.as_array()).expect("errors array");
    assert_eq!(errors.len(), 1, "expected 1 error, got {}: {manifest:?}", errors.len());

    // 4. run_events_query {run_id, level:"ERROR"} → matched==1, events[0].fields.error_code=="WXCTL-H001".
    let r = client.call_tool(CallToolRequestParams::new("run_events_query").with_arguments(json!({"run_id": run_id, "level": "ERROR"}).as_object().cloned().unwrap())).await.expect("run_events_query");
    assert_ne!(r.is_error, Some(true), "run_events_query errored: {r:?}");
    let sc = r.structured_content.as_ref().expect("structured_content");
    let matched = sc.get("matched").and_then(|v| v.as_u64()).expect("matched field");
    assert_eq!(matched, 1, "expected matched==1, got {matched}: {sc:?}");
    let events = sc.get("events").and_then(|v| v.as_array()).expect("events array");
    let error_code = events[0].get("fields").and_then(|f| f.get("error_code")).and_then(|v| v.as_str()).expect("error_code in fields");
    assert_eq!(error_code, "WXCTL-H001", "wrong error_code: {error_code}");

    // 5. run_diagnose {} (no run_id → auto-selects latest failed) → bundle.run_id==run_id, errors[0].error_code=="WXCTL-H001", fix non-empty, triage present.
    let r = client.call_tool(CallToolRequestParams::new("run_diagnose").with_arguments(json!({}).as_object().cloned().unwrap())).await.expect("run_diagnose");
    assert_ne!(r.is_error, Some(true), "run_diagnose errored: {r:?}");
    let bundle = r.structured_content.as_ref().expect("structured_content");
    assert_eq!(bundle.get("run_id").and_then(|v| v.as_str()), Some(run_id.as_str()), "bundle.run_id mismatch: {bundle:?}");
    let bundle_errors = bundle.get("errors").and_then(|v| v.as_array()).expect("errors array in bundle");
    let first_err = &bundle_errors[0];
    assert_eq!(first_err.get("error_code").and_then(|v| v.as_str()), Some("WXCTL-H001"), "error_code mismatch: {first_err:?}");
    let fix = first_err.get("fix").and_then(|v| v.as_str()).unwrap_or("");
    assert!(!fix.is_empty(), "fix should be non-empty: {first_err:?}");
    assert!(first_err.get("triage").is_some(), "triage field should be present: {first_err:?}");

    client.cancel().await.ok();

    // Cleanup: temp dir is dropped at end of scope (auto-remove); remove env var.
    drop(tmp);
    unsafe { std::env::remove_var("WXCTL_RUNS_DIR") };
}
