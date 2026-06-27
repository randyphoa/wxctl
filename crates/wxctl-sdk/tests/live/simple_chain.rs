use super::{LiveTest, load_fixture, short_id};

/// Mirrors examples/simple/config.yaml: knowledge_base + tool → agent.
/// Verifies cross-resource references resolve correctly through the full
/// create → idempotency → update → idempotency → destroy → verify cycle.
#[tokio::test]
async fn test_simple_chain() -> anyhow::Result<()> {
    let yaml = load_fixture("simple_chain.yaml", &short_id());

    LiveTest::new("test_simple_chain").yaml(yaml).expect_resources(3).update("Integration test agent with tool and knowledge base", "Updated integration test agent").run_crud().await
}
