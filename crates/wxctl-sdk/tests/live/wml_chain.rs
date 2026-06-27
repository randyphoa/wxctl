use super::{LiveTest, load_fixture, short_id};

/// Full WML resource lifecycle: space → software_specification → ai_service → wml_deployment.
///
/// Idempotency checks are skipped because resources with unresolved `${...}` template
/// refs get deferred during plan's reconciliation phase, causing them to appear as
/// Create instead of NoOp. This is an engine limitation for resources whose scoping
/// params (space_id) are template references.
#[tokio::test]
async fn test_wml_chain() -> anyhow::Result<()> {
    let yaml = load_fixture("wml_chain.yaml", &short_id());

    LiveTest::new("test_wml_chain").timeout(600).yaml(yaml).expect_resources(4).skip_idempotency().skip_destroyed_check().run_crud().await
}
