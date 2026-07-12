use super::{LiveTest, short_id};
use wxctl_core::Config;

/// Apply with a mixed config where one resource has a validation error.
/// Verifies validation catches the error and no resources are created.
#[tokio::test]
async fn test_partial_failure_validation_rejects_all() -> anyhow::Result<()> {
    let safe_id = short_id();

    let yaml = format!(
        r#"
kind: knowledge_base
ref_name: wxctl_test_kb_{safe_id}
name: wxctl_test_kb_{safe_id}
description: Partial failure test KB
---
kind: tool
ref_name: wxctl_test_tool_{safe_id}
name: wxctl_test_tool_{safe_id}
display_name: wxctl_test_tool_{safe_id}
description: Partial failure test tool with bad path
permission: read_write
is_async: false
source_path: /nonexistent/path/that/does/not/exist_{safe_id}
binding:
    python:
        function: calculator:main
"#
    );
    let kb_yaml = format!(
        r#"
kind: knowledge_base
ref_name: wxctl_test_kb_{safe_id}
name: wxctl_test_kb_{safe_id}
description: Partial failure test KB
"#
    );

    // No guard yaml — validation error prevents any resource creation, so nothing to clean.
    LiveTest::new("test_partial_failure_validation_rejects_all")
        .timeout(60)
        .run(move |ctx| async move {
            ctx.phase("apply_expected_to_fail", async {
                let mut config = Config::from_yaml(&yaml)?;
                let result = ctx.client.apply(&mut config).await;
                ctx.expect("apply_expected_to_fail", result.is_err(), "Err (validation rejected)", "Ok")?;
                Ok(())
            })
            .await?;

            ctx.phase("verify_kb_not_created", async {
                let plan = ctx.plan(&kb_yaml).await?;
                let kb_op = plan.operations.iter().find(|op| op.key.kind.as_ref() == "knowledge_base").expect("KB not in plan");
                ctx.expect("verify_kb_not_created", matches!(kb_op.op_type, wxctl_engine::OperationType::Create), "Create", format!("{:?}", kb_op.op_type))?;
                Ok(())
            })
            .await?;

            Ok(())
        })
        .await
}
