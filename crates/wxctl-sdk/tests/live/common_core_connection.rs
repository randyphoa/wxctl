use super::{LiveTest, short_id};

/// Verify common_core_connection CRUD against api.dataplatform.cloud.ibm.com/v2/connections.
/// Exercises the service routing fix (watsonx_data → common_core) from commit 136bd56 — this
/// is the rename-adjacent coverage gap called out in the 2026-04-19 orchestrate-ai handover.
///
/// The /v2/connections API requires one of catalog_id/project_id/space_id. Chains through
/// project to cover the project post_create guid-extraction path as well.
#[tokio::test]
async fn test_common_core_connection_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: project
ref_name: wxctl_test_{safe_id}_proj
name: wxctl_test_{safe_id}_proj
description: wxctl live integration test project
type: wx
---
kind: common_core_connection
ref_name: wxctl_test_{safe_id}_conn
name: wxctl_test_{safe_id}_conn
datasource_type: postgresql
description: wxctl live integration test connection
project_id: ${{project.wxctl_test_{safe_id}_proj}}
test: "false"
properties:
    host: db.example.com
    port: "5432"
    database: testdb
    username: testuser
    password: testpass
    ssl: "false"
"#
    );

    LiveTest::new("test_common_core_connection_crud").yaml(yaml).run_crud().await
}
