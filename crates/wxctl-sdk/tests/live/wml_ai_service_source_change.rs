use super::run_source_change_test;

#[tokio::test]
async fn test_wml_ai_service_source_change() -> anyhow::Result<()> {
    run_source_change_test(
        "ai_service",
        "aisrc",
        "ai_service.py",
        "def deployable_ai_service(context):\n    def generate(context) -> dict:\n        payload = context.get_json()\n        return {\"body\": payload}\n    def generate_stream(context):\n        yield generate(context)\n    return generate, generate_stream\n",
        "def deployable_ai_service(context):\n    def generate(context) -> dict:\n        payload = context.get_json()\n        return {\"body\": {\"updated\": True}}\n    def generate_stream(context):\n        yield generate(context)\n    return generate, generate_stream\n",
        r#"kind: ai_service
ref_name: wxctl_test_aisrc_{safe_id}
name: wxctl-test-aisrc-{safe_id}
software_spec: ${software_specification.wxctl_test_aisrc_swspec_{safe_id}}
space_id: ${space.wxctl_test_aisrc_{safe_id}}
source_path: {source_path_str}"#,
    )
    .await
}
