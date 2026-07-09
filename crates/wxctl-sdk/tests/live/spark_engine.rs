use super::{LiveTest, short_id};

/// Spark engine CRUD lifecycle. Create is async (202) — `post_create` polls until
/// the engine reaches `running`, so a successful apply implicitly verifies the poller.
#[tokio::test]
async fn test_spark_engine_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: spark_engine
ref_name: wxctl_test_{safe_id}
display_name: wxctl-test-{safe_id}
description: wxctl spark_engine CRUD test
origin: native
"#
    );

    LiveTest::new("test_spark_engine_crud").profile("wxd").timeout(900).yaml(yaml).run_crud().await
}
