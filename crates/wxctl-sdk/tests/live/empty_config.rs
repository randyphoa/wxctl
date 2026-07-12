use super::LiveTest;
use wxctl_core::Config;

/// Empty config produces no errors and no operations.
#[tokio::test]
async fn test_empty_config_no_ops() -> anyhow::Result<()> {
    LiveTest::new("test_empty_config_no_ops")
        .timeout(60)
        .run(|ctx| async move {
            ctx.phase("plan_empty", async {
                let mut config = Config { resources: vec![] };
                let plan = ctx.client.plan(&mut config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                ctx.expect_eq_usize("plan_empty", "operations", 0, plan.operations.len())?;
                Ok(())
            })
            .await?;

            ctx.phase("apply_empty", async {
                let mut config = Config { resources: vec![] };
                let result = ctx.client.apply(&mut config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                ctx.expect_eq_usize("apply_empty", "succeeded", 0, result.succeeded.len())?;
                ctx.expect_eq_usize("apply_empty", "failed", 0, result.failed.len())?;
                ctx.expect_eq_usize("apply_empty", "skipped", 0, result.skipped.len())?;
                Ok(())
            })
            .await?;

            Ok(())
        })
        .await
}
