use super::{LiveTest, short_id};

/// Verify basic_auth connection CRUD: create → idempotency → destroy.
#[tokio::test]
async fn test_connection_basic_auth_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: orchestrate_connection
ref_name: wxctl_test_{safe_id}
app_id: wxctl_test_{safe_id}
connection_type: basic_auth
environment: [draft]
preference: team
config_security_scheme: basic_auth
config_server_url: https://example.service-now.com
credentials:
    username: test_user_{safe_id}
    password: test_pass_{safe_id}
"#
    );

    LiveTest::new("test_connection_basic_auth_crud").yaml(yaml).run_crud().await
}
