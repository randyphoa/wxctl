use super::{LiveTest, load_fixture, short_id};

/// MCP toolkit + agent lifecycle with tool name resolution:
/// toolkit create → agent create (with `${toolkit.ref.tools.hello}`) → idempotency → destroy.
/// Proves enrich_toolkit_tools converts the UUID array to a name-to-UUID map and
/// the template resolver resolves `${toolkit.x.tools.hello}` to the tool UUID.
#[tokio::test]
async fn test_mcp_toolkit_agent() -> anyhow::Result<()> {
    let yaml = load_fixture("mcp_toolkit_agent.yaml", &short_id());

    LiveTest::new("test_mcp_toolkit_agent").yaml(yaml).expect_resources(2).run_crud().await
}
