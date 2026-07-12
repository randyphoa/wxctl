use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::code_upload;

const API_PATH: &str = "/ml/v4/ai_services";

pub struct AiServiceHandler;

impl ResourceHandler for AiServiceHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            code_upload::wrap_software_spec(resource, operation_id);
            Ok(HookOutcome::Continue)
        })
    }

    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { code_upload::upload_code(resource, response, client, API_PATH, operation_id).await })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let response = code_upload::patch_and_upload(current, desired, client, API_PATH, operation_id).await?;
            Ok(HookOutcome::Handled(response))
        })
    }

    fn post_validate<'a>(&'a self, resource: &'a mut Value, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { code_upload::hash_and_tag_source(resource) })
    }
}
