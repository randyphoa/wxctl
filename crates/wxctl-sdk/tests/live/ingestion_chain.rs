use super::{COS_ENV_MAPPINGS, LiveTest, set_env_from_profile, short_id};

/// Full CSV → Iceberg ingestion chain from the 2026-04-20 eu-gb spec.
///
/// Provisions storage_connection + s3_bucket + s3_object (3-row CSV) +
/// storage_registration + presto_engine + spark_engine + schema +
/// ingestion_job in a single DAG, then tears everything down. A successful
/// apply implicitly verifies every post_create poller:
///   * spark_engine reaches status=running
///   * ingestion_job reaches status=completed (120×10s poll budget)
/// No assertion on raw table data — `wxctl` has no SQL query API.
///
/// Skipped when the `wxd` profile is missing COS credentials. Expected to
/// fail at the `spark_engine` step on classic-SaaS tenants where Spark
/// provisioning returns opaque 500s (see
/// `docs/superpowers/specs/2026-04-20-declarative-ingestion-eugb-design.md`
/// risk table). The harness's cleanup guard tears down any created
/// resources either way.
#[tokio::test]
async fn test_ingestion_chain_csv_to_iceberg() -> anyhow::Result<()> {
    // Uses the pre-existing COS bucket from the profile — bucket CREATE with
    // HMAC signing has a separate, known issue on classic-SaaS profiles and
    // the `storage_registration` API enforces 1 reg per bucket anyway. Tests
    // run against the shared bucket: s3_bucket is NoOp (HEAD succeeds),
    // storage_registration creates a fresh catalog per run.
    //
    // Precondition: the bucket must not already have a storage_registration
    // with a different catalog_name — if it does, this test will fail at the
    // registration step with `HTTP 400: Bucket already exists`. Clean up any
    // stale registration manually before running.
    let missing = set_env_from_profile("wxd", COS_ENV_MAPPINGS)?;
    if let Some(field) = missing {
        eprintln!("SKIP test_ingestion_chain_csv_to_iceberg: profile missing {field}");
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
kind: s3_object
ref_name: people_{safe_id}
bucket: ${{s3_bucket.bucket_{safe_id}.name}}
region: ${{s3_bucket.bucket_{safe_id}.region}}
key: wxctl-test/{safe_id}/people.csv
content: |
  id,name,city
  1,alice,london
  2,bob,tokyo
  3,carol,new-york
content_type: text/csv

---
kind: storage_registration
ref_name: reg_{safe_id}
display_name: wxctl-test-{safe_id}
description: wxctl e2e ingestion test
bucket: ${{s3_bucket.bucket_{safe_id}.name}}
associated_catalog:
  catalog_name: wxctl_test_{safe_id}
  catalog_type: iceberg

---
kind: presto_engine
ref_name: presto_{safe_id}
display_name: wxctl-test-presto-{safe_id}
description: wxctl e2e ingestion presto engine
origin: native
configuration:
  size_config: starter
  coordinator:
    node_type: bx2.48x192
    quantity: 1
  worker:
    node_type: bx2.48x192
    quantity: 1
associated_catalogs:
  - ${{storage_registration.reg_{safe_id}.catalog_name}}
status: running

---
kind: spark_engine
ref_name: spark_{safe_id}
display_name: wxctl-test-spark-{safe_id}
description: wxctl e2e ingestion spark engine
origin: native
type: spark
configuration:
  default_version: "3.4"
  engine_home:
    storage_name: ${{s3_bucket.bucket_{safe_id}.name}}
  scale_config:
    node_type: starter
    number_of_nodes: 1
associated_catalogs:
  - ${{storage_registration.reg_{safe_id}.catalog_name}}
status: running

---
kind: schema
ref_name: schema_{safe_id}
name: sample_{safe_id}
custom_path: wxctl-test/{safe_id}
catalog_id: ${{storage_registration.reg_{safe_id}.catalog_name}}
engine_id: ${{presto_engine.presto_{safe_id}}}

---
kind: ingestion_job
ref_name: ingest_{safe_id}
id: wxctl-test-ingest-{safe_id}
engine_id: ${{spark_engine.spark_{safe_id}}}
source:
  source_type: storage
  file_type: csv
  file_paths: s3://${{s3_bucket.bucket_{safe_id}.name}}/${{s3_object.people_{safe_id}.key}}
  bucket_details:
    bucket_name: ${{s3_bucket.bucket_{safe_id}.name}}
    bucket_type: ibm_cos
    endpoint: ${{env:WXCTL_TEST_COS_ENDPOINT}}
  file_format_properties:
    header: "true"
target:
  catalog: ${{storage_registration.reg_{safe_id}.catalog_name}}
  schema: ${{schema.schema_{safe_id}.name}}
  table: people
  write_mode: overwrite
  schema_infer: true
"#
    );

    LiveTest::new("test_ingestion_chain_csv_to_iceberg").profile("wxd").timeout(1800).yaml(yaml).skip_idempotency().skip_destroyed_check().run_crud().await
}
