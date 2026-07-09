use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

pub struct CatalogHandler;

impl ResourceHandler for CatalogHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let guid = response.get("metadata").and_then(|m| m.get("guid")).and_then(|g| g.as_str()).ok_or_else(|| anyhow::anyhow!("No GUID in response metadata"))?;

            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %_operation_id,
                guid = %guid,
                "Catalog created successfully"
            );
            Ok(())
        })
    }
}
