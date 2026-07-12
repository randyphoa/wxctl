//! Shared helpers for cascading catalog deletion from the registration
//! handlers. `/v3/catalogs/{name}` exposes GET + DELETE only (no POST,
//! no PATCH), so the catalog is atomically created by the registration's
//! POST and must be explicitly deleted on teardown for iceberg-style
//! catalogs. See `docs/superpowers/specs/2026-04-22-destroy-cascade-and-skip-design.md`.

use anyhow::{Context, Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use wxctl_core::client::{HttpClient, RequestSpec, error_has_status, error_matches};

/// Issue `DELETE /v3/catalogs/{catalog_name}` as a cascade after a
/// registration DELETE succeeds. "Already absent" is tolerated — the
/// watsonx.data server cascades catalog deletion from the registration
/// DELETE, so this follow-up almost always hits an already-gone catalog
/// and is defense-in-depth for the rare case the server cascade doesn't
/// fire. Any non-absent error propagates so the destroy summary surfaces
/// the orphan.
pub(super) async fn cascade_delete_catalog(client: &HttpClient, operation_id: &str, catalog_name: &str) -> Result<()> {
    let path = format!("/v3/catalogs/{}", catalog_name);
    // Mark 400 and 404 as expected so the HTTP client does not emit a WXCTL-H001
    // error event when the server cascade has already removed the catalog.
    // The returned Err is unchanged — is_catalog_already_absent still inspects it
    // to distinguish "already gone" (tolerate) from a genuine failure (propagate).
    let spec = RequestSpec::new(Method::DELETE, &path).expect_status(400).not_found_ok();
    match client.execute::<Value>(operation_id, spec).await {
        Ok(_) => Ok(()),
        Err(e) if is_catalog_already_absent(&e) => {
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                catalog_name = %catalog_name,
                "cascade: catalog already absent (server-cascaded or manual cleanup)"
            );
            Ok(())
        }
        Err(e) => Err(e).with_context(|| format!("cascade delete failed for catalog '{}'; registration was already deleted, catalog is now orphaned and requires manual cleanup", catalog_name)),
    }
}

/// The v3 catalog DELETE endpoint returns 404 for some tenants and a 400 with
/// body `"catalog does not exist <name>"` for others. Both mean "catalog is
/// gone" — the registration DELETE cascades server-side in most cases, so
/// this is the common path, not an edge.
fn is_catalog_already_absent(err: &anyhow::Error) -> bool {
    error_has_status(err, 404) || error_matches(err, 400, &["catalog does not exist"])
}

/// Extract `associated_catalog.catalog_name` from a registration's local
/// resource data. Always present in a valid registration config — schemas
/// require it as a nested required string.
pub(super) fn catalog_name_from_local(resource: &Value) -> Result<String> {
    resource.get("associated_catalog").and_then(|ac| ac.get("catalog_name")).and_then(|v| v.as_str()).map(String::from).ok_or_else(|| anyhow!("cascade: missing associated_catalog.catalog_name on registration"))
}

/// `post_delete` body shared by `storage_registration` and
/// `database_registration`: resolve the registration's catalog name and
/// issue the 404-tolerant cascade DELETE.
pub(super) async fn cascade_from_registration(resource: &Value, client: &HttpClient, operation_id: &str) -> Result<()> {
    let catalog_name = catalog_name_from_local(resource)?;
    cascade_delete_catalog(client, operation_id, &catalog_name).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::Method;
    use serde_json::json;
    use wxctl_core::client::RequestSpec;

    /// Regression test: the cascade DELETE RequestSpec must declare 400 and 404
    /// as expected so the HTTP client does not emit a WXCTL-H001 error event when
    /// the server cascade has already removed the catalog.
    #[test]
    fn cascade_spec_suppresses_400_and_404() {
        let path = "/v3/catalogs/wxctl_noobaa_iceberg";
        let spec = RequestSpec::new(Method::DELETE, path).expect_status(400).not_found_ok();
        assert!(spec.expected_statuses.contains(&400), "400 must be in expected_statuses to suppress spurious WXCTL-H001");
        assert!(spec.expected_statuses.contains(&404), "404 must be in expected_statuses for already-absent probe");
    }

    // "Already gone" = any 404, OR a 400 whose body says "catalog does not exist" (the
    // common server-cascade path); every other 400 and any 5xx must propagate. The error
    // strings match with_retry's format: "{code} HTTP {status} {method}: {api_message}".
    #[test]
    fn is_catalog_already_absent_cases() {
        let cases: &[(&str, bool)] = &[
            // 400 "catalog does not exist <name>" — trailing name varies, both tolerated.
            ("WXCTL-H001 HTTP 400 DELETE: catalog does not exist wxctl_noobaa_iceberg", true),
            ("WXCTL-H001 HTTP 400 DELETE: catalog does not exist db2_catalog_v2", true),
            // 404 — already absent.
            ("WXCTL-H001 HTTP 404 DELETE: not found", true),
            // Other 400s and any 5xx propagate.
            ("WXCTL-H001 HTTP 400 DELETE: catalog still has dependents", false),
            ("WXCTL-H001 HTTP 500 DELETE: internal server error", false),
        ];
        for (msg, expected) in cases {
            assert_eq!(is_catalog_already_absent(&anyhow!("{msg}")), *expected, "{msg}");
        }
    }

    // catalog_name_from_local extracts the nested required string, erroring when the
    // block is missing or the field is the wrong type.
    #[test]
    fn catalog_name_from_local_cases() {
        let ok = json!({"associated_catalog": {"catalog_name": "wxctl_iceberg", "catalog_type": "iceberg"}});
        assert_eq!(catalog_name_from_local(&ok).unwrap(), "wxctl_iceberg");

        assert!(catalog_name_from_local(&json!({"display_name": "whatever"})).is_err(), "block absent → err");
        assert!(catalog_name_from_local(&json!({"associated_catalog": {"catalog_name": 42}})).is_err(), "wrong type → err");
    }
}
