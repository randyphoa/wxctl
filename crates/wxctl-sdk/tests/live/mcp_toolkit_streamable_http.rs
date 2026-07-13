/// Deploy a streamable HTTP MCP toolkit (GitHub) + agent, then verify tool invocation.
/// Tests the streamable_http transport path with connection credentials.
/// Resources: 2 (toolkit + agent). Tests: 1 (test_github).
///
/// Ignored: the fixture targets https://api.githubcopilot.com/mcp/ and references a
/// pre-existing orchestrate_connection named "mcpgithub" on the test account. When that
/// connection isn't present, the orchestrate gateway's own initialization call against
/// GitHub returns 401 and the toolkit POST fails with "Gateway creation failed: 502 ...
/// 401 Unauthorized". Restore once the test profile provisions a real GitHub MCP
/// connection — or swap the fixture to a public streamable_http MCP endpoint.
#[ignore = "(2026-04-19) requires GitHub Copilot MCP credentials on the test account; gateway returns 401 otherwise (see doc comment above)"]
#[tokio::test]
async fn test_mcp_toolkit_streamable_http() -> anyhow::Result<()> {
    super::run_e2e_test("mcp_toolkit_streamable_http.yaml", 2, 1).await
}
