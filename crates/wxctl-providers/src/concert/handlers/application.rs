//! `concert_application` handler — hoists the nested release id to the top level.
//!
//! `concert_compliance_profile.application_releases_ids` must carry the application's
//! RELEASE id, not the application id itself (live-verified twice on Concert 3.0.0.0:
//! sending the application's own `id` 500s; the release id succeeds). The schema declares
//! `application_release_id` as a Computed field on `concert_application` so
//! `${concert_application.<ref>.application_release_id}` can be referenced, but Concert's
//! create/GET responses never carry that key flat — the release id only exists nested at
//! `associations.releases[0].id`. The engine's generic reference resolver reads flat
//! top-level keys only, so without this handler the template reference fails at apply with
//! "Field 'application_release_id' not found in template reference". `ApplicationHandler`
//! hoists the nested id to top level on BOTH the create response (`post_create`) and the
//! discovered state (`post_discover`), mirroring `CategoryHandler`'s `hoist_artifact_id`
//! pattern, so the reference resolves on first-apply and on re-apply/replan against an
//! already-discovered application alike.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

pub struct ApplicationHandler;

/// Hoist `associations.releases[0].id` to a top-level `application_release_id`.
///
/// No-op when a non-empty top-level `application_release_id` is already present (don't
/// clobber). No-op when `associations.releases` is absent, not an array, empty, or its
/// first element carries no string `id` — nothing to hoist.
fn hoist_application_release_id(value: &mut Value) {
    if value.get("application_release_id").and_then(|v| v.as_str()).is_some_and(|s| !s.is_empty()) {
        return;
    }
    let release_id = value.pointer("/associations/releases/0/id").and_then(|v| v.as_str()).map(str::to_string);
    if let (Some(obj), Some(release_id)) = (value.as_object_mut(), release_id) {
        obj.insert("application_release_id".to_string(), Value::String(release_id));
    }
}

impl ResourceHandler for ApplicationHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            hoist_application_release_id(response);
            if let Some(id) = response.get("application_release_id").and_then(|v| v.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, application_release_id = %id, "Application created; release id hoisted to top level");
            }
            Ok(())
        })
    }

    /// Mirror the create-time hoist on the discovery path (list_and_get GET). A
    /// pre-existing application discovered on re-apply/replan carries the release id only
    /// at `associations.releases[0].id` too — without this, a compliance profile
    /// referencing an already-applied application would fail to resolve
    /// `${concert_application.<ref>.application_release_id}` on replan, even though the
    /// create-time hoist resolved it on first apply.
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            hoist_application_release_id(remote_data);
            if let Some(id) = remote_data.get("application_release_id").and_then(|v| v.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, application_release_id = %id, "Application discovered; release id hoisted to top level");
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // hoist_application_release_id lifts the id nested at associations.releases[0].id to
    // the top level so `${concert_application.<ref>.application_release_id}` resolves. A
    // pre-existing non-empty top-level id wins (no clobber); a missing/empty releases array
    // is a no-op (no-fabricate). Expected `None` = key absent.
    #[test]
    fn hoist_application_release_id_cases() {
        let cases: &[(&str, Value, Option<&str>)] = &[
            ("hoists from nested associations.releases[0].id", json!({"id": "app-1", "associations": {"releases": [{"id": "rel-9"}]}}), Some("rel-9")),
            ("existing top-level id wins over nested", json!({"application_release_id": "top-1", "associations": {"releases": [{"id": "rel-9"}]}}), Some("top-1")),
            ("missing associations is a no-op", json!({"id": "app-1"}), None),
            ("empty releases array is a no-op", json!({"id": "app-1", "associations": {"releases": []}}), None),
        ];
        for (msg, mut resp, expected) in cases.iter().map(|(m, r, e)| (*m, r.clone(), *e)) {
            hoist_application_release_id(&mut resp);
            assert_eq!(resp.get("application_release_id").and_then(|v| v.as_str()), expected, "{msg}");
        }
    }
}
