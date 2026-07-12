//! Phase 5 deliverable: watsonx_data v3 CRUD lifecycle against an IBM Software
//! Hub 5.3.x cluster, plus R004 negative-path check for any kind marked
//! `unsupported_on: ["software"]`. Skips cleanly when `cp4d.watsonx_data`
//! is not configured in `~/.wxctl/test_profiles.json`.
//!
//! Run: `cargo test -p wxctl-sdk --features live-tests -- cpd_watsonx_data_v3`

use super::{LiveTest, read_profile_field, short_id};

#[tokio::test]
async fn test_cpd_watsonx_data_v3_supported_lifecycle() -> anyhow::Result<()> {
    if read_profile_field("cp4d", "watsonx_data", "instance_id")?.is_none() {
        eprintln!("SKIP test_cpd_watsonx_data_v3_supported_lifecycle: cp4d.watsonx_data.instance_id not set");
        return Ok(());
    }

    let safe_id = short_id();
    let yaml = format!(
        r#"
kind: spark_engine
ref_name: cpd_wxd_v3_{safe_id}_spark
display_name: cpd_wxd_v3_{safe_id}_spark
origin: native
type: spark
configuration:
    default_version: "3.5"
metadata:
    requires:
        deployment: "software-5.3.x"
"#
    );

    LiveTest::new("test_cpd_watsonx_data_v3_supported_lifecycle").profile("cp4d").timeout(900).yaml(yaml).skip_idempotency().run_crud().await
}

#[tokio::test]
async fn test_cpd_watsonx_data_unsupported_errors_r004() -> anyhow::Result<()> {
    use std::path::PathBuf;
    use wxctl_core::Config;
    use wxctl_sdk::WxctlClient;

    if read_profile_field("cp4d", "watsonx_data", "instance_id")?.is_none() {
        eprintln!("SKIP test_cpd_watsonx_data_unsupported_errors_r004: cp4d.watsonx_data.instance_id not set");
        return Ok(());
    }

    let yaml = r#"
kind: business_term
ref_name: cpd_wxd_unsupported
name: cpd_wxd_unsupported
short_description: R004 negative-path verification
metadata:
  requires:
    deployment: "software-5.3.x"
"#;

    let test_profiles: PathBuf = dirs::home_dir().unwrap().join(".wxctl/test_profiles.json");
    let client = WxctlClient::new("cp4d", test_profiles.to_str())?;
    let mut config = Config::from_yaml(yaml)?;
    let result = client.plan(&mut config).await;
    let err = match result {
        Err(e) => e,
        Ok(_) => anyhow::bail!("plan succeeded unexpectedly; expected R004 failure for business_term on software deployment"),
    };
    let msg = format!("{:#}", err);
    assert!(msg.contains("WXCTL-R004"), "expected WXCTL-R004 in error, got: {}", msg);
    assert!(msg.contains("not supported on"), "expected 'not supported on' phrase in error, got: {}", msg);

    Ok(())
}
