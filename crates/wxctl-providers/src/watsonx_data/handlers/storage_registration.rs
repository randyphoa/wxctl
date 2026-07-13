//! `storage_registration` handler — walks DAG edges at apply time to
//! assemble the v3 `StorageRegistrationPrototype` wire body. The schema
//! no longer carries an inline `connection:` object or user-set `type`;
//! instead, `bucket:` references an `s3_bucket` / `adls_container` /
//! `gcs_bucket`, and the engine injects the linked bucket + its
//! `storage_connection` under `__ref__bucket.__ref__connection`.

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
use super::registration_normalize::backfill_associated_catalog;
use crate::util::{REF_BUCKET, REF_CONNECTION, require_ref};

const REGISTRATIONS_PATH: &str = "/v3/storage_registrations";

pub struct StorageRegistrationHandler;

impl ResourceHandler for StorageRegistrationHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let body = assemble_create_body(resource)?;
            let spec = RequestSpec::new(Method::POST, REGISTRATIONS_PATH).body(BodyKind::Json(body));
            let mut response: Value = client.execute(operation_id, spec).await?;
            // post_discover/post_create are skipped for HookOutcome::Handled, so run
            // the same normalization inline here before the response lands in the
            // runtime store — matches the pattern in database_registration.
            backfill_associated_catalog(&mut response);
            Ok(HookOutcome::Handled(response))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            backfill_associated_catalog(remote_data);
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

/// Build the v3 `StorageRegistrationPrototype` body from the user-facing
/// resource data + the engine-injected `__ref__bucket` /
/// `__ref__bucket.__ref__connection`. The schema ships only user-facing
/// fields; this function fills in `type`, `connection`, and `region`
/// derived from the DAG.
fn assemble_create_body(resource: &Value) -> Result<Value> {
    let bucket = require_ref(resource, REF_BUCKET)?;
    let connection = require_ref(bucket, REF_CONNECTION)?;

    let conn_type = connection.get("type").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("linked storage_connection missing required 'type' field"))?;

    let connection_block = assemble_connection_block(conn_type, bucket, connection)?;

    let mut body = Map::new();
    body.insert("type".to_string(), Value::String(conn_type.to_string()));
    body.insert("connection".to_string(), Value::Object(connection_block));
    if let Some(region) = bucket.get("region").and_then(|v| v.as_str()) {
        body.insert("region".to_string(), Value::String(region.to_string()));
    }

    for field in ["display_name", "description", "managed_by", "tags", "associated_catalog"] {
        if let Some(v) = resource.get(field) {
            body.insert(field.to_string(), v.clone());
        }
    }

    Ok(Value::Object(body))
}

/// Build the `StorageDetails` `connection` object per backend family. S3
/// families carry endpoint + HMAC creds; ADLS gen2 carries account +
/// container (+ optional principal); ADLS gen1 carries account only; GCS
/// carries a key file. Field set is dictated by `watsonxdata-v3.json`
/// `StorageDetails`.
fn assemble_connection_block(conn_type: &str, bucket: &Value, connection: &Value) -> Result<Map<String, Value>> {
    let mut block = Map::new();
    match conn_type {
        "adls_gen2" => {
            let account_name = require_conn_str(connection, "account_name")?;
            block.insert("account_name".to_string(), Value::String(account_name));
            if let Some(container) = connection.get("container_name").and_then(|v| v.as_str()) {
                block.insert("container_name".to_string(), Value::String(container.to_string()));
            }
            for opt in ["sas_token", "application_id", "directory_id"] {
                if let Some(v) = connection.get(opt).and_then(|v| v.as_str()) {
                    block.insert(opt.to_string(), Value::String(v.to_string()));
                }
            }
        }
        "adls_gen1" => {
            let account_name = require_conn_str(connection, "account_name")?;
            block.insert("account_name".to_string(), Value::String(account_name));
        }
        "google_cs" => {
            // The user-facing field is `service_account_json`; the v3 wire
            // field is `key_file`. Map one onto the other (no redundant
            // `key_file` on the connection schema).
            let key_file = require_conn_str(connection, "service_account_json")?;
            block.insert("key_file".to_string(), Value::String(key_file));
        }
        // S3 family (aws_s3, ibm_cos, minio, ibm_ceph, amazon_s3, s3) and
        // any unrecognised type fall through to the endpoint + HMAC shape.
        _ => {
            let bucket_name = bucket.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("linked bucket missing 'name' field"))?;
            let endpoint = bucket.get("endpoint").and_then(|v| v.as_str()).or_else(|| connection.get("endpoint").and_then(|v| v.as_str())).ok_or_else(|| anyhow!("cannot derive connection.endpoint — neither bucket nor storage_connection provides one"))?;
            block.insert("name".to_string(), Value::String(bucket_name.to_string()));
            block.insert("endpoint".to_string(), Value::String(endpoint.to_string()));
            if let Some(ak) = connection.get("access_key").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                block.insert("access_key".to_string(), Value::String(ak.to_string()));
            }
            if let Some(sk) = connection.get("secret_key").and_then(|v| v.as_str()).filter(|s| !s.is_empty()) {
                block.insert("secret_key".to_string(), Value::String(sk.to_string()));
            }
        }
    }
    Ok(block)
}

fn require_conn_str(connection: &Value, field: &str) -> Result<String> {
    connection.get(field).and_then(|v| v.as_str()).map(String::from).ok_or_else(|| anyhow!("linked storage_connection missing required '{field}' field"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Each backend family assembles a distinct connection block from the DAG edges
    // (`__ref__bucket.__ref__connection`). `present` asserts (pointer, value) pairs;
    // `absent` asserts pointers that must NOT exist for that family (e.g. ADLS carries no
    // region/endpoint/access_key; gen1 carries no container).
    #[test]
    fn assemble_create_body_per_family() {
        type Case<'a> = (&'a str, Value, &'a [(&'a str, &'a str)], &'a [&'a str]);
        let cases: &[Case] = &[
            (
                "s3 family (ibm_cos): endpoint + HMAC creds + passthrough display_name/catalog",
                json!({"bucket": "iceberg_bucket", "display_name": "wxctl_test", "associated_catalog": {"catalog_name": "wxctl_iceberg", "catalog_type": "iceberg"}, "__ref__bucket": {"name": "my-bucket", "region": "eu-gb", "endpoint": "https://s3.eu-gb.cloud-object-storage.appdomain.cloud", "__ref__connection": {"type": "ibm_cos", "access_key": "AK", "secret_key": "SK"}}}),
                &[
                    ("/type", "ibm_cos"),
                    ("/region", "eu-gb"),
                    ("/connection/name", "my-bucket"),
                    ("/connection/endpoint", "https://s3.eu-gb.cloud-object-storage.appdomain.cloud"),
                    ("/connection/access_key", "AK"),
                    ("/connection/secret_key", "SK"),
                    ("/display_name", "wxctl_test"),
                    ("/associated_catalog/catalog_name", "wxctl_iceberg"),
                ],
                &[],
            ),
            (
                "adls_gen2: account + container (+ optional principal); no region/endpoint/access_key",
                json!({"bucket": "lake_fs", "display_name": "wxctl_adls", "associated_catalog": {"catalog_name": "wxctl_adls", "catalog_type": "iceberg"}, "__ref__bucket": {"filesystem": "myfs", "__ref__connection": {"type": "adls_gen2", "account_name": "mystorageacct", "container_name": "mycontainer", "sas_token": "sv=2021", "application_id": "app-123", "directory_id": "dir-456"}}}),
                &[("/type", "adls_gen2"), ("/connection/account_name", "mystorageacct"), ("/connection/container_name", "mycontainer"), ("/connection/sas_token", "sv=2021"), ("/connection/application_id", "app-123"), ("/connection/directory_id", "dir-456")],
                &["/region", "/connection/endpoint", "/connection/access_key"],
            ),
            (
                "adls_gen1: account only; no container",
                json!({"bucket": "lake_store", "display_name": "wxctl_adls1", "associated_catalog": {"catalog_name": "wxctl_adls1", "catalog_type": "iceberg"}, "__ref__bucket": {"data_lake_store_name": "mystore", "__ref__connection": {"type": "adls_gen1", "account_name": "gen1acct"}}}),
                &[("/type", "adls_gen1"), ("/connection/account_name", "gen1acct")],
                &["/connection/container_name"],
            ),
            (
                "google_cs: user-facing service_account_json emitted as wire field key_file",
                json!({"bucket": "gcs_b", "display_name": "wxctl_gcs", "associated_catalog": {"catalog_name": "wxctl_gcs", "catalog_type": "iceberg"}, "__ref__bucket": {"name": "my-gcs-bucket", "location": "us", "__ref__connection": {"type": "google_cs", "service_account_json": "{\"type\":\"service_account\"}"}}}),
                &[("/type", "google_cs"), ("/connection/key_file", "{\"type\":\"service_account\"}")],
                &[],
            ),
        ];
        for (msg, resource, present, absent) in cases {
            let body = assemble_create_body(resource).unwrap_or_else(|e| panic!("{msg}: {e}"));
            for (ptr, expected) in *present {
                assert_eq!(body.pointer(ptr).and_then(|v| v.as_str()), Some(*expected), "{msg}: {ptr}");
            }
            for ptr in *absent {
                assert!(body.pointer(ptr).is_none(), "{msg}: {ptr} must be absent");
            }
        }
    }

    // Missing DAG enrichment errors before any wire body is built.
    #[test]
    fn assemble_create_body_errors_on_missing_enrichment() {
        assert!(assemble_create_body(&json!({"bucket": "iceberg_bucket"})).is_err(), "missing __ref__bucket");
        assert!(assemble_create_body(&json!({"__ref__bucket": {"name": "b", "region": "r"}})).is_err(), "missing __ref__connection");
    }
}
