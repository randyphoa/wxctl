use super::{LiveTest, short_id};

/// Verify oauth2_client_creds connection CRUD: create → idempotency → destroy.
/// Exercises the most complex config path with config_auth_type and SSO fields.
#[tokio::test]
async fn test_connection_oauth2_client_creds_crud() -> anyhow::Result<()> {
    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: orchestrate_connection
ref_name: wxctl_test_{safe_id}
app_id: wxctl_test_{safe_id}
connection_type: oauth2_client_creds
environment: draft
preference: team
config_security_scheme: oauth2
config_auth_type: oauth2_client_creds
config_server_url: https://api.example.com
credentials:
    client_id: test_client_{safe_id}
    client_secret: test_secret_{safe_id}
    token_url: https://auth.example.com/oauth/token
    grant_type: client_credentials
    send_via: header
"#
    );

    LiveTest::new("test_connection_oauth2_client_creds_crud").yaml(yaml).run_crud().await
}
