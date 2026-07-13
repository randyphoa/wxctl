//! `vault_auth_method` handler.
//!
//! `post_create`: after the default enable POST (`POST /v1/sys/auth/{path}`, which 204s),
//! issue the JWT-config sub-write (`POST /v1/auth/{path}/config`) carrying the LocalOnly
//! OIDC fields, then read back the mount `accessor` (`GET /v1/sys/auth/{path}`, data-unwrapped)
//! and stamp it onto the create response so `${vault_auth_method.<ref>.accessor}` resolves.
//!
//! `post_discover`: unwrap Vault's top-level `data` envelope (surfaces `accessor` on re-plan).

use anyhow::{Result, anyhow};
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

pub struct AuthMethodHandler;

/// Fields that ride the JWT-config sub-write (LocalOnly on the schema, excluded from
/// the enable body). Only non-null present values are forwarded.
const CONFIG_FIELDS: &[&str] = &["oidc_discovery_url", "bound_issuer", "default_role"];

impl ResourceHandler for AuthMethodHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let path = resource.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] vault_auth_method post_create: missing `path`"))?.to_string();

            // JWT-config sub-write: POST /v1/auth/{path}/config with the OIDC fields.
            let mut config = Map::new();
            for key in CONFIG_FIELDS {
                if let Some(v) = resource.get(*key).filter(|v| !v.is_null()) {
                    config.insert((*key).to_string(), v.clone());
                }
            }
            if !config.is_empty() {
                let _: Value = client.create(operation_id, &format!("/v1/auth/{path}/config"), Value::Object(config)).await?;
            }

            // Read back the mount accessor and stamp it onto the response for refs.
            let mut mount: Value = client.get(operation_id, &format!("/v1/sys/auth/{path}")).await?;
            super::envelope::unwrap_data_envelope(&mut mount);
            if let Some(accessor) = mount.get("accessor").and_then(|v| v.as_str()).map(str::to_string) {
                match response {
                    Value::Object(map) => {
                        map.insert("accessor".to_string(), Value::String(accessor));
                    }
                    _ => *response = Value::Object(Map::from_iter([("accessor".to_string(), Value::String(accessor))])),
                }
            }
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
