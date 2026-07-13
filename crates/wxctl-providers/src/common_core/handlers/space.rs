use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::cos_discovery;
use super::wml_discovery;

pub struct SpaceHandler;

/// Poll a space until its status reaches "active" (or timeout).
/// The space API returns 202 on create with status "preparing". Dependent
/// resources (software_specification, etc.) cannot be used until the space
/// catalog is initialized.
async fn wait_for_space_active(client: &HttpClient, space_id: &str, operation_id: &str) -> Result<()> {
    let max_attempts = 30;

    crate::util::poll_until(max_attempts, std::time::Duration::from_secs(5), crate::util::PollTimeout::Bail(format!("[{operation_id}] Timed out waiting for space {space_id} to become active")), None::<String>, |attempt, mut prev_state| async move {
        let spec = RequestSpec::new(Method::GET, format!("/v2/spaces/{space_id}")).body(BodyKind::None);
        let response: Value = client.execute(operation_id, spec).await?;

        let state = response.pointer("/entity/status/state").or_else(|| response.pointer("/status/state")).and_then(|v| v.as_str()).unwrap_or("unknown");

        if prev_state.as_deref() != Some(state) {
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "space",
                space_id = %space_id,
                status = %state,
                attempt = attempt,
                max_attempts = max_attempts,
                "space status observed"
            );
            prev_state = Some(state.to_string());
        }

        let outcome = match state {
            "active" => crate::util::PollOutcome::Done(Value::Null),
            "failed" => crate::util::PollOutcome::Failed(format!("[{operation_id}] Space {space_id} creation failed")),
            _ => crate::util::PollOutcome::Pending,
        };
        Ok((outcome, prev_state))
    })
    .await
    .map(|_| ())
}

impl ResourceHandler for SpaceHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            cos_discovery::ensure_space_storage(resource, client, operation_id).await?;
            wml_discovery::ensure_space_compute(resource, client, operation_id).await?;
            Ok(HookOutcome::Continue)
        })
    }

    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let space_id = crate::util::resource_id(response);

            let Some(space_id) = space_id else {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "space",
                    reason = "missing_id_in_create_response",
                    "skipping readiness check"
                );
                return Ok(());
            };

            wait_for_space_active(client, space_id, operation_id).await
        })
    }
}
