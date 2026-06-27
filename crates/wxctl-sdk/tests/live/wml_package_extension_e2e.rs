/// Deploy a full WML chain with package extensions: space → 2x package_extension →
/// 2x software_specification → 2x ai_service → 2x wml_deployment, then verify
/// both deployments respond with correct package names via kind: test.
/// Resources: 9. Tests: 2 (test_requests, test_pyyaml).
#[tokio::test]
async fn test_wml_package_extension_e2e() -> anyhow::Result<()> {
    super::run_e2e_test("wml_package_extension_e2e.yaml", 9, 2).await
}
