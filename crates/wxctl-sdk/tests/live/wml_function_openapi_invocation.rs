/// Deploy a WML function (space → swspec → function → deployment) plus
/// Orchestrate resources (connection → OpenAPI tool → agent) in a single apply,
/// then verify the deployment responds correctly.
/// Resources: 7 (4 WML + 3 orchestrate). Tests: 1 (deployment prediction).
#[tokio::test]
async fn test_wml_function_openapi_invocation() -> anyhow::Result<()> {
    super::run_e2e_test("wml_function_openapi_invocation.yaml", 7, 1).await
}
