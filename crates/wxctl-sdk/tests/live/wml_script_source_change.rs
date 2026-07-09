use super::run_source_change_test;

#[tokio::test]
async fn test_wml_script_source_change() -> anyhow::Result<()> {
    run_source_change_test(
        "wml_script",
        "scsrc",
        "script.py",
        "def score(payload):\n    return {\"predictions\": [{\"values\": [[1]]}]}\n",
        "def score(payload):\n    return {\"predictions\": [{\"values\": [[2]]}]}\n",
        r#"kind: wml_script
ref_name: wxctl_test_scsrc_{safe_id}
name: wxctl-test-scsrc-{safe_id}
software_spec: ${software_specification.wxctl_test_scsrc_swspec_{safe_id}}
space_id: ${space.wxctl_test_scsrc_{safe_id}}
source_path: {source_path_str}"#,
    )
    .await
}
