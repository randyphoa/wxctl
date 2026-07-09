use super::{LiveTest, short_id};

#[tokio::test]
async fn test_agent_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: agent
ref_name: wxctl_test_{safe_id}
name: wxctl_test_{safe_id}
description: Integration test agent
llm: groq/openai/gpt-oss-120b
"#
    );

    LiveTest::new("test_agent_crud").yaml(yaml).update("Integration test agent", "Updated integration test agent").run_crud().await
}
