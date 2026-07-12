//! Phase 4 deliverable: watsonx_data core-surface CRUD lifecycle against an
//! IBM Software Hub 5.3.x cluster. Exercises `database_connection` (local-only)
//! + `database_registration` (covered in both watsonxdata-software.json and
//!   watsonxdata-v3.json — the only schema-driven kind in this batch with a
//!   working handler and no bootstrap dependencies).
//!
//! Skips cleanly when:
//!   - `cp4d` is not configured in `~/.wxctl/test_profiles.json`
//!   - the `watsonx_data` block is missing
//!   - the Db2 connection env vars are absent (set with WXCTL_TEST_CPD_DB2_*)
//!
//! Run: `cargo test -p wxctl-sdk --features live-tests -- cpd_watsonx_data_core`

use super::{LiveTest, read_profile_field, set_env_from_profile, short_id};

#[tokio::test]
async fn test_cpd_watsonx_data_core_lifecycle() -> anyhow::Result<()> {
    // Bail if the watsonx_data profile block isn't there at all.
    if read_profile_field("cp4d", "watsonx_data", "instance_id")?.is_none() {
        eprintln!("SKIP test_cpd_watsonx_data_core_lifecycle: cp4d.watsonx_data.instance_id not set");
        return Ok(());
    }

    // Source Db2 credentials from the test profile (mirrors database_registration.rs).
    // The test profile's `db2.*` block is the same one the SaaS database_registration
    // test uses; reuse it under cp4d so a single Db2 target backs both.
    let missing = set_env_from_profile("cp4d", &[("WXCTL_TEST_CPD_DB2_HOST", "db2", "host"), ("WXCTL_TEST_CPD_DB2_DATABASE", "db2", "database"), ("WXCTL_TEST_CPD_DB2_USERNAME", "db2", "username"), ("WXCTL_TEST_CPD_DB2_PASSWORD", "db2", "password")])?;
    if let Some(field) = missing {
        eprintln!("SKIP test_cpd_watsonx_data_core_lifecycle: cp4d profile missing {field}");
        return Ok(());
    }
    let Some(port) = read_profile_field("cp4d", "db2", "port")? else {
        eprintln!("SKIP test_cpd_watsonx_data_core_lifecycle: cp4d profile missing db2.port");
        return Ok(());
    };

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: database_connection
ref_name: cpd_wxd_core_{safe_id}_dbconn
metadata:
    requires:
        deployment: "software-5.3.x"
type: db2
hostname: ${{env:WXCTL_TEST_CPD_DB2_HOST}}
port: {port}
name: ${{env:WXCTL_TEST_CPD_DB2_DATABASE}}
username: ${{env:WXCTL_TEST_CPD_DB2_USERNAME}}
password: ${{env:WXCTL_TEST_CPD_DB2_PASSWORD}}
ssl: true

---
kind: database_registration
ref_name: cpd_wxd_core_{safe_id}_dbreg
metadata:
    requires:
        deployment: "software-5.3.x"
display_name: cpd-wxd-core-{safe_id}
description: Phase 4 cpd_watsonx_data_core live test database registration
connection: ${{database_connection.cpd_wxd_core_{safe_id}_dbconn}}
associated_catalog:
    catalog_name: cpd_wxd_core_{safe_id}
    catalog_type: db2
"#
    );

    LiveTest::new("test_cpd_watsonx_data_core_lifecycle").profile("cp4d").yaml(yaml).run_crud().await
}
