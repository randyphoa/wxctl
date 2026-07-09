//! `source_repo` handler — Concert's `POST /core/api/v1/source_repos` is a bulk
//! endpoint: the request body is `WorkspaceRepositoryPrototype
//! { source_repos: [WorkspaceRepositoryPrototypeDetail] }` (an array wrapper),
//! but the wxctl schema models a single repo. The `source_repos` wrapper key is
//! NOT a declared schema field, so a `pre_create` that merely inserts it and
//! returns `Continue` would have it dropped by `RequestMaterializer` (declared
//! fields only — see docs/troubleshoot/pre-create-body-reshape-dropped-fix.md).
//! So this handler owns the POST: it wraps the single declared repo into a
//! one-element array, POSTs, extracts the created repo (top-level `id`, matched
//! by `repo_url`), and returns `HookOutcome::Handled(created)` — which makes the
//! engine skip the default POST and `post_create`. `recover_from_create_error`
//! adopts an already-existing repo (e.g. 409) by listing and matching `repo_url`.
//!
//! `pre_delete` works around a Concert 3.0.x teardown quirk (live-confirmed):
//! creating a `concert_source_repo` auto-creates a
//! `placeholder_app_<base64-user>` application and correlates the repo to it. The
//! item DELETE — even with `is_cascade_delete=true` — then 403s ("the repository
//! is correlated to an application") until that app is gone. So this handler owns
//! the DELETE: it tries the cascade delete, and on a 403 lists the repo's
//! correlated applications and deletes only the `placeholder_app_`-prefixed ones
//! (never a differently-named, presumably user-managed app).
//!
//! Teardown is EVENTUALLY CONSISTENT and the delete status code is NOT a reliable
//! success signal, so the handler then POLLS for the repo's absence rather than
//! trusting the delete response. Live-observed sequence: after the placeholder
//! app is deleted (204), the correlation takes ~3-4s to clear, and clearing it
//! also removes the repo. Once the repo is gone, `DELETE` returns a *misleading*
//! `400 "application database could not be established"` (not 204/404), and `GET`
//! returns `200` with a BLANK record (empty `id`) — not a 404. So absence is
//! judged by GET returning an empty `id`/`repo_url` (or a clean 404), which is the
//! authoritative signal the poll loop waits on; each round also re-attempts the
//! cascade to cover instances where clearing the correlation does not auto-remove
//! the repo. Requires the discovered `id`, which the engine's delete path now
//! injects from the discovered remote before calling `pre_delete`.

use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Map, Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::error_has_status;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

const SOURCE_REPOS_PATH: &str = "/core/api/v1/source_repos";
const APPLICATIONS_PATH: &str = "/core/api/v1/applications";

/// Poll budget for repo teardown after the app correlation is broken. Observed
/// live timing on Concert 3.0.x: the correlation clears (and the repo disappears)
/// ~3-4s after the placeholder app is deleted, so ~24s of headroom absorbs the
/// eventual consistency without hanging destroy on a genuinely stuck repo.
const REPO_DELETE_MAX_POLLS: u32 = 12;
const REPO_DELETE_POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Prefix Concert stamps on the app it auto-creates + correlates to a newly
/// created source repo (`placeholder_app_<base64-user>`). Only apps matching
/// this prefix are safe to delete on the repo's behalf — a real, user-managed
/// app can also be correlated and must never be touched here.
const PLACEHOLDER_APP_PREFIX: &str = "placeholder_app_";

/// Writable fields copied from the declared resource into the
/// `WorkspaceRepositoryPrototypeDetail` wire body.
const DETAIL_FIELDS: &[&str] = &["name", "repo_url", "branch", "commit_sha", "tags", "associations", "properties"];

pub struct SourceRepoHandler;

impl ResourceHandler for SourceRepoHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let repo_url = resource.get("repo_url").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_source_repo requires a 'repo_url' field"))?.to_string();
            let detail = build_detail(resource);
            let mut wrapper = Map::new();
            wrapper.insert("source_repos".to_string(), Value::Array(vec![Value::Object(detail)]));
            let spec = RequestSpec::new(Method::POST, SOURCE_REPOS_PATH).body(BodyKind::Json(Value::Object(wrapper)));
            let response: Value = client.execute(operation_id, spec).await?;
            let created = extract_created_repo(response, &repo_url)?;
            Ok(HookOutcome::Handled(created))
        })
    }

    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, _error: &'a anyhow::Error, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            // A failed recovery must NOT replace the original create error (the engine
            // `.await?`s this hook) — return Ok(None) so the engine falls back to it.
            let Some(repo_url) = resource.get("repo_url").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            // Read every page — an already-existing repo may be beyond page 1.
            match crate::util::fetch_all_pages(client, operation_id, SOURCE_REPOS_PATH, "source_repos").await {
                Ok(items) => Ok(find_repo_by_url(&json!({ "source_repos": items }), repo_url)),
                Err(e) => {
                    tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, kind = "concert_source_repo", error = %e, "recovery list GET failed — falling back to the original create error");
                    Ok(None)
                }
            }
        })
    }

    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let id = resource.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("concert_source_repo delete requires a resolved 'id'"))?.to_string();

            // First attempt: a repo with no correlated app deletes cleanly here.
            match cascade_delete_repo(client, operation_id, &id).await {
                Ok(v) => return Ok(HookOutcome::Handled(v)),
                Err(e) if error_has_status(&e, 404) => return Ok(HookOutcome::Handled(json!({"deleted": id}))),
                Err(e) if error_has_status(&e, 403) => {
                    // Correlated to an application (typically Concert's own auto-created
                    // placeholder_app_*). Break the correlation, then poll below.
                    delete_placeholder_apps(client, operation_id, &id).await;
                }
                // Any other status (e.g. the misleading 400 Concert returns for an
                // already-absent repo) — confirm via GET before trusting or failing.
                Err(e) => {
                    if repo_absent(client, operation_id, &id).await {
                        return Ok(HookOutcome::Handled(json!({"deleted": id})));
                    }
                    return Err(e);
                }
            }

            // Correlation broken: teardown is async (~3-4s) and the DELETE status is
            // unreliable, so poll for absence (GET → blank record) as the authoritative
            // signal, re-attempting the cascade each round for instances where clearing
            // the correlation does not itself remove the repo.
            for attempt in 1..=REPO_DELETE_MAX_POLLS {
                if repo_absent(client, operation_id, &id).await {
                    return Ok(HookOutcome::Handled(json!({"deleted": id})));
                }
                match cascade_delete_repo(client, operation_id, &id).await {
                    Ok(v) => return Ok(HookOutcome::Handled(v)),
                    Err(e) if error_has_status(&e, 404) => return Ok(HookOutcome::Handled(json!({"deleted": id}))),
                    // Still correlated (re-spawned placeholder) — break it again.
                    Err(e) if error_has_status(&e, 403) => delete_placeholder_apps(client, operation_id, &id).await,
                    // Misleading 400 / transient — the next-round GET decides absence.
                    Err(_) => {}
                }
                if attempt < REPO_DELETE_MAX_POLLS {
                    tokio::time::sleep(REPO_DELETE_POLL_INTERVAL).await;
                }
            }

            // Exhausted the poll budget: one final authoritative check.
            if repo_absent(client, operation_id, &id).await { Ok(HookOutcome::Handled(json!({"deleted": id}))) } else { Err(anyhow!("concert_source_repo '{id}' still present after breaking its application correlation and {REPO_DELETE_MAX_POLLS} delete polls")) }
        })
    }
}

/// `DELETE /core/api/v1/source_repos/{id}?is_cascade_delete=true`. The `expected_statuses`
/// suppress spurious execution-error events for the statuses this teardown legitimately
/// passes through — `404` (already absent), `403` (still correlated to an app; handled by
/// breaking the correlation and polling), and the misleading `400` Concert returns for an
/// already-deleted repo (absence is then confirmed by `repo_absent`). The call still returns
/// `Err` for each so the caller's control flow decides; a genuinely stuck repo surfaces via
/// the handler's own final `Err`, which is NOT suppressed.
async fn cascade_delete_repo(client: &HttpClient, operation_id: &str, id: &str) -> Result<Value> {
    let path = format!("{SOURCE_REPOS_PATH}/{id}");
    let spec = RequestSpec::new(Method::DELETE, &path).query_param("is_cascade_delete", "true").body(BodyKind::None).not_found_ok().expect_status(403).expect_status(400);
    client.execute(operation_id, spec).await
}

/// True when the repo `id` no longer exists. Concert does NOT 404 a gone repo on
/// `GET /source_repos/{id}` — it returns `200` with a BLANK record (empty `id`) —
/// so absence is judged by that empty body (or a genuine 404). A network/other
/// error can't confirm absence, so it reads as "still present" (keep polling).
async fn repo_absent(client: &HttpClient, operation_id: &str, id: &str) -> bool {
    let path = format!("{SOURCE_REPOS_PATH}/{id}");
    let spec = RequestSpec::new(Method::GET, &path).body(BodyKind::None).not_found_ok();
    match client.execute::<Value>(operation_id, spec).await {
        Ok(body) => repo_is_absent_response(&body),
        Err(e) if error_has_status(&e, 404) => true,
        Err(_) => false,
    }
}

/// Pure predicate for `repo_absent`: a `GET /source_repos/{id}` body denotes an
/// absent repo when its `id` (and `repo_url`) are missing or empty — Concert's
/// blank-record response for a deleted repo.
fn repo_is_absent_response(body: &Value) -> bool {
    let blank = |field: &str| body.get(field).and_then(|v| v.as_str()).is_none_or(str::is_empty);
    blank("id") && blank("repo_url")
}

/// List the applications correlated to this repo and best-effort delete every
/// one whose name matches the `placeholder_app_` prefix Concert auto-creates.
/// Never touches a correlated application that doesn't match — that may be a
/// real, user-managed app. Individual delete failures are swallowed: this is
/// a best-effort unblock before retrying the cascade delete, which reports
/// the authoritative failure if the correlation couldn't be broken.
async fn delete_placeholder_apps(client: &HttpClient, operation_id: &str, repo_id: &str) {
    let path = format!("{SOURCE_REPOS_PATH}/{repo_id}/applications");
    let spec = RequestSpec::new(Method::GET, &path).body(BodyKind::None);
    let Ok(listed) = client.execute::<Value>(operation_id, spec).await else {
        return;
    };
    let Some(apps) = listed.get("applications").and_then(|v| v.as_array()) else {
        return;
    };
    for app in apps {
        let Some(app_id) = app.get("id").and_then(|v| v.as_str()) else {
            continue;
        };
        let Some(name) = app.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        if !is_placeholder_app(name) {
            continue;
        }
        let delete_path = format!("{APPLICATIONS_PATH}/{app_id}");
        let spec = RequestSpec::new(Method::DELETE, &delete_path).body(BodyKind::None).not_found_ok();
        let _ = client.execute::<Value>(operation_id, spec).await;
    }
}

/// True when `name` is one of Concert's own auto-created placeholder apps
/// (`placeholder_app_<base64-user>`) — the only applications this handler is
/// allowed to delete on a repo's behalf.
fn is_placeholder_app(name: &str) -> bool {
    name.starts_with(PLACEHOLDER_APP_PREFIX)
}

/// Copy the declared writable fields onto a `WorkspaceRepositoryPrototypeDetail` map.
fn build_detail(resource: &Value) -> Map<String, Value> {
    let mut detail = Map::new();
    for &field in DETAIL_FIELDS {
        if let Some(v) = resource.get(field) {
            detail.insert(field.to_string(), v.clone());
        }
    }
    detail
}

/// Extract the created repo object (carrying top-level `id`) from the create
/// response. Concert's bulk create returns a single `WorkspaceRepository`
/// object; tolerate an array or `{source_repos:[…]}` envelope by matching on
/// `repo_url`.
fn extract_created_repo(response: Value, repo_url: &str) -> Result<Value> {
    match &response {
        Value::Object(o) if o.contains_key("id") && !o.contains_key("source_repos") => Ok(response),
        _ => find_repo_by_url(&response, repo_url).ok_or_else(|| anyhow!("concert_source_repo create response had no entry matching repo_url '{repo_url}'")),
    }
}

/// Search a create/list response (array or `{source_repos:[…]}` envelope) for the
/// repo whose `repo_url` matches, returning a clone of that object.
fn find_repo_by_url(value: &Value, repo_url: &str) -> Option<Value> {
    let items = match value {
        Value::Array(a) => a,
        Value::Object(o) => o.get("source_repos").and_then(|v| v.as_array())?,
        _ => return None,
    };
    items.iter().find(|item| item.get("repo_url").and_then(|v| v.as_str()) == Some(repo_url)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn build_detail_copies_only_declared_writable_fields() {
        let resource = json!({"name": "r", "repo_url": "https://git/r", "branch": "main", "id": "should-not-copy", "created_on": 1});
        let detail = build_detail(&resource);
        assert_eq!(detail.get("name").and_then(|v| v.as_str()), Some("r"));
        assert_eq!(detail.get("repo_url").and_then(|v| v.as_str()), Some("https://git/r"));
        assert_eq!(detail.get("branch").and_then(|v| v.as_str()), Some("main"));
        assert!(!detail.contains_key("id"), "computed id must not ride the create body");
        assert!(!detail.contains_key("created_on"));
    }

    #[test]
    fn extract_created_repo_single_object_returns_as_is() {
        let resp = json!({"id": "abc", "repo_url": "https://git/r", "name": "r"});
        let got = extract_created_repo(resp, "https://git/r").expect("single object with id");
        assert_eq!(got.get("id").and_then(|v| v.as_str()), Some("abc"));
    }

    #[test]
    fn extract_created_repo_array_matches_repo_url() {
        let resp = json!([{"id": "x", "repo_url": "https://git/other"}, {"id": "y", "repo_url": "https://git/r"}]);
        let got = extract_created_repo(resp, "https://git/r").expect("array match");
        assert_eq!(got.get("id").and_then(|v| v.as_str()), Some("y"));
    }

    #[test]
    fn extract_created_repo_envelope_matches_repo_url() {
        let resp = json!({"pagination": {}, "source_repos": [{"id": "z", "repo_url": "https://git/r"}]});
        let got = extract_created_repo(resp, "https://git/r").expect("envelope match");
        assert_eq!(got.get("id").and_then(|v| v.as_str()), Some("z"));
    }

    #[test]
    fn extract_created_repo_no_match_errors() {
        let resp = json!([{"id": "x", "repo_url": "https://git/other"}]);
        assert!(extract_created_repo(resp, "https://git/r").is_err());
    }

    #[test]
    fn is_placeholder_app_matches_prefix() {
        assert!(is_placeholder_app("placeholder_app_Y3BhZG1pbg=="));
    }

    #[test]
    fn is_placeholder_app_rejects_user_managed_names() {
        assert!(!is_placeholder_app("checkout-service"));
    }

    #[test]
    fn repo_absent_response_blank_record_is_absent() {
        // Concert's live "deleted repo" GET body: 200 with an empty record.
        let body = json!({"id": "", "repo_url": "", "name": "", "created_on": 0});
        assert!(repo_is_absent_response(&body));
    }

    #[test]
    fn repo_absent_response_missing_fields_is_absent() {
        assert!(repo_is_absent_response(&json!({})));
    }

    #[test]
    fn repo_absent_response_populated_record_is_present() {
        let body = json!({"id": "202866c8", "repo_url": "https://github.com/example-org/checkout-service", "name": "checkout-service-repo"});
        assert!(!repo_is_absent_response(&body));
    }

    #[test]
    fn repo_absent_response_partial_id_only_is_present() {
        // A non-empty id (even with a blank repo_url) means the repo still exists.
        assert!(!repo_is_absent_response(&json!({"id": "202866c8", "repo_url": ""})));
    }
}
