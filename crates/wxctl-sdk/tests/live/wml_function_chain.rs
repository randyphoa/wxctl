use super::{LiveTest, load_fixture, short_id};

/// Full WML function lifecycle: space → software_specification → wml_function → wml_deployment.
/// Idempotency skipped — see wml_chain.rs for rationale.
#[tokio::test]
async fn test_wml_function_chain() -> anyhow::Result<()> {
    let yaml = load_fixture("wml_function_chain.yaml", &short_id());

    LiveTest::new("test_wml_function_chain").timeout(600).yaml(yaml).expect_resources(4).skip_idempotency().skip_destroyed_check().run_crud().await
}
