//! `vault_secret_engine` handler.
//!
//! `post_create`: after the default mount enable (`POST /v1/sys/mounts/{path}`, body
//! `{"type":"database"}`, which 204s), issue the database connection-config sub-write
//! (`POST /v1/{path}/config/{db_name}`) carrying the LocalOnly connection fields. The
//! `password` path is marked sensitive on the sub-write's `RequestSpec` so it is redacted
//! at emission (`RequestSpec.sensitive_paths`; pa_user's write-only `Password` precedent).
//! `verify_connection: false` is forwarded verbatim so Vault skips the live database
//! reachability check (mount + config still created); true (or absent, Vault's default)
//! fails the sub-write against an unreachable database.
//!
//! `post_discover`: unwrap Vault's top-level `data` envelope.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::traits::ResourceHandler;

pub struct SecretEngineHandler;

/// Fields that ride the connection-config sub-write body (LocalOnly on the schema,
/// excluded from the mount enable body). `db_name` is NOT here — it fills the {db_name}
/// endpoint segment. Only non-null present values are forwarded.
const CONFIG_FIELDS: &[&str] = &["plugin_name", "connection_url", "username", "password", "allowed_roles", "verify_connection"];

impl ResourceHandler for SecretEngineHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, _response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let path = resource.get("path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] vault_secret_engine post_create: missing `path`"))?.to_string();
            let db_name = resource.get("db_name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] vault_secret_engine post_create: missing `db_name`"))?.to_string();

            // Connection-config sub-write: POST /v1/{path}/config/{db_name}.
            let mut config = Map::new();
            for key in CONFIG_FIELDS {
                if let Some(v) = resource.get(*key).filter(|v| !v.is_null()) {
                    config.insert((*key).to_string(), v.clone());
                }
            }
            // `password` marked sensitive so its value is redacted at emission.
            let spec = RequestSpec::new(Method::POST, format!("/v1/{path}/config/{db_name}")).body(BodyKind::Json(Value::Object(config))).sensitive_paths(vec!["password".to_string()]);
            let _: Value = client.execute(operation_id, spec).await?;
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
