/// Deploy KB + tool + agent, then verify the agent correctly picks between
/// KB retrieval and tool invocation depending on the question.
/// Mirrors ADK ibm_knowledge dynamic mode.
#[tokio::test]
async fn test_kb_and_tool() -> anyhow::Result<()> {
    super::run_e2e_test("kb_and_tool.yaml", 3, 2).await
}
