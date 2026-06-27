/// Deploy a WML script (space -> swspec -> wml_script -> deployment),
/// then verify deployment responds with correct predictions via kind: test.
/// Resources: 4 (space + swspec + wml_script + deployment). Tests: 1 (test_script).
#[tokio::test]
async fn test_wml_script_invocation() -> anyhow::Result<()> {
    super::run_e2e_test("wml_script_invocation.yaml", 4, 1).await
}
