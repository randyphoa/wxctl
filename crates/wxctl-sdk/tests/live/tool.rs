use super::{LiveTest, short_id};

#[tokio::test]
async fn test_tool_with_agent_run_parameter() -> anyhow::Result<()> {
    let safe_id = short_id();
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let yaml = format!(
        r#"
kind: tool
ref_name: wxctl_test_{safe_id}
name: wxctl_test_{safe_id}
display_name: wxctl_test_{safe_id}
description: Integration test tool with agent_run_parameter
permission: read_write
is_async: false
source_path: {manifest_dir}/tests/fixtures/context_tool
binding:
    python:
        function: echo_session:echo_session
        agent_run_parameter: context
"#
    );

    // No update step here: idempotency after create proves agent_run_parameter
    // translation works (mismatch would surface as Update on the second plan).
    LiveTest::new("test_tool_with_agent_run_parameter").yaml(yaml).run_crud().await
}
