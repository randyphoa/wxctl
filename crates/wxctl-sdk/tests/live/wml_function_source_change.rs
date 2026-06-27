use super::run_source_change_test;

#[tokio::test]
async fn test_wml_function_source_change() -> anyhow::Result<()> {
    run_source_change_test(
        "wml_function",
        "fnsrc",
        "function.py",
        "def score(payload):\n    return {\"predictions\": [{\"values\": [[1]]}]}\n",
        "def score(payload):\n    return {\"predictions\": [{\"values\": [[2]]}]}\n",
        r#"kind: wml_function
ref_name: wxctl_test_fnsrc_{safe_id}
name: wxctl-test-fnsrc-{safe_id}
software_spec: ${software_specification.wxctl_test_fnsrc_swspec_{safe_id}}
space_id: ${space.wxctl_test_fnsrc_{safe_id}}
source_path: {source_path_str}"#,
    )
    .await
}
