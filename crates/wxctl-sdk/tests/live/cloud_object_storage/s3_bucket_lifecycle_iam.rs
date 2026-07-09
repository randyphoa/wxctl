//! s3_bucket CRUD against a live IBM COS instance using IAM (apikey)
//! auth. Skips when the profile lacks COS config. Auto-discovery of the
//! COS instance CRN is done at load time by the `storage_connection`
//! handler when the account has exactly one COS instance.

use crate::{LiveTest, read_profile_field, short_id};

#[tokio::test]
async fn test_s3_bucket_lifecycle_iam() -> anyhow::Result<()> {
    let crn_line = match read_profile_field("wxd", "cos", "cos_instance_crn")? {
        Some(crn) => format!("instance_crn: \"{crn}\""),
        None => String::new(),
    };

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: storage_connection
ref_name: cos_{safe_id}
type: ibm_cos
{crn_line}

---
kind: s3_bucket
ref_name: wxctl_test_{safe_id}
connection: ${{storage_connection.cos_{safe_id}}}
name: wxctl-test-iam-{safe_id}
region: eu-gb
storage_class: smart
force_destroy: true
"#
    );

    LiveTest::new("test_s3_bucket_lifecycle_iam")
        .profile("wxd")
        .timeout(180)
        .yaml(yaml)
        // Discovery is `skip` on s3_bucket, so the engine always plans
        // Create; idempotency is provided by the handler's HEAD-then-noop path.
        .skip_idempotency()
        .run_crud()
        .await
}
