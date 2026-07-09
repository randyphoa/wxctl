use super::{LiveTest, load_fixture, short_id};

#[tokio::test]
async fn test_full_dependency_chain() -> anyhow::Result<()> {
    let yaml = load_fixture("full_chain.yaml", &short_id());

    LiveTest::new("test_full_dependency_chain").yaml(yaml).expect_resources(5).update("Integration test agent with full dependency chain", "Updated integration test agent").run_crud().await
}
