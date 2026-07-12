//! on_destroy: retain — destroy preserves the bucket while still
//! deleting the dependent s3_object. Re-apply adopts the retained
//! bucket. Final cleanup flips `on_destroy: delete` so the guard
//! leaves nothing behind.

use crate::{LiveTest, read_profile_field, short_id};

#[tokio::test]
async fn test_on_destroy_retain_roundtrip() -> anyhow::Result<()> {
    let Some(crn) = read_profile_field("wxd", "cos", "cos_instance_crn")? else {
        eprintln!("SKIP test_on_destroy_retain_roundtrip: profile missing cos.cos_instance_crn");
        return Ok(());
    };
    let Some(access_key) = read_profile_field("wxd", "cos", "access_key")? else {
        eprintln!("SKIP test_on_destroy_retain_roundtrip: profile missing cos.access_key");
        return Ok(());
    };
    let Some(secret_key) = read_profile_field("wxd", "cos", "secret_key")? else {
        eprintln!("SKIP test_on_destroy_retain_roundtrip: profile missing cos.secret_key");
        return Ok(());
    };

    let safe_id = short_id();
    let retained_yaml = format!(
        r#"
kind: storage_connection
ref_name: cos_{safe_id}
type: ibm_cos
access_key: "{access_key}"
secret_key: "{secret_key}"
instance_crn: "{crn}"

---
kind: s3_bucket
ref_name: wxctl_test_{safe_id}
connection: ${{storage_connection.cos_{safe_id}}}
name: wxctl-test-retain-{safe_id}
region: eu-gb
storage_class: smart
force_destroy: true
on_destroy: retain

---
kind: s3_object
ref_name: config_{safe_id}
bucket: ${{s3_bucket.wxctl_test_{safe_id}.name}}
region: ${{s3_bucket.wxctl_test_{safe_id}.region}}
key: config/retain-test.json
content: '{{"feature_flag": true}}'
content_type: application/json
"#
    );
    let cleanup_yaml = retained_yaml.replace("on_destroy: retain", "on_destroy: delete");

    LiveTest::new("test_on_destroy_retain_roundtrip")
        .profile("wxd_hmac")
        .timeout(240)
        .guard_yaml(cleanup_yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create", async { ctx.apply("create", &retained_yaml).await.map(|_| ()) }).await?;

            ctx.phase("destroy_with_retain", async {
                let r = ctx.destroy("destroy", &retained_yaml).await?;
                // s3_bucket should be Retain; s3_object should be Delete.
                let bucket = r.succeeded.iter().find(|s| &*s.key.kind == "s3_bucket").ok_or_else(|| anyhow::anyhow!("no s3_bucket op in destroy results"))?;
                ctx.expect("destroy_with_retain", matches!(bucket.operation, wxctl_engine::OperationType::Retain), "Retain", format!("{:?}", bucket.operation))?;
                let object = r.succeeded.iter().find(|s| &*s.key.kind == "s3_object").ok_or_else(|| anyhow::anyhow!("no s3_object op in destroy results"))?;
                ctx.expect("destroy_with_retain", matches!(object.operation, wxctl_engine::OperationType::Delete), "Delete", format!("{:?}", object.operation))?;
                Ok(())
            })
            .await?;

            // Re-apply: the retained bucket is adopted via the existing idempotent-create path,
            // and the s3_object is recreated. Any failure here proves retention broke adoption.
            ctx.phase("reapply_adopts", async { ctx.apply("reapply", &retained_yaml).await.map(|_| ()) }).await?;

            // Final cleanup: switch to on_destroy: delete so everything goes.
            ctx.phase("final_cleanup", async { ctx.destroy("final", &cleanup_yaml).await.map(|_| ()) }).await?;

            Ok(())
        })
        .await
}
