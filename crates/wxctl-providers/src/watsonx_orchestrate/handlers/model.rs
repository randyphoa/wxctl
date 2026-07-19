use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

pub struct ModelHandler;

/// camelCase keys the wxO models API returns on GET, mapped back to the
/// snake_case subfield names the schema declares (and that configs author).
const PROVIDER_CONFIG_KEY_MAP: [(&str, &str); 7] =
    [("customHost", "custom_host"), ("watsonxSpaceId", "watsonx_space_id"), ("watsonxProjectId", "watsonx_project_id"), ("watsonxDeploymentId", "watsonx_deployment_id"), ("apiBase", "api_base"), ("apiVersion", "api_version"), ("deploymentId", "deployment_id")];

/// Normalize a discovered `provider_config` to the schema's snake_case shape.
///
/// The wxO models API is asymmetric: it accepts snake_case keys on create but
/// returns camelCase on GET and adds a server-derived `provider` key. Drift
/// detection compares the whole `provider_config` object (it is a `state_field`),
/// so the desired snake_case config never equals the discovered camelCase+`provider`
/// one — every plan/apply reports a spurious `~provider_config` update and re-PATCHes
/// it. Rewriting the discovered keys to snake_case and dropping the derived `provider`
/// makes desired↔discovered match.
///
/// `provider` is dropped unconditionally because the AI-Gateway pattern (the only
/// place `provider_config` is set across the repo's configs) never sets it — the
/// server re-derives it from the model name. A config that sets `provider`
/// explicitly would need the general recursive-subset comparison instead.
fn normalize_provider_config(remote_data: &mut Value, operation_id: &str) {
    let Some(pc) = remote_data.pointer_mut("/provider_config").and_then(|v| v.as_object_mut()) else {
        return;
    };
    for (camel, snake) in PROVIDER_CONFIG_KEY_MAP {
        if let Some(value) = pc.remove(camel) {
            pc.insert(snake.to_string(), value);
        }
    }
    // Server-derived from the model name; absent from the desired AI-Gateway config.
    pc.remove("provider");

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "model",
        "normalized discovered provider_config to snake_case (dropped server-derived `provider`)"
    );
}

impl ResourceHandler for ModelHandler {
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            normalize_provider_config(remote_data, operation_id);
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // normalize_provider_config rewrites the GET-returned camelCase keys to snake_case and
    // drops the server-derived `provider` so desired↔discovered match (no spurious
    // ~provider_config update). Each case asserts the whole resulting object.
    #[test]
    fn normalize_provider_config_cases() {
        let cases: &[(&str, Value, Value)] = &[
            // Live SaaS shape (GET /v1/orchestrate/models): camelCase keys + derived `provider`,
            // plus sibling fields that must stay untouched.
            (
                "camelCase rewritten, provider dropped, siblings untouched",
                json!({"id": "abc-123", "name": "virtual-model/watsonx/openai/gpt-oss-120b", "provider_config": {"customHost": "https://us-south.ml.cloud.ibm.com", "watsonxSpaceId": "space-guid", "provider": "watsonx"}}),
                json!({"id": "abc-123", "name": "virtual-model/watsonx/openai/gpt-oss-120b", "provider_config": {"custom_host": "https://us-south.ml.cloud.ibm.com", "watsonx_space_id": "space-guid"}}),
            ),
            // Every key in PROVIDER_CONFIG_KEY_MAP is mapped.
            (
                "maps all known camelCase keys",
                json!({"provider_config": {"customHost": "h", "watsonxSpaceId": "s", "watsonxProjectId": "p", "watsonxDeploymentId": "d", "apiBase": "b", "apiVersion": "v", "deploymentId": "dep"}}),
                json!({"provider_config": {"custom_host": "h", "watsonx_space_id": "s", "watsonx_project_id": "p", "watsonx_deployment_id": "d", "api_base": "b", "api_version": "v", "deployment_id": "dep"}}),
            ),
            // Already snake_case → idempotent.
            ("already snake_case is idempotent", json!({"provider_config": {"custom_host": "h", "watsonx_space_id": "s"}}), json!({"provider_config": {"custom_host": "h", "watsonx_space_id": "s"}})),
            // No provider_config → whole resource untouched.
            ("missing provider_config is a no-op", json!({"id": "abc", "name": "m"}), json!({"id": "abc", "name": "m"})),
        ];
        for (msg, mut remote, expected) in cases.iter().map(|(m, r, e)| (*m, r.clone(), e.clone())) {
            normalize_provider_config(&mut remote, "test-op");
            assert_eq!(remote, expected, "{msg}");
        }
    }
}
