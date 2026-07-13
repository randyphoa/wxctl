//! `resilience_posture` handler — Concert's resilience posture API returns the created id
//! under a DIFFERENT key than it reads back: `POST /resilience/assessment/api/v1/posture`
//! responds `{ posture_id }` (201), while GET/list return the canonical `id` (used by
//! GET/PATCH/DELETE /posture/{id}). The schema's `id_field` is `id`, so the engine's
//! post-create id extraction would find nothing in the `{ posture_id }` create response.
//! `ResiliencePostureHandler::post_create` maps `posture_id -> id` on the response so the
//! engine records the canonical id (the same lever `ResilienceLibraryHandler` uses).
//! `recover_from_create_error` adopts an already-existing posture (e.g. a duplicate-name
//! conflict discovery missed) by listing `/posture` and matching on `name`, returning the
//! existing object (which already carries `id`).
//!
//! The posture binds its profile BY NAME (`profile_name` -> concert_resilience_profile.name,
//! spec Q3) and exposes only a narrow PATCH (`UpdatePostureRequest` = assessment_period +
//! comments); every other writable field is `immutable_fields` (drift -> Recreate). Update is
//! schema-driven (PATCH merge pruned to state_fields); no update hook.
//! The id-map + list-and-match logic is shared with the other resilience kinds in
//! `super::common`.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

use super::common::{map_create_id, recover_by_name_from_list};

const POSTURE_PATH: &str = "/resilience/assessment/api/v1/posture";

pub struct ResiliencePostureHandler;

impl ResourceHandler for ResiliencePostureHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            map_create_id(response, "posture_id");
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, id = %id, "resilience posture created; posture_id mapped to id");
            }
            Ok(())
        })
    }

    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, _error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(recover_by_name_from_list(client, operation_id, POSTURE_PATH, resource.get("name").and_then(|v| v.as_str()), "concert_resilience_posture"))
    }
}
