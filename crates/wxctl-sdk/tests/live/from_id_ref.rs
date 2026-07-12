use super::{LiveTest, assert_plan_op_type, short_id};

/// Create a KB first, then add an agent that references it in a second apply.
/// Verifies incremental config additions work: KB is NoOp, agent is Created.
#[tokio::test]
async fn test_incremental_reference() -> anyhow::Result<()> {
    let safe_id = short_id();
    let kb_yaml = format!(
        r#"
kind: knowledge_base
ref_name: wxctl_test_kb_{safe_id}
name: wxctl_test_kb_{safe_id}
description: Incremental ref test KB
"#
    );
    let combined_yaml = format!(
        r#"
kind: knowledge_base
ref_name: wxctl_test_kb_{safe_id}
name: wxctl_test_kb_{safe_id}
description: Incremental ref test KB
---
kind: agent
ref_name: wxctl_test_agent_{safe_id}
name: wxctl_test_agent_{safe_id}
description: Incremental ref test agent
llm: groq/openai/gpt-oss-120b
knowledge_base:
    - ${{knowledge_base.wxctl_test_kb_{safe_id}}}
"#
    );

    // Cleanup guard targets the combined YAML so a failure between the two
    // applies still cleans up KB (the engine treats missing-resource destroys
    // as no-ops).
    LiveTest::new("test_incremental_reference")
        .guard_yaml(combined_yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create_kb_only", async { ctx.apply("create_kb_only", &kb_yaml).await.map(|_| ()) }).await?;

            ctx.phase("plan_with_agent_added", async {
                let plan = ctx.plan(&combined_yaml).await?;
                assert_plan_op_type(&plan, "knowledge_base", "wxctl_test_kb_", "no-op");
                assert_plan_op_type(&plan, "agent", "wxctl_test_agent_", "create");
                Ok(())
            })
            .await?;

            ctx.phase("apply_combined", async { ctx.apply("apply_combined", &combined_yaml).await.map(|_| ()) }).await?;
            ctx.phase("idempotency", async { ctx.assert_plan_all_noop(&combined_yaml).await }).await?;
            ctx.phase("destroy", async { ctx.destroy("destroy", &combined_yaml).await.map(|_| ()) }).await?;

            Ok(())
        })
        .await
}
