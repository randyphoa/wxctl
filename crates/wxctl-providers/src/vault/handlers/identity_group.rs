//! `vault_identity_group` handler.
//!
//! `post_create`: unwrap Vault's `data` envelope on the create response so the
//! server-assigned canonical `id` (returned at `data.id`) surfaces at the top level and
//! `${vault_identity_group.<ref>.id}` resolves for a downstream vault_group_alias.
//!
//! `post_discover`: unwrap the `data` envelope on discovery, same as every vault kind.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

pub struct IdentityGroupHandler;

impl ResourceHandler for IdentityGroupHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            super::envelope::unwrap_data_envelope(response);
            Ok(())
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            super::envelope::unwrap_data_envelope(remote_data);
            Ok(())
        })
    }
}
