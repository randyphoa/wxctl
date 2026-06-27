use anyhow::{Result, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct SoftwareSpecificationHandler;

impl ResourceHandler for SoftwareSpecificationHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            resolve_base_spec(resource, client, operation_id).await?;
            resolve_package_extensions(resource, operation_id);
            Ok(HookOutcome::Continue)
        })
    }
}

/// Resolve `base_software_specification` name to `{"guid": "..."}` object.
async fn resolve_base_spec(resource: &mut Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    let base_name = resource.get("base_software_specification").and_then(|v| v.as_str()).map(|s| s.to_string());

    let Some(base_name) = base_name else {
        bail!("[{operation_id}] base_software_specification is required");
    };

    let mut spec = RequestSpec::new(Method::GET, "/v2/software_specifications").body(BodyKind::None);
    if let Some(space_id) = resource.get("space_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("space_id", space_id);
    }
    if let Some(project_id) = resource.get("project_id").and_then(|v| v.as_str()) {
        spec = spec.query_param("project_id", project_id);
    }

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "software_specification",
        base_name = %base_name,
        "resolving base software specification"
    );

    let body: Value = client.execute(operation_id, spec).await?;

    let empty = vec![];
    let resources = body.get("resources").and_then(|v| v.as_array()).unwrap_or(&empty);

    let base_guid = resources.iter().find_map(|r| {
        let name = r.pointer("/metadata/name").or_else(|| r.pointer("/entity/software_specification/name")).and_then(|v| v.as_str());
        if name == Some(&base_name) { r.pointer("/metadata/asset_id").and_then(|v| v.as_str()).map(|s| s.to_string()) } else { None }
    });

    let Some(guid) = base_guid else {
        bail!("[{operation_id}] Base software specification '{base_name}' not found. Available: {}", resources.iter().filter_map(|r| r.pointer("/metadata/name").and_then(|v| v.as_str())).collect::<Vec<_>>().join(", "));
    };

    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        resource_type = "software_specification",
        base_name = %base_name,
        guid = %guid,
        "resolved base software specification"
    );

    resource.as_object_mut().unwrap().remove("base_software_specification");
    resource["base_software_specification"] = json!({"guid": guid});

    Ok(())
}

/// Convert `package_extensions` from array of resolved IDs to array of `{"guid": "..."}` objects.
fn resolve_package_extensions(resource: &mut Value, operation_id: &str) {
    if let Some(extensions) = resource.get("package_extensions").and_then(|v| v.as_array()).cloned() {
        let guid_objects: Vec<Value> = extensions
            .iter()
            .filter_map(|v| {
                v.as_str().map(|id| {
                    tracing::debug!(
                        target: "wxctl::substage::provider",
                        operation_id = %operation_id,
                        resource_type = "software_specification",
                        package_extension_guid = %id,
                        "adding package extension reference"
                    );
                    json!({"guid": id})
                })
            })
            .collect();

        if !guid_objects.is_empty() {
            resource["package_extensions"] = json!(guid_objects);
        }
    }
}
