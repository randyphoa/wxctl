use super::{LiveTest, short_id};

/// OpenAPI tool expansion with wildcard filter (all endpoints) + bearer_token connection.
/// Creates 1 connection + 2 expanded tools (echoGet, echoPost). Tests create → idempotency → destroy.
#[tokio::test]
async fn test_openapi_tool_expansion_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let spec_path = format!("{}/tests/fixtures/openapi_tool_spec.yaml", env!("CARGO_MANIFEST_DIR"));
    let yaml = format!(
        r#"
kind: orchestrate_connection
ref_name: wxctl_test_conn_{safe_id}
app_id: wxctl_test_conn_{safe_id}
connection_type: bearer_token
environment: [draft]
preference: team
config_security_scheme: bearer_token
config_server_url: https://httpbin.org
credentials:
    token: test_token_{safe_id}
---
kind: tool
ref_name: wxctl_test_openapi_{safe_id}
name: wxctl_test_openapi_{safe_id}
permission: read_write
spec_path: {spec_path}
binding:
    openapi:
        tools: ["*"]
        connection_id: wxctl_test_conn_{safe_id}
"#
    );

    LiveTest::new("test_openapi_tool_expansion_crud").yaml(yaml).expect_resources(3).run_crud().await
}
