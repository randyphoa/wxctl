//! `resilience_profile` handler — Concert's resilience profile API returns the created id
//! under a DIFFERENT key than it reads back: `POST /resilience/assessment/api/v1/profile`
//! responds `{ profile_id }` (201), while GET/list return the canonical `id` (used by
//! GET/DELETE /profile/{id}). The schema's `id_field` is `id`, so the engine's post-create
//! id extraction would find nothing in the `{ profile_id }` create response.
//! `ResilienceProfileHandler::post_create` maps `profile_id -> id` on the response so the
//! engine records the canonical id (the same lever `ResilienceLibraryHandler` uses).
//! `recover_from_create_error` adopts an already-existing profile (e.g. a duplicate-name
//! conflict discovery missed) by listing `/profile` and matching on `name`, returning the
//! existing object (which already carries `id`).
//!
//! The profile API has NO update verb (`/profile/{id}` exposes GET + DELETE only), so all
//! round-tripping writable scalars are `immutable_fields` and drift reconciles by Recreate
//! (destroy + recreate) — there is no update hook or endpoint (spec Q4).
//! The id-map + list-and-match logic is shared with the other resilience kinds in
//! `super::common`.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

use super::common::{map_create_id, recover_by_name_from_list};

const PROFILE_PATH: &str = "/resilience/assessment/api/v1/profile";

pub struct ResilienceProfileHandler;

impl ResourceHandler for ResilienceProfileHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            map_create_id(response, "profile_id");
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, id = %id, "resilience profile created; profile_id mapped to id");
            }
            Ok(())
        })
    }

    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, _error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(recover_by_name_from_list(client, operation_id, PROFILE_PATH, resource.get("name").and_then(|v| v.as_str()), "concert_resilience_profile"))
    }
}
