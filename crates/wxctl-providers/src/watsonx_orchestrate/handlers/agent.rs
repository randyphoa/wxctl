use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::traits::ResourceHandler;

pub struct AgentHandler;

/// Inject `"state": "active"` into each starter prompt item that lacks it.
///
/// The UI always sets `state: "active"` on every prompt, but the server stores
/// `state: null` when it is omitted, which prevents prompts from rendering.
fn inject_starter_prompt_state(resource: &mut Value, operation_id: &str) {
    let items = resource.get_mut("additional_properties").and_then(|ap| ap.get_mut("starter_prompts")).and_then(|sp| sp.get_mut("customize")).and_then(|c| c.as_array_mut());

    if let Some(prompts) = items {
        for prompt in prompts {
            if let Some(obj) = prompt.as_object_mut()
                && (!obj.contains_key("state") || obj.get("state") == Some(&Value::Null))
            {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "agent",
                    "injecting state=active into starter prompt"
                );
                obj.insert("state".to_string(), Value::String("active".to_string()));
            }
        }
    }
}

impl ResourceHandler for AgentHandler {
    fn post_validate<'a>(&'a self, resource: &'a mut Value, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            inject_starter_prompt_state(resource, operation_id);
            Ok(())
        })
    }
}
