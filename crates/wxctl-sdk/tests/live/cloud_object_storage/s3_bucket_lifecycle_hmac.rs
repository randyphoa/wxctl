//! s3_bucket CRUD using HMAC SigV4 auth against IBM COS. HMAC mode
//! requires an explicit `instance_crn` on the storage_connection.
//! Skips when the test profile is missing the required keys.

use crate::{LiveTest, read_profile_field, short_id};

#[tokio::test]
async fn test_s3_bucket_lifecycle_hmac() -> anyhow::Result<()> {
    let crn = match read_profile_field("wxd", "cos", "cos_instance_crn")? {
        Some(v) => v,
        None => {
            eprintln!("SKIP test_s3_bucket_lifecycle_hmac: profile missing cos.cos_instance_crn");
            return Ok(());
        }
    };
    let Some(access_key) = read_profile_field("wxd", "cos", "access_key")? else {
        eprintln!("SKIP test_s3_bucket_lifecycle_hmac: profile missing cos.access_key");
        return Ok(());
    };
    let Some(secret_key) = read_profile_field("wxd", "cos", "secret_key")? else {
        eprintln!("SKIP test_s3_bucket_lifecycle_hmac: profile missing cos.secret_key");
        return Ok(());
    };

    let safe_id = short_id();
    let yaml = format!(
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
name: wxctl-test-hmac-{safe_id}
region: eu-gb
storage_class: smart
force_destroy: true
"#
    );

    LiveTest::new("test_s3_bucket_lifecycle_hmac").profile("wxd_hmac").timeout(180).yaml(yaml).skip_idempotency().run_crud().await
}
