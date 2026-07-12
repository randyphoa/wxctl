/// Deploy an OpenAPI tool (httpbin echoGet + echoPost) + bearer_token connection + agent,
/// then verify structured tool invocation via kind: test assertions.
/// Resources: 4 (1 connection + 2 expanded tools + 1 agent). Tests: 1 (test_echo_get).
#[tokio::test]
async fn test_openapi_tool_invocation() -> anyhow::Result<()> {
    super::run_e2e_test("openapi_tool_invocation.yaml", 4, 1).await
}
