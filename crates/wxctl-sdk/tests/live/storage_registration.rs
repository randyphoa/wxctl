use super::{COS_ENV_MAPPINGS, LiveTest, set_env_from_profile, short_id};

/// storage_registration CRUD against a watsonx.data SaaS instance (eu-gb region).
///
/// After the 2026-04-20 refactor the registration's wire body is
/// assembled from DAG edges: this test first materialises a
/// `storage_connection` + `s3_bucket` pair pointing at the pre-existing
/// bucket in `test_profiles.json`, then registers it.
#[tokio::test]
async fn test_storage_registration_lifecycle() -> anyhow::Result<()> {
    let missing = set_env_from_profile("wxd", COS_ENV_MAPPINGS)?;
    if let Some(field) = missing {
        eprintln!("SKIP test_storage_registration_lifecycle: profile missing {field}");
        return Ok(());
    }

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: storage_connection
ref_name: cos_{safe_id}
type: ibm_cos
access_key: ${{env:WXCTL_TEST_COS_ACCESS_KEY}}
secret_key: ${{env:WXCTL_TEST_COS_SECRET_KEY}}
endpoint: ${{env:WXCTL_TEST_COS_ENDPOINT}}
instance_crn: ${{env:WXCTL_TEST_COS_CRN}}

---
kind: s3_bucket
ref_name: bucket_{safe_id}
connection: ${{storage_connection.cos_{safe_id}}}
name: ${{env:WXCTL_TEST_COS_BUCKET}}
region: eu-gb
storage_class: smart

---
kind: storage_registration
ref_name: wxctl_test_{safe_id}
display_name: wxctl-test-{safe_id}
description: wxctl storage_registration CRUD test
bucket: ${{s3_bucket.bucket_{safe_id}.name}}
associated_catalog:
  catalog_name: wxctl_test_{safe_id}
  catalog_type: iceberg
"#
    );

    LiveTest::new("test_storage_registration_lifecycle").profile("wxd").timeout(300).yaml(yaml).run_crud().await
}
