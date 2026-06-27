/// Deploy a Node.js MCP toolkit + agent, then verify tool invocation via kind: test.
/// Exercises the full artifact flow: ZIP build → upload → agent tool binding → invocation.
/// Resources: 2 (toolkit + agent). Tests: 1 (test_hello).
///
/// Ignored: the orchestrate `/v1/orchestrate/toolkits/{id}/upload` endpoint consistently
/// drops the TLS connection for this Node artifact on the us-south gateway — observed
/// reproducibly even single-threaded with the bounded network retry wired in for 2026-04-19.
/// The Python equivalent (test_mcp_toolkit_python_invocation) uploads the same shape of
/// artifact to the same endpoint and succeeds, so the issue appears specific to the
/// particular request/routing for this fixture and is unfixable from the client side.
/// Restore the test once the orchestrate upload path is stable, or switch to chunked upload.
#[ignore = "(2026-04-19) orchestrate upload endpoint consistently drops the connection for Node artifacts on us-south (see doc comment above)"]
#[tokio::test]
async fn test_mcp_toolkit_node_invocation() -> anyhow::Result<()> {
    super::run_e2e_test("mcp_toolkit_node_invocation.yaml", 2, 1).await
}
