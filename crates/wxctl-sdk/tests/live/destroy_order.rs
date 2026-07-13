use super::{LiveTest, load_fixture, short_id};

/// Verify destroy of dependency chain completes without errors.
/// If ordering were wrong, API would reject deleting KB/tool while agent references them.
#[tokio::test]
async fn test_destroy_dependency_chain_succeeds() -> anyhow::Result<()> {
    let yaml = load_fixture("simple_chain.yaml", &short_id());

    LiveTest::new("test_destroy_dependency_chain_succeeds").yaml(yaml).skip_idempotency().run_crud().await
}
