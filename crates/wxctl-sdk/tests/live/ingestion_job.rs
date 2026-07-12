use super::{LiveTest, short_id};
use wxctl_core::Config;
use wxctl_engine::OperationType;

/// Ingestion job CRUD. Requires a configured Spark engine, target table, and a
/// source file accessible to watsonx.data. Set:
///   WXCTL_TEST_WXD_SPARK_ENGINE_ID=<spark-engine-id>
///   WXCTL_TEST_WXD_INGEST_SOURCE_PATH=<s3://bucket/file.csv>
///   WXCTL_TEST_WXD_INGEST_FILE_TYPE=<csv|parquet|json|orc|avro|txt>
///   WXCTL_TEST_WXD_INGEST_BUCKET_NAME=<bucket-name>
///   WXCTL_TEST_WXD_INGEST_BUCKET_TYPE=<aws_s3|minio|ibm_cos|...>
///   WXCTL_TEST_WXD_INGEST_CATALOG=<target-catalog>
///   WXCTL_TEST_WXD_INGEST_SCHEMA=<target-schema>
///   WXCTL_TEST_WXD_INGEST_TABLE=<target-table>
///
/// Skipped if any required env var is unset. Verifies the `post_create` poller
/// runs to completion and the `pre_delete` no-op hook completes (ingestion_job
/// has no DELETE endpoint).
#[tokio::test]
async fn test_ingestion_job_crud() -> anyhow::Result<()> {
    let required = ["WXCTL_TEST_WXD_SPARK_ENGINE_ID", "WXCTL_TEST_WXD_INGEST_SOURCE_PATH", "WXCTL_TEST_WXD_INGEST_FILE_TYPE", "WXCTL_TEST_WXD_INGEST_BUCKET_NAME", "WXCTL_TEST_WXD_INGEST_BUCKET_TYPE", "WXCTL_TEST_WXD_INGEST_CATALOG", "WXCTL_TEST_WXD_INGEST_SCHEMA", "WXCTL_TEST_WXD_INGEST_TABLE"];
    let missing: Vec<&str> = required.iter().copied().filter(|v| std::env::var(v).is_err()).collect();
    if !missing.is_empty() {
        eprintln!("SKIP test_ingestion_job_crud: missing env vars: {}", missing.join(", "));
        return Ok(());
    }

    let engine_id = std::env::var("WXCTL_TEST_WXD_SPARK_ENGINE_ID")?;
    let source_path = std::env::var("WXCTL_TEST_WXD_INGEST_SOURCE_PATH")?;
    let file_type = std::env::var("WXCTL_TEST_WXD_INGEST_FILE_TYPE")?;
    let bucket_name = std::env::var("WXCTL_TEST_WXD_INGEST_BUCKET_NAME")?;
    let bucket_type = std::env::var("WXCTL_TEST_WXD_INGEST_BUCKET_TYPE")?;
    let target_catalog = std::env::var("WXCTL_TEST_WXD_INGEST_CATALOG")?;
    let target_schema = std::env::var("WXCTL_TEST_WXD_INGEST_SCHEMA")?;
    let target_table = std::env::var("WXCTL_TEST_WXD_INGEST_TABLE")?;

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: ingestion_job
ref_name: wxctl_test_{safe_id}
id: wxctl-test-{safe_id}
engine_id: {engine_id}
source:
  source_type: storage
  file_paths: {source_path}
  file_type: {file_type}
  bucket_details:
    bucket_name: {bucket_name}
    bucket_type: {bucket_type}
target:
  catalog: {target_catalog}
  schema: {target_schema}
  table: {target_table}
  write_mode: append
  schema_infer: true
"#
    );

    LiveTest::new("test_ingestion_job_crud")
        .profile("wxd")
        .timeout(1800)
        .yaml(yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create", async {
                let mut config = Config::from_yaml(&yaml)?;
                let result = ctx.client.apply(&mut config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                ctx.expect_no_failures("create", &result.failed)?;
                Ok(())
            })
            .await?;

            // Destroy is a client-side no-op (pre_delete returns Handled). Verify the
            // Delete op completes successfully even though no API call is made.
            ctx.phase("destroy", async {
                let mut config = Config::from_yaml(&yaml)?;
                let result = ctx.client.destroy(&mut config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                ctx.expect_no_failures("destroy", &result.failed)?;
                let delete_op = result.succeeded.iter().find(|r| &*r.key.kind == "ingestion_job").expect("ingestion_job in destroy results");
                ctx.expect("destroy", matches!(delete_op.operation, OperationType::Delete), "Delete", format!("{:?}", delete_op.operation))?;
                Ok(())
            })
            .await?;

            Ok(())
        })
        .await
}
