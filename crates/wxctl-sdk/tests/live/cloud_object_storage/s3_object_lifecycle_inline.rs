//! s3_object CRUD with inline content. Creates a storage_connection,
//! s3_bucket, and s3_object; destroys all in reverse topological order.

use crate::{LiveTest, read_profile_field, short_id};

#[tokio::test]
async fn test_s3_object_lifecycle_inline() -> anyhow::Result<()> {
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
name: wxctl-test-obj-{safe_id}
region: eu-gb
storage_class: smart
force_destroy: true

---
kind: s3_object
ref_name: config_{safe_id}
bucket: ${{s3_bucket.wxctl_test_{safe_id}.name}}
region: ${{s3_bucket.wxctl_test_{safe_id}.region}}
key: config/settings.json
content: '{{"feature_flag": true}}'
content_type: application/json
"#
    );

    LiveTest::new("test_s3_object_lifecycle_inline").profile("wxd").timeout(240).yaml(yaml).skip_idempotency().run_crud().await
}
