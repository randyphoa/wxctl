/// Deploy a WML AI service (space → swspec → ai_service → deployment),
/// then verify deployment responds with correct payload via kind: test.
/// Resources: 4 (space + swspec + ai_service + deployment). Tests: 1 (test_ai_service).
#[tokio::test]
async fn test_wml_ai_service_invocation() -> anyhow::Result<()> {
    super::run_e2e_test("wml_ai_service_invocation.yaml", 4, 1).await
}
