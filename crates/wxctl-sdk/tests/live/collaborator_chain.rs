/// Deploy tool + helper agent + primary agent (with collaborator),
/// then verify the primary agent delegates math to the helper.
/// Mirrors ADK customer_care example.
#[tokio::test]
async fn test_collaborator_chain() -> anyhow::Result<()> {
    super::run_e2e_test("collaborator_chain.yaml", 3, 1).await
}
