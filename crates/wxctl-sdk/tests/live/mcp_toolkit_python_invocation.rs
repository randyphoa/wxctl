/// Deploy a Python MCP toolkit + agent, then verify tool invocation via kind: test.
/// Exercises the full artifact flow: ZIP build → upload → agent tool binding → invocation.
/// Resources: 2 (toolkit + agent). Tests: 1 (test_hello).
#[tokio::test]
async fn test_mcp_toolkit_python_invocation() -> anyhow::Result<()> {
    super::run_e2e_test("mcp_toolkit_python_invocation.yaml", 2, 1).await
}
