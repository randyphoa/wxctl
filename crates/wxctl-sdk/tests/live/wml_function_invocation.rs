/// Deploy a WML function (space → swspec → wml_function → deployment),
/// then verify deployment responds with correct predictions via kind: test.
/// Resources: 4 (space + swspec + wml_function + deployment). Tests: 1 (test_function).
#[tokio::test]
async fn test_wml_function_invocation() -> anyhow::Result<()> {
    super::run_e2e_test("wml_function_invocation.yaml", 4, 1).await
}
