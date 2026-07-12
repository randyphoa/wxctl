//! `vault_group_alias` handler.
//!
//! Vault has no lookup-of-a-group-alias-by-name endpoint (aliases are addressed only by
//! their server-minted id), so a re-created alias 400s with "combination of mount and
//! group alias name is already in use". Discovery therefore goes through the PARENT
//! identity group, which embeds its single alias: `GET /identity/group/id/{canonical_id}`
//! returns `data.alias = {id, name, mount_accessor}` (or `data.alias.id: null` when the
//! group has no alias yet, matched by the schema's `absent_when`).
//!
//! `post_discover`: lift `data.alias` to the top level so the reconciler compares/deletes
//! against the alias (its own `id`, `name`, `mount_accessor`) rather than the group body.
//!
//! `post_create`: `POST /identity/group-alias` returns `{data: {id, canonical_id}}`; unwrap
//! the `data` envelope so the created state matches the discovered (lifted-alias) shape.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

pub struct GroupAliasHandler;

impl ResourceHandler for GroupAliasHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            super::envelope::unwrap_data_envelope(response);
            Ok(())
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Discovery GETs the parent group; its `data.alias` object IS this resource.
            if let Some(alias) = remote_data.get("data").and_then(|d| d.get("alias")).filter(|a| a.is_object()).cloned() {
                *remote_data = alias;
            } else {
                // Already-unwrapped, or the create response — fall back to the shared
                // `data`-envelope unwrap so the shape is consistent either way.
                super::envelope::unwrap_data_envelope(remote_data);
            }
            Ok(())
        })
    }
}
