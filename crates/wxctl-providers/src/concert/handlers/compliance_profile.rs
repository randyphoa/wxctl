//! `compliance_profile` handler — Concert's `POST /compliance/api/v1/profiles` create
//! returns `201 {message}` with NO id (the profile `uuid` is not echoed). So
//! `ComplianceProfileHandler.post_create` recovers the id: it lists
//! `GET /compliance/api/v1/profiles`, matches the just-created profile by `title`, and
//! injects its `uuid` into the create response so the engine's id extractor
//! (schema `id_field: uuid`) and `${concert_compliance_profile.<ref>.uuid}` resolve —
//! the same top-level-id normalization `CategoryHandler.post_create` does, sourced from a
//! list call rather than the response body. `recover_from_create_error` adopts an
//! already-existing profile (e.g. a duplicate-`title`/409 conflict reconciliation couldn't
//! detect ahead of time) via the identical list-and-match, returning it as the successful
//! create response.
//!
//! Discovery (schema `list_and_get`, `id_source: uuid`) and item GET both carry `uuid` at
//! the top level, so no discovery-path hook is needed. Delete is a real item DELETE
//! (`DELETE /compliance/api/v1/profiles/{id}`), and update is schema-driven full PUT-replace
//! (`update_method: PUT` + `update_strategy: replace`) — no delete/update hook here.
//!
//! Title uniqueness (spec Q5): matching assumes `title` is unique per instance; if a live
//! instance allows duplicate titles, Phase 5 can switch to sending a client-generated
//! `uuid` in the create body and matching on it.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::traits::ResourceHandler;

const PROFILES_PATH: &str = "/compliance/api/v1/profiles";

pub struct ComplianceProfileHandler;

impl ResourceHandler for ComplianceProfileHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // Defensive: if a uuid somehow rode the create response, keep it.
            if response.get("uuid").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
                return Ok(());
            }
            // Create returned {message} with no uuid → recover it by listing and matching title.
            let title = resource.get("title").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_compliance_profile requires a 'title' to recover its uuid after create"))?;
            let spec = RequestSpec::new(Method::GET, PROFILES_PATH).body(BodyKind::None);
            let listed: Value = client.execute(operation_id, spec).await?;
            let found = find_profile_by_title(&listed, title).ok_or_else(|| anyhow!("concert_compliance_profile '{title}' not found in GET {PROFILES_PATH} after create — cannot recover uuid"))?;
            let uuid = found.get("uuid").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("recovered concert_compliance_profile '{title}' has no 'uuid'"))?.to_string();
            if let Some(obj) = response.as_object_mut() {
                obj.insert("uuid".to_string(), Value::String(uuid));
            }
            Ok(())
        })
    }

    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, _error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            let Some(title) = resource.get("title").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            // Adopt an already-existing profile with the same title (idempotent create).
            // The compliance list response carries no `pagination` object (it uses
            // `total_items`), so `fetch_all=true` returns every profile in one page —
            // the Concert-native way to read past page 1 here.
            // A failed recovery list must NOT replace the original create error (the
            // engine `.await?`s this hook) — warn and return Ok(None) so the engine
            // falls back to the real error.
            let spec = RequestSpec::new(Method::GET, PROFILES_PATH).query_param("fetch_all", "true").body(BodyKind::None);
            match client.execute::<Value>(operation_id, spec).await {
                Ok(listed) => Ok(find_profile_by_title(&listed, title)),
                Err(e) => {
                    tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, kind = "concert_compliance_profile", error = %e, "recovery list GET failed — falling back to the original create error");
                    Ok(None)
                }
            }
        })
    }
}

/// Search a list response — a bare array or a `{profiles:[…]}` / `{data:[…]}` envelope —
/// for the profile whose `title` matches, returning a clone of that object. Tolerates both
/// shapes because the 3.0.0.0 OpenAPI mistypes the list response (single object); the real
/// runtime shape is confirmed in Phase 5.
fn find_profile_by_title(value: &Value, title: &str) -> Option<Value> {
    let items = match value {
        Value::Array(a) => a,
        Value::Object(o) => o.get("profiles").or_else(|| o.get("data")).and_then(|v| v.as_array())?,
        _ => return None,
    };
    items.iter().find(|item| item.get("title").and_then(|v| v.as_str()) == Some(title)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn find_profile_by_title_matches_in_bare_array() {
        let listed = json!([{"uuid": "u1", "title": "Other"}, {"uuid": "u2", "title": "SOC2"}]);
        let got = find_profile_by_title(&listed, "SOC2").expect("match in array");
        assert_eq!(got.get("uuid").and_then(|v| v.as_str()), Some("u2"));
    }

    #[test]
    fn find_profile_by_title_matches_in_profiles_envelope() {
        let listed = json!({"profiles": [{"uuid": "u3", "title": "ISO27001"}], "total_items": 1});
        let got = find_profile_by_title(&listed, "ISO27001").expect("match in envelope");
        assert_eq!(got.get("uuid").and_then(|v| v.as_str()), Some("u3"));
    }

    #[test]
    fn find_profile_by_title_no_match_returns_none() {
        let listed = json!([{"uuid": "u1", "title": "Other"}]);
        assert!(find_profile_by_title(&listed, "SOC2").is_none());
    }
}
