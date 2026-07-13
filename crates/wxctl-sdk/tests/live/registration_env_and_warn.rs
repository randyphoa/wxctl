//! Thin coverage tests for cross-cutting features that ship with the
//! registration schemas: env-var interpolation failure (`WXCTL-V301`)
//! and the `soft_allowed_values` warn path (`WXCTL-V401`). Both complete
//! locally without live service calls.
//!
//! After the 2026-04-20 refactor, the discriminator / sensitive fields
//! live on the connection kinds (`storage_connection`,
//! `database_connection`), so the env-var interpolation test targets
//! one of those.

use wxctl_core::Config;

/// Missing `${env:...}` references fail at parse time with `WXCTL-V301`
/// before any HTTP call. No profile / network access needed.
#[tokio::test]
async fn test_env_var_missing_fails_validation() {
    let yaml = r#"
kind: storage_connection
ref_name: wxctl_test_missing_env
type: ibm_cos
access_key: ${env:WXCTL_TEST_UNSET_VAR_DO_NOT_SET}
secret_key: ${env:WXCTL_TEST_UNSET_VAR_DO_NOT_SET}
"#;
    unsafe { std::env::remove_var("WXCTL_TEST_UNSET_VAR_DO_NOT_SET") };
    let err = Config::from_yaml(yaml).expect_err("missing env var must fail");
    let msg = err.to_string();
    assert!(msg.contains("WXCTL-V301"), "expected WXCTL-V301, got: {msg}");
    assert!(msg.contains("WXCTL_TEST_UNSET_VAR_DO_NOT_SET"), "expected var name in error, got: {msg}");
}

/// Unknown `database_connection.type` values must pass validation while
/// emitting a `WXCTL-V401` warn event. The assertion is that parsing
/// succeeds — the warn is observable via `test-logs.jsonl`.
#[tokio::test]
async fn test_soft_warn_unknown_database_type() -> anyhow::Result<()> {
    let yaml = r#"
kind: database_connection
ref_name: wxctl_test_soft_warn
type: not_a_real_connector
hostname: example.com
port: 1234
name: db
username: u
password: p
"#;
    let _config = Config::from_yaml(yaml)?;
    Ok(())
}
