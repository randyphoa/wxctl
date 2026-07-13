//! Shared `recover_from_create_error` helper for storage_registration and
//! database_registration. When POST returns `HTTP 400 "already exists"`,
//! list + match by `associated_catalog.catalog_name` (the schema-declared
//! identity) and return the existing remote. Restores the adopt-on-conflict
//! behavior removed in commit e460b16 — kept here instead of per-handler so
//! the two impls don't drift.

use anyhow::Result;
use serde_json::Value;
use wxctl_core::client::{HttpClient, error_matches};

use super::registration_normalize::backfill_associated_catalog;

/// Recognize the "this name/bucket already exists" shape the v3 registration
/// APIs return for both the display-name collision (db) and the bucket
/// collision (storage) cases.
fn is_already_exists(err: &anyhow::Error) -> bool {
    error_matches(err, 400, &["already exists"])
}

/// Adopt an existing registration on 400-already-exists: list, filter by
/// `associated_catalog.catalog_name`, denormalize. Returns `None` if the
/// error isn't already-exists OR the identity doesn't match any listed entry
/// (the latter surfaces as the original error through the engine).
pub(super) async fn adopt_registration_on_conflict(resource: &Value, error: &anyhow::Error, client: &HttpClient, operation_id: &str, list_endpoint: &str) -> Result<Option<Value>> {
    if !is_already_exists(error) {
        return Ok(None);
    }
    let Some(local_catalog) = resource.get("associated_catalog").and_then(|ac| ac.get("catalog_name")).and_then(|v| v.as_str()) else {
        return Ok(None);
    };
    let items: Vec<Value> = client.list_with_params(operation_id, list_endpoint, None).await?;
    // List items may carry the plural `associated_catalogs[0].catalog_name`
    // (storage) or the singular `associated_catalog.catalog_name` (database).
    let matched = items.into_iter().find(|item| {
        let plural = item.pointer("/associated_catalogs/0/catalog_name").and_then(|v| v.as_str());
        let singular = item.pointer("/associated_catalog/catalog_name").and_then(|v| v.as_str());
        plural == Some(local_catalog) || singular == Some(local_catalog)
    });
    if let Some(mut adopted) = matched {
        backfill_associated_catalog(&mut adopted);
        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            list_endpoint = %list_endpoint,
            catalog_name = %local_catalog,
            "adopt: existing registration matched by identity, returning as Create result"
        );
        return Ok(Some(adopted));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;

    // Both the storage-bucket and database-display-name conflicts surface as a 400
    // carrying "already exists" (wording differs). An unrelated 400, or a 500 with the
    // phrase, must not match.
    #[test]
    fn is_already_exists_cases() {
        let cases: &[(&str, bool)] = &[("WXCTL-H001 HTTP 400 POST: bucket already exists", true), ("WXCTL-H001 HTTP 400 POST: database registration with this name already exists", true), ("WXCTL-H001 HTTP 400 POST: invalid payload", false), ("WXCTL-H002 HTTP 500 POST: already exists", false)];
        for (msg, expected) in cases {
            assert_eq!(is_already_exists(&anyhow!("{msg}")), *expected, "{msg}");
        }
    }
}
