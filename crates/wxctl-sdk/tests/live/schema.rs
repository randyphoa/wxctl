use super::{LiveTest, short_id};

/// Catalog schema CRUD. Requires an existing catalog and Presto engine because
/// catalog provisioning is out of scope. Set environment variables:
///   WXCTL_TEST_WXD_CATALOG_ID=<catalog-id>
///   WXCTL_TEST_WXD_PRESTO_ENGINE_ID=<presto-engine-id>
/// Optional: WXCTL_TEST_WXD_STORAGE_NAME=<bucket-name>
#[tokio::test]
async fn test_schema_crud() -> anyhow::Result<()> {
    let Ok(catalog_id) = std::env::var("WXCTL_TEST_WXD_CATALOG_ID") else {
        eprintln!("SKIP test_schema_crud: WXCTL_TEST_WXD_CATALOG_ID not set");
        return Ok(());
    };
    let Ok(engine_id) = std::env::var("WXCTL_TEST_WXD_PRESTO_ENGINE_ID") else {
        eprintln!("SKIP test_schema_crud: WXCTL_TEST_WXD_PRESTO_ENGINE_ID not set");
        return Ok(());
    };
    let storage_line = std::env::var("WXCTL_TEST_WXD_STORAGE_NAME").ok().map(|s| format!("storage_name: {s}\n")).unwrap_or_default();

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: schema
ref_name: wxctl_test_{safe_id}
name: wxctl_test_{safe_id}
custom_path: wxctl-test/{safe_id}
{storage_line}catalog_id: {catalog_id}
engine_id: {engine_id}
"#
    );

    LiveTest::new("test_schema_crud").profile("wxd").yaml(yaml).run_crud().await
}
