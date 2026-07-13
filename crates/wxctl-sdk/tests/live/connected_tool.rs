/// Deploy connection + tool (with connection binding) + agent, then verify
/// the agent can invoke the connection-backed tool.
/// Mirrors ADK customer_care ServiceNow example.
#[tokio::test]
async fn test_connected_tool() -> anyhow::Result<()> {
    super::run_e2e_test("connected_tool.yaml", 3, 1).await
}
