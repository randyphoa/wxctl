use super::LiveTest;
use wxctl_core::Config;
use wxctl_engine::ValidationError;

/// Valid agent YAML → validate() returns is_valid() == true.
#[tokio::test]
async fn test_validate_valid_config() -> anyhow::Result<()> {
    let yaml = r#"
kind: agent
ref_name: wxctl_test_valid
name: wxctl_test_valid
description: Valid agent for validation test
llm: groq/openai/gpt-oss-120b
"#;

    LiveTest::new("test_validate_valid_config")
        .timeout(60)
        .run(|ctx| async move {
            ctx.phase("validate", async {
                let mut config = Config::from_yaml(yaml)?;
                let result = ctx.client.validate(&mut config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                ctx.expect("validate", result.is_valid(), "valid", format!("invalid: {:?}", result.errors()))?;
                Ok(())
            })
            .await
        })
        .await
}

/// Agent without description → validate() returns is_valid() == false with MissingField.
#[tokio::test]
async fn test_validate_missing_required_field() -> anyhow::Result<()> {
    let yaml = r#"
kind: agent
ref_name: wxctl_test_invalid
name: wxctl_test_invalid
llm: groq/openai/gpt-oss-120b
"#;

    LiveTest::new("test_validate_missing_required_field")
        .timeout(60)
        .run(|ctx| async move {
            ctx.phase("validate", async {
                let mut config = Config::from_yaml(yaml)?;
                let result = ctx.client.validate(&mut config).await.map_err(|e| anyhow::anyhow!("{e}"))?;
                ctx.expect("validate", !result.is_valid(), "invalid", "valid")?;
                let has_missing = result.errors().iter().any(|e| matches!(&e.error, ValidationError::MissingField { field, .. } if field == "description"));
                ctx.expect("validate", has_missing, "MissingField(description)", format!("{:?}", result.errors()))?;
                Ok(())
            })
            .await
        })
        .await
}

/// YAML with unknown resource type → validate() returns invalid or error.
#[tokio::test]
async fn test_validate_unknown_resource_type() -> anyhow::Result<()> {
    let yaml = r#"
kind: nonexistent_xyz
ref_name: wxctl_test_unknown
name: wxctl_test_unknown
description: Should not be valid
"#;

    LiveTest::new("test_validate_unknown_resource_type")
        .timeout(60)
        .run(|ctx| async move {
            ctx.phase("validate", async {
                let mut config = Config::from_yaml(yaml)?;
                let result = ctx.client.validate(&mut config).await;
                match result {
                    Ok(validation) => {
                        ctx.expect("validate", !validation.is_valid(), "invalid", "valid")?;
                        let has_unknown = validation.errors().iter().any(|e| matches!(&e.error, ValidationError::UnknownResourceType { .. }));
                        ctx.expect("validate", has_unknown, "UnknownResourceType", format!("{:?}", validation.errors()))?;
                    }
                    Err(_) => {
                        // An error result is also acceptable — unknown types may fail before validation.
                    }
                }
                Ok(())
            })
            .await
        })
        .await
}
