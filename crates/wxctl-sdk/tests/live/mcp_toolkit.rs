use super::{LiveTest, short_id};

/// MCP toolkit lifecycle with public-registry source: create → idempotency → destroy.
/// Uses the @modelcontextprotocol/server-everything npm package; no artifact upload.
#[tokio::test]
async fn test_mcp_toolkit_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: toolkit
ref_name: wxctl_test_mcp_{safe_id}
name: wxctl-test-mcp-{safe_id}
description: MCP test server - official everything server
mcp:
    source: public-registry
    command: npx
    args: ["-y", "@modelcontextprotocol/server-everything"]
    tools: ["echo", "add"]
"#
    );

    LiveTest::new("test_mcp_toolkit_crud").yaml(yaml).expect_resources(1).run_crud().await
}

/// MCP toolkit lifecycle with local files source: create → upload → idempotency → destroy.
/// Exercises the full artifact flow: ZIP build → POST create → POST upload.
#[tokio::test]
async fn test_mcp_toolkit_local_upload() -> anyhow::Result<()> {
    let safe_id = short_id();
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let yaml = format!(
        r#"
kind: toolkit
ref_name: wxctl_test_mcp_local_{safe_id}
name: wxctl-test-mcp-local-{safe_id}
description: Hello world MCP server for upload testing
server_path: {manifest_dir}/tests/fixtures/mcp_hello_server
mcp:
    source: files
    command: python
    args: ["server.py"]
    tools: ["hello"]
"#
    );

    LiveTest::new("test_mcp_toolkit_local_upload").yaml(yaml).expect_resources(1).run_crud().await
}
