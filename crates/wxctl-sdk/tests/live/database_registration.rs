use super::{LiveTest, read_profile_field, set_env_from_profile, short_id};

/// database_registration CRUD against the itz Db2 warehouse.
///
/// After the 2026-04-20 refactor, the registration references a
/// `database_connection` rather than inlining the connection block.
/// Sensitive credentials come from `test_profiles.json` `wxd.db2.*` via
/// env-var interpolation; the numeric `port` is baked in directly
/// because interpolation yields strings and `port` is integer-typed.
#[tokio::test]
async fn test_database_registration_lifecycle() -> anyhow::Result<()> {
    let missing = set_env_from_profile("wxd", &[("WXCTL_TEST_DB2_HOST", "db2", "host"), ("WXCTL_TEST_DB2_DATABASE", "db2", "database"), ("WXCTL_TEST_DB2_USERNAME", "db2", "username"), ("WXCTL_TEST_DB2_PASSWORD", "db2", "password")])?;
    if let Some(field) = missing {
        eprintln!("SKIP test_database_registration_lifecycle: profile missing {field}");
        return Ok(());
    }
    let Some(port) = read_profile_field("wxd", "db2", "port")? else {
        eprintln!("SKIP test_database_registration_lifecycle: profile missing db2.port");
        return Ok(());
    };

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: database_connection
ref_name: db2_{safe_id}
type: db2
hostname: ${{env:WXCTL_TEST_DB2_HOST}}
port: {port}
name: ${{env:WXCTL_TEST_DB2_DATABASE}}
username: ${{env:WXCTL_TEST_DB2_USERNAME}}
password: ${{env:WXCTL_TEST_DB2_PASSWORD}}
ssl: true

---
kind: database_registration
ref_name: wxctl_test_{safe_id}
display_name: wxctl-test-{safe_id}
description: wxctl database_registration CRUD test
connection: ${{database_connection.db2_{safe_id}}}
associated_catalog:
  catalog_name: wxctl_test_{safe_id}
  catalog_type: db2
"#
    );

    LiveTest::new("test_database_registration_lifecycle").profile("wxd").timeout(300).yaml(yaml).run_crud().await
}
