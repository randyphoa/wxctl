//! `database_registration` handler — walks the `connection:` edge to
//! assemble the v3 `DatabaseRegistrationPrototype` wire body. Schema no
//! longer carries an inline `connection:` object; the linked
//! `database_connection` supplies `type:` (via its discriminator) and
//! every `connection.*` sub-field.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value};
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::catalog_cascade::cascade_from_registration;
use super::registration_adopt::adopt_registration_on_conflict;
use super::registration_normalize::{backfill_associated_catalog, backfill_connection_username_from_properties, backfill_db_catalog_type};
use crate::util::{REF_CONNECTION, REF_PREFIX, require_ref};

const REGISTRATIONS_PATH: &str = "/v3/database_registrations";

pub struct DatabaseRegistrationHandler;

impl ResourceHandler for DatabaseRegistrationHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = assemble_create_body(resource)?;
            let spec = RequestSpec::new(Method::POST, REGISTRATIONS_PATH).body(BodyKind::Json(body));
            let mut response: Value = client.execute(operation_id, spec).await?;
            // post_discover is skipped for HookOutcome::Handled, so run the
            // same normalizers inline — downstream engines' template refs
            // read the top-level `catalog_name` this backfills.
            normalize_response(&mut response);
            Ok(HookOutcome::Handled(response))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            normalize_response(remote_data);
            Ok(())
        })
    }

    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(adopt_registration_on_conflict(resource, error, client, operation_id, REGISTRATIONS_PATH))
    }

    fn post_delete<'a>(&'a self, resource: &'a Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(cascade_from_registration(resource, client, operation_id))
    }
}

fn normalize_response(response: &mut Value) {
    backfill_associated_catalog(response);
    backfill_db_catalog_type(response);
    backfill_connection_username_from_properties(response);
}

/// Build the v3 body from the user-facing resource + the engine-injected
/// `__ref__connection`. The connection's `type:` drives every variant
/// field; the handler copies the entire connection spec (minus the
/// engine/internal keys `type` / `ref_name` / `kind` / `metadata`) into
/// the `connection:` object on the wire body.
fn assemble_create_body(resource: &Value) -> Result<Value> {
    let connection = require_ref(resource, REF_CONNECTION)?;

    let conn_type = connection.get("type").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("linked database_connection missing required 'type' field"))?;

    // Assemble the wire `connection:` block from every field on the
    // resolved connection EXCEPT the discriminator (`type` — belongs at
    // top level) and engine/internal keys (`ref_name`, `kind`, and
    // `metadata` — e.g. a `requires.deployment` gate declared on the
    // connection — plus any deeper `__ref__*` enrichment keys). The
    // watsonx.data API rejects unknown fields, so a leaked `metadata`
    // returns HTTP 400 ("unknown field \"metadata\"").
    let mut connection_block = Map::new();
    if let Some(conn_obj) = connection.as_object() {
        for (key, value) in conn_obj {
            if key == "type" || key == "ref_name" || key == "kind" || key == "metadata" || key.starts_with(REF_PREFIX) {
                continue;
            }
            connection_block.insert(key.clone(), value.clone());
        }
    }

    let mut body = Map::new();
    body.insert("type".to_string(), Value::String(conn_type.to_string()));
    body.insert("connection".to_string(), Value::Object(connection_block));

    for field in ["display_name", "description", "tags", "associated_catalog"] {
        if let Some(v) = resource.get(field) {
            body.insert(field.to_string(), v.clone());
        }
    }

    Ok(Value::Object(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn assembles_db2_body_from_connection_edge() {
        let resource = json!({
            "connection": "db2_prod",
            "display_name": "wxctl-db2",
            "associated_catalog": {"catalog_name": "db2_catalog", "catalog_type": "db2"},
            "__ref__connection": {
                // Engine enrichment copies the FULL resolved connection,
                // including the internal keys below. The handler must strip
                // them — the live watsonx.data API 400s on unknown fields
                // (regression: a leaked `metadata` deployment gate).
                "kind": "database_connection",
                "ref_name": "db2_prod",
                "metadata": {"requires": {"deployment": ["saas", "software-5.3.x"]}},
                "type": "db2",
                "hostname": "db2.example.com",
                "port": 31030,
                "name": "bludb",
                "username": "u",
                "password": "p",
                "ssl": true
            }
        });

        let body = assemble_create_body(&resource).unwrap();
        assert_eq!(body["type"], "db2");
        assert_eq!(body["connection"]["hostname"], "db2.example.com");
        assert_eq!(body["connection"]["port"], 31030);
        assert_eq!(body["connection"]["name"], "bludb");
        assert_eq!(body["connection"]["username"], "u");
        assert_eq!(body["connection"]["password"], "p");
        assert_eq!(body["connection"]["ssl"], true);
        // Internal/engine keys must NOT leak into the connection block —
        // the API rejects unknown fields.
        assert!(body["connection"].get("type").is_none());
        assert!(body["connection"].get("ref_name").is_none());
        assert!(body["connection"].get("kind").is_none());
        assert!(body["connection"].get("metadata").is_none());
    }

    #[test]
    fn errors_when_connection_enrichment_missing() {
        let resource = json!({"connection": "db2_prod"});
        assert!(assemble_create_body(&resource).is_err());
    }
}
