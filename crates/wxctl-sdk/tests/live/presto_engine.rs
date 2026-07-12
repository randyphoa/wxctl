use super::{LiveTest, short_id};

/// Presto engine CRUD lifecycle — create → idempotency → destroy.
#[tokio::test]
async fn test_presto_engine_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: presto_engine
ref_name: wxctl_test_{safe_id}
display_name: wxctl-test-{safe_id}
description: wxctl presto_engine CRUD test
origin: native
configuration:
  size_config: starter
  coordinator:
    node_type: bx2.48x192
    quantity: 1
  worker:
    node_type: bx2.48x192
    quantity: 1
"#
    );

    LiveTest::new("test_presto_engine_crud").profile("wxd").timeout(600).yaml(yaml).run_crud().await
}

/// Pause/resume via `status` field transitions. Verifies `pre_update` routes to
/// `/pause` and `/resume` action endpoints (returning `HookOutcome::Handled`) rather
/// than a plain PATCH, and that subsequent plans are NoOp.
#[tokio::test]
async fn test_presto_engine_pause_resume() -> anyhow::Result<()> {
    let safe_id = short_id();
    let running_yaml = format!(
        r#"
kind: presto_engine
ref_name: wxctl_test_{safe_id}
display_name: wxctl-test-{safe_id}
description: wxctl presto_engine pause/resume test
origin: native
configuration:
  size_config: starter
  coordinator:
    node_type: bx2.48x192
    quantity: 1
  worker:
    node_type: bx2.48x192
    quantity: 1
status: running
"#
    );
    let paused_yaml = running_yaml.replace("status: running", "status: paused");

    LiveTest::new("test_presto_engine_pause_resume")
        .profile("wxd")
        .timeout(600)
        .yaml(running_yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create", async { ctx.apply("create", &running_yaml).await.map(|_| ()) }).await?;
            ctx.phase("pause", async { ctx.apply("pause", &paused_yaml).await.map(|_| ()) }).await?;
            ctx.phase("idempotency", async { ctx.assert_plan_all_noop(&paused_yaml).await }).await?;
            ctx.phase("resume", async { ctx.apply("resume", &running_yaml).await.map(|_| ()) }).await?;
            ctx.phase("idempotency", async { ctx.assert_plan_all_noop(&running_yaml).await }).await?;
            ctx.phase("destroy", async { ctx.destroy("destroy", &running_yaml).await.map(|_| ()) }).await?;
            ctx.phase("verify_destroyed", async { ctx.assert_destroyed(&running_yaml).await }).await?;

            Ok(())
        })
        .await
}
