//! Scripted MCP client smoke test for the four compose_* tools. Wires an in-process
//! rmcp client and server over a tokio duplex pair (no child process, no profile, no
//! network — compose tools are pure compute / FS), then drives start → paths →
//! prompt → scaffold against a temp dir. This is the Phase 4 deliverable's E2E.

use rmcp::model::{CallToolRequestParams, ClientInfo};
use rmcp::{ServiceExt, serve_client};
use serde_json::json;
use wxctl_mcp::WxctlMcpServer;

async fn connect(read_only: bool) -> rmcp::service::RunningService<rmcp::RoleClient, ClientInfo> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = WxctlMcpServer::new("nonexistent-profile-for-smoke", None, read_only, false);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server serve");
        let _ = running.waiting().await;
    });
    serve_client(ClientInfo::default(), client_io).await.expect("client serve")
}

#[tokio::test]
async fn compose_pipeline_smoke_full_server() {
    let client = connect(false).await;

    // 1. tools/list contains all four compose_* tools.
    let tools = client.list_all_tools().await.expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    for n in ["compose_start", "compose_paths", "compose_prompt", "compose_scaffold"] {
        assert!(names.contains(&n), "missing tool {n}; got {names:?}");
    }

    // 2. start → a recipe + identify prompt.
    let r = client.call_tool(CallToolRequestParams::new("compose_start").with_arguments(json!({"use_case": "HR chatbot with a postgres database"}).as_object().cloned().unwrap())).await.expect("start");
    assert_ne!(r.is_error, Some(true), "start errored: {r:?}");
    let identify_prompt = r.structured_content.as_ref().and_then(|v| v.get("identify_prompt")).and_then(|v| v.as_str()).expect("identify_prompt field");
    assert!(identify_prompt.contains("HR chatbot"));
    let steps = r.structured_content.as_ref().and_then(|v| v.get("recipe")).and_then(|v| v.as_array()).expect("recipe array");
    assert_eq!(steps.len(), 9, "recipe has 9 steps");
    let step_names: Vec<&str> = steps.iter().map(|s| s.get("name").and_then(|v| v.as_str()).unwrap_or_default()).collect();
    assert!(step_names.contains(&"generate_tests"), "recipe includes the generate_tests step; got {step_names:?}");

    // 3. paths → compose/v1 YAML.
    let r = client.call_tool(CallToolRequestParams::new("compose_paths").with_arguments(json!({"config": "resources:\n  - kind: agent\n  - kind: tool\n", "deployment": "saas"}).as_object().cloned().unwrap())).await.expect("paths");
    assert_ne!(r.is_error, Some(true), "paths errored: {r:?}");
    let paths_yaml = r.structured_content.as_ref().and_then(|v| v.get("paths_yaml")).and_then(|v| v.as_str()).expect("paths_yaml field").to_string();
    assert!(paths_yaml.contains("format: compose/v1"));

    // 4. prompt (config mode) from the paths YAML.
    let r = client.call_tool(CallToolRequestParams::new("compose_prompt").with_arguments(json!({"input": "HR chatbot", "paths": paths_yaml}).as_object().cloned().unwrap())).await.expect("prompt");
    assert_ne!(r.is_error, Some(true), "prompt errored: {r:?}");
    let cfg_prompt = r.structured_content.as_ref().and_then(|v| v.get("prompt")).and_then(|v| v.as_str()).expect("prompt field");
    assert!(cfg_prompt.contains("HR chatbot"));

    // 5. scaffold a config into a temp dir → files materialized, no failures.
    // The explicit output_dir must resolve INSIDE cwd (compose_scaffold rejects out-of-cwd
    // dirs — compose-scaffold-incwd spec AC3), so create the temp dir under cwd.
    let tmp = tempfile::tempdir_in(".").unwrap();
    let r = client.call_tool(CallToolRequestParams::new("compose_scaffold").with_arguments(json!({"config": "kind: wml_function\nref_name: f\nsource_path: score.py\n", "output_dir": tmp.path().to_string_lossy()}).as_object().cloned().unwrap())).await.expect("scaffold");
    assert_ne!(r.is_error, Some(true), "scaffold errored: {r:?}");
    let failed = r.structured_content.as_ref().and_then(|v| v.get("failed")).and_then(|v| v.as_bool()).expect("failed field");
    assert!(!failed, "scaffold reported failures: {r:?}");
    assert!(tmp.path().join("score.py").exists(), "scaffold did not write score.py");

    client.cancel().await.ok();
}

#[tokio::test]
async fn read_only_server_omits_compose_scaffold() {
    let client = connect(true).await;
    let tools = client.list_all_tools().await.expect("list tools");
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_ref()).collect();
    // The three read-only compose tools are present...
    for n in ["compose_start", "compose_paths", "compose_prompt"] {
        assert!(names.contains(&n), "read-only server missing {n}; got {names:?}");
    }
    // ...but the FS-writing compose_scaffold is NOT registered under --read-only.
    assert!(!names.contains(&"compose_scaffold"), "read-only server must not expose compose_scaffold; got {names:?}");
    // (mutating apply/destroy/test are also absent — covered by the existing startup smoke.)
    client.cancel().await.ok();
}
