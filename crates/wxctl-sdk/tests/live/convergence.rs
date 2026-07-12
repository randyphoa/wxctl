use super::{LiveTest, assert_plan_op_type, load_fixture, short_id};

/// Apply chain → manually destroy one resource → re-apply → only the missing resource is recreated.
#[tokio::test]
async fn test_convergence_after_manual_delete() -> anyhow::Result<()> {
    let test_id = short_id();
    let yaml = load_fixture("simple_chain.yaml", &test_id);
    let kb_yaml = format!(
        r#"
kind: knowledge_base
ref_name: wxctl_test_kb_{test_id}
name: wxctl_test_kb_{test_id}
description: Integration test knowledge base
"#
    );

    LiveTest::new("test_convergence_after_manual_delete")
        .timeout(600)
        .yaml(yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create", async {
                let result = ctx.apply("create", &yaml).await?;
                ctx.expect_eq_usize("create", "expected_resources", 3, result.succeeded.len())?;
                Ok(())
            })
            .await?;

            ctx.phase("destroy_kb_only", async { ctx.destroy("destroy_kb_only", &kb_yaml).await.map(|_| ()) }).await?;

            ctx.phase("plan_after_partial_destroy", async {
                let plan = ctx.plan(&yaml).await?;
                assert_plan_op_type(&plan, "knowledge_base", "wxctl_test_kb_", "create");
                assert_plan_op_type(&plan, "tool", "wxctl_test_tool_", "no-op");
                // Agent may show update (KB reference changed) — that's expected
                Ok(())
            })
            .await?;

            ctx.phase("reapply", async { ctx.apply("reapply", &yaml).await.map(|_| ()) }).await?;
            ctx.phase("idempotency", async { ctx.assert_plan_all_noop(&yaml).await }).await?;
            ctx.phase("destroy", async { ctx.destroy("destroy", &yaml).await.map(|_| ()) }).await?;

            Ok(())
        })
        .await
}
