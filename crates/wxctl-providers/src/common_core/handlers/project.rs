use anyhow::Result;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::cos_discovery;

pub struct ProjectHandler;

impl ResourceHandler for ProjectHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            cos_discovery::ensure_project_storage(resource, client, operation_id).await?;
            Ok(HookOutcome::Continue)
        })
    }

    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // The /transactional/v2/projects POST returns only `{"location": "/v2/projects/{guid}"}`.
            // Dependents reference the project via `${project.ref_name}` → schema `field: guid`, and
            // the engine's extract_reference_field looks for guid at top level or under entity/metadata.
            // Without this, chained resources get the whole response object instead of the uuid string
            // and fail with "Cannot coerce complex JSON value to string".
            if response.get("guid").and_then(|v| v.as_str()).is_some() {
                return Ok(());
            }
            if let Some(guid) = response.get("location").and_then(|v| v.as_str()).and_then(extract_guid_from_location) {
                response["guid"] = json!(guid);
            }
            Ok(())
        })
    }
}

/// Parse the UUID segment out of a `/v2/projects/<guid>` location value.
fn extract_guid_from_location(location: &str) -> Option<String> {
    location.rsplit('/').find(|s| !s.is_empty()).map(|s| s.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // extract_guid_from_location pulls the trailing non-empty segment (tolerating a
    // trailing slash); an empty location yields None.
    #[test]
    fn extract_guid_from_location_cases() {
        let cases: &[(&str, Option<&str>)] = &[("/v2/projects/abc-123", Some("abc-123")), ("/v2/projects/abc-123/", Some("abc-123")), ("", None)];
        for (input, expected) in cases {
            assert_eq!(extract_guid_from_location(input), expected.map(str::to_string), "{input:?}");
        }
    }
}
