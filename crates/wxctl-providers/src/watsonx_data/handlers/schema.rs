//! `schema` handler — only task is idempotent recovery. The `catalog_id`
//! / `engine_id` template refs defer reconciliation's list probe, so
//! re-apply trips the presto DDL "schema already exists" error; adopt the
//! desired spec instead of failing.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{HttpClient, error_matches};
use wxctl_core::logging::error_codes;
use wxctl_core::traits::ResourceHandler;

pub struct SchemaHandler;

impl ResourceHandler for SchemaHandler {
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            if !is_schema_already_exists(error) {
                return Ok(None);
            }
            let Some(name) = resource.get("name").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            // Schemas are immutable — adopting the desired spec as-is is safe;
            // drift on a subsequent reconcile surfaces via the normal Recreate path.
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "schema",
                schema_name = %name,
                error_code = error_codes::H710,
                "recovered from already-exists conflict by adopting desired spec"
            );
            Ok(Some(resource.clone()))
        })
    }
}

fn is_schema_already_exists(err: &anyhow::Error) -> bool {
    error_matches(err, 400, &["Schema", "already exists"])
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    // Only a 400 carrying both "Schema" and "already exists" (the presto DDL
    // SemanticException) recovers; a 500 or an unrelated 400 must not.
    #[test]
    fn is_schema_already_exists_cases() {
        let cases: &[(&str, bool)] = &[
            ("WXCTL-H001 HTTP 400 POST: Executing query failed with error: presto: query failed (200 OK): \"com.facebook.presto.sql.analyzer.SemanticException: line 1:1: Schema 'wxctl_iceberg.sample' already exists\"", true),
            ("WXCTL-H002 HTTP 500 POST: internal error", false),
            ("WXCTL-H001 HTTP 400 POST: invalid custom_path", false),
        ];
        for (msg, expected) in cases {
            assert_eq!(is_schema_already_exists(&anyhow!("{msg}")), *expected, "{msg}");
        }
    }
}
