//! Shared HashiCorp Vault response-envelope helper.
//!
//! Every Vault read wraps its payload in a top-level `data` object
//! (`{"request_id":..., "data": {...}, ...}`). `unwrap_data_envelope` replaces the
//! discovered value with the contents of that `data` object, dropping Vault's
//! metadata wrapper so declared fields compare correctly during reconciliation.
//! Lifting `data.*` to the top level also surfaces any computed id (`data.id`,
//! `data.canonical_id`) for `${vault_*.<ref>.<field>}` references in later phases.
//!
//! Writes often return `204 No Content`. The engine's create path already tolerates
//! an empty body (`Value::Null`) and preserves the client-supplied name as the id via
//! `merge_request_response`, so no envelope handling is needed there — this helper is
//! discovery-only.

use serde_json::Value;

/// If `remote_data` is an object carrying an object-valued `data` field, replace
/// `remote_data` with the contents of that `data` object. No-op when there is no
/// object `data` field (already unwrapped, or an unexpected shape).
pub(crate) fn unwrap_data_envelope(remote_data: &mut Value) {
    if let Some(inner) = remote_data.get("data").filter(|v| v.is_object()).cloned() {
        *remote_data = inner;
    }
}

use anyhow::Result;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

/// Generic vault handler for kinds whose only custom behavior is unwrapping
/// Vault's top-level `data` response envelope on discovery (no sub-writes, no
/// computed-id hoist). Registered directly by kinds like `vault_jwt_role` and
/// `vault_group_alias`.
pub struct EnvelopeHandler;

impl ResourceHandler for EnvelopeHandler {
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            unwrap_data_envelope(remote_data);
            Ok(())
        })
    }
}
