/// Deploy a remote SSE MCP toolkit (CoinGecko) + agent, then verify tool invocation.
/// Tests the SSE transport path — no local artifact upload, URL-only reference.
/// Resources: 2 (toolkit + agent). Tests: 1 (test_coingecko).
#[tokio::test]
async fn test_mcp_toolkit_remote_sse() -> anyhow::Result<()> {
    super::run_e2e_test("mcp_toolkit_remote_sse.yaml", 2, 1).await
}
