use super::{LiveTest, assert_plan_op_type, assert_plan_update_fields, short_id};

/// Apply, modify description, plan → Update with correct fields.
#[tokio::test]
async fn test_plan_update_fields() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: agent
ref_name: wxctl_test_{safe_id}
name: wxctl_test_{safe_id}
description: Plan update fields test agent
llm: groq/openai/gpt-oss-120b
"#
    );

    LiveTest::new("test_plan_update_fields")
        .yaml(yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create", async { ctx.apply("create", &yaml).await.map(|_| ()) }).await?;

            let updated = yaml.replace("Plan update fields test agent", "Updated plan update fields test agent");

            ctx.phase("plan_update_check", async {
                let plan = ctx.plan(&updated).await?;
                assert_plan_op_type(&plan, "agent", "wxctl_test_", "update");
                assert_plan_update_fields(&plan, "agent", "wxctl_test_", &["description"]);
                Ok(())
            })
            .await?;

            ctx.phase("destroy", async { ctx.destroy("destroy", &yaml).await.map(|_| ()) }).await?;

            Ok(())
        })
        .await
}
