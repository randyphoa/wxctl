//! `resilience_library` handler — Concert's resilience library API returns the created id
//! under a DIFFERENT key than it reads back: `POST /resilience/assessment/api/v1/library`
//! responds `{ library_id }`, while GET/list return the canonical `id` (used by
//! GET/PUT/DELETE /library/{id}). The schema's `id_field` is `id`, so the engine's
//! post-create id extraction would find nothing in the `{ library_id }` create response.
//! `ResilienceLibraryHandler::post_create` maps `library_id → id` on the response so the
//! engine records the canonical id (the same lever CategoryHandler uses to hoist artifact_id).
//! `recover_from_create_error` adopts an already-existing library (e.g. a duplicate-name
//! conflict discovery missed) by listing `/library` and matching on `name`, returning the
//! existing object (which already carries `id`) — mirroring concert_source_repo's recovery.
//! Update is schema-driven (PUT replace pruned to state_fields); no update hook.
//! The id-map + list-and-match logic is shared with the other resilience kinds in
//! `super::common`.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

use super::common::{map_create_id, recover_by_name_from_list};

const LIBRARY_PATH: &str = "/resilience/assessment/api/v1/library";

pub struct ResilienceLibraryHandler;

impl ResourceHandler for ResilienceLibraryHandler {
    fn post_create<'a>(&'a self, _resource: &'a Value, response: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            map_create_id(response, "library_id");
            if let Some(id) = response.get("id").and_then(|v| v.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, id = %id, "resilience library created; library_id mapped to id");
            }
            Ok(())
        })
    }

    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, _error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(recover_by_name_from_list(client, operation_id, LIBRARY_PATH, resource.get("name").and_then(|v| v.as_str()), "concert_resilience_library"))
    }
}
