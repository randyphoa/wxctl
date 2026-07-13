use anyhow::{Context, Result, anyhow, bail};
use futures::future::join_all;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zip::write::FileOptions;

/// Synthetic key prefix the engine uses to inject full resolved linked-
/// resource specs into a resource's data just before a handler runs.
/// Handlers that walk DAG edges at apply time read from
/// `resource["__ref__<field>"]`.
pub const REF_PREFIX: &str = "__ref__";

/// Canonical enriched-reference key for a `connection:` field — used by
/// resources that reference a `storage_connection` / `database_connection`.
pub const REF_CONNECTION: &str = "__ref__connection";

/// Canonical enriched-reference key for a `bucket:` field — used by
/// `s3_object` and `storage_registration`.
pub const REF_BUCKET: &str = "__ref__bucket";

/// Read an engine-injected enriched reference from a resource, erroring
/// with a descriptive message when absent. Absence is always a bug in
/// the engine's enrichment pass or a misconfigured schema — user YAML
/// is validated separately.
pub fn require_ref<'a>(resource: &'a Value, key: &str) -> Result<&'a Value> {
    resource.get(key).ok_or_else(|| anyhow!("'{key}' missing from resource data — engine enrichment did not resolve the upstream reference"))
}

/// Validate path to prevent traversal attacks.
/// The canonical path must stay inside a trusted root: the current working
/// directory, or any config base directory registered via
/// `wxctl_core::paths::allow_path_root` — relative path fields resolve against
/// their config file's directory (the documented contract), which may
/// legitimately be outside the CWD.
pub fn validate_path(source_path: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(source_path).context(format!("Failed to canonicalize source path: {}", source_path.display()))?;

    let cwd = std::env::current_dir().context("Failed to get current working directory")?;

    let canonical_cwd = std::fs::canonicalize(&cwd).context("Failed to canonicalize working directory")?;

    if canonical.starts_with(&canonical_cwd) || wxctl_core::paths::allowed_path_roots().iter().any(|root| canonical.starts_with(root)) {
        return Ok(canonical);
    }

    Err(anyhow!("Path traversal detected: {} is outside working directory {} and all loaded config directories", canonical.display(), canonical_cwd.display()))
}

/// Extract artifact_id from a response value, handling both SaaS and on-prem formats.
///
/// Tries in order:
/// 1. `metadata.artifact_id` (SaaS format)
/// 2. `resources[0].artifact_id` (on-prem format)
/// 3. `id` (fallback / already-transformed)
pub fn extract_artifact_id(value: &Value) -> Option<&str> {
    value.get("metadata").and_then(|m| m.get("artifact_id")).and_then(|id| id.as_str()).or_else(|| value.get("resources").and_then(|r| r.as_array()).and_then(|arr| arr.first()).and_then(|item| item.get("artifact_id")).and_then(|id| id.as_str())).or_else(|| value.get("id").and_then(|id| id.as_str()))
}

/// Extract a resource id from a create/get response, handling both SaaS (`metadata.id`) and on-prem (`id`) shapes.
/// Thin re-export of [`wxctl_core::resource_id`] — the single home for id extraction.
pub use wxctl_core::resource_id;

/// Execute futures in parallel and collect results, failing on the first error.
pub async fn join_all_ok<T, F>(futures: impl IntoIterator<Item = F>) -> Result<Vec<T>>
where
    F: Future<Output = Result<T>>,
{
    let results = join_all(futures).await;
    results.into_iter().collect()
}

pub fn extract_artifact_path(resource: &Value) -> Option<String> {
    resource.get("artifact").and_then(|a| a.get("path")).and_then(|p| p.as_str()).map(String::from)
}

pub fn extract_source_hash(resource: &Value) -> Option<&str> {
    resource.get("tags").and_then(|v| v.as_array()).and_then(|tags| tags.iter().find_map(|t| t.as_str()?.strip_prefix("source-hash:")))
}

pub fn set_source_hash_tag(resource: &mut Value, hash: &str) {
    let mut tags: Vec<Value> = resource.get("tags").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().filter(|t| !t.as_str().is_some_and(|s| s.starts_with("source-hash:"))).collect();

    tags.push(json!(format!("source-hash:{}", hash)));
    resource["tags"] = json!(tags);
}

/// Compute BLAKE3 hash of a file's contents, returned as a hex string.
pub fn hash_file_blake3(path: &Path) -> Result<String> {
    let content = std::fs::read(path).with_context(|| format!("Failed to read file for hashing: {}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(&content);
    Ok(hasher.finalize().to_hex().to_string())
}

/// Reserved key under which the optional nonce is folded into the identity hash,
/// so it can never collide with a real hashed field name.
const IDENTITY_HASH_NONCE_KEY: &str = "__nonce__";

/// Deterministic, key-order-independent identity hash over a resource's declared
/// input fields plus an optional nonce. Builds canonical JSON (all object keys
/// BTreeMap-sorted, no whitespace) of each present, non-null field value under its
/// field name, plus the nonce value under a reserved key; hashes with BLAKE3;
/// returns the first `length` hex chars. Missing or null fields are omitted
/// (null ≈ absent), so reordering YAML keys — or dropping a null — yields the same
/// hash. `fields` order is irrelevant (the BTreeMap sorts).
pub fn identity_hash(resource: &Value, fields: &[String], nonce_field: Option<&str>, length: usize) -> String {
    let mut canonical: BTreeMap<String, Value> = BTreeMap::new();
    for field in fields {
        if let Some(v) = resource.get(field)
            && !v.is_null()
        {
            canonical.insert(field.clone(), canonicalize_value(v));
        }
    }
    if let Some(nf) = nonce_field
        && let Some(v) = resource.get(nf)
        && !v.is_null()
    {
        canonical.insert(IDENTITY_HASH_NONCE_KEY.to_string(), canonicalize_value(v));
    }
    let serialized = serde_json::to_string(&canonical).unwrap_or_default();
    blake3::hash(serialized.as_bytes()).to_hex().to_string().chars().take(length).collect()
}

/// Recursively sort object keys so equal data hashes equally regardless of key
/// order at any depth. Arrays keep their order (order is semantic there).
fn canonicalize_value(value: &Value) -> Value {
    match value {
        Value::Object(map) => {
            // serde_json::Map preserves insertion order, so inserting in BTreeMap
            // (sorted) order yields a canonically ordered object — infallibly.
            let mut sorted: BTreeMap<String, Value> = BTreeMap::new();
            for (k, v) in map {
                sorted.insert(k.clone(), canonicalize_value(v));
            }
            Value::Object(sorted.into_iter().collect())
        }
        Value::Array(arr) => Value::Array(arr.iter().map(canonicalize_value).collect()),
        other => other.clone(),
    }
}

/// Read the `run-hash:<hex>` tag from a resource's `tags` array. Mirrors
/// `extract_source_hash` (prefix `source-hash:`) but for the identity-hash model;
/// the two prefixes are independent so a tool artifact and a job run can coexist.
pub fn extract_run_hash(resource: &Value) -> Option<&str> {
    resource.get("tags").and_then(|v| v.as_array()).and_then(|tags| tags.iter().find_map(|t| t.as_str()?.strip_prefix("run-hash:")))
}

/// Set (replacing any existing) the `run-hash:<hex>` tag on a resource's `tags`
/// array, leaving `source-hash:` and other tags untouched. Mirrors
/// `set_source_hash_tag`.
pub fn set_run_hash_tag(resource: &mut Value, hash: &str) {
    let mut tags: Vec<Value> = resource.get("tags").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().filter(|t| !t.as_str().is_some_and(|s| s.starts_with("run-hash:"))).collect();
    tags.push(json!(format!("run-hash:{}", hash)));
    resource["tags"] = json!(tags);
}

/// Reserved env-variable name carrying the identity hash for `identity_hash.storage:
/// env_marker` kinds (job_run). Both CPDaaS and CP4D clobber the submitted run name
/// to `"Notebook Job"` (live-pinned 2026-07-05, both deployments), so the name cannot
/// carry identity; `entity.job_run.configuration` round-trips verbatim, so the hash
/// rides there as a `WXCTL_IDENTITY=<hash>` entry instead. The key is reserved: any
/// user-declared entry with this name is replaced by the injected marker.
pub const IDENTITY_ENV_KEY: &str = "WXCTL_IDENTITY";

/// True when an `env_variables` entry is the identity marker — either the local
/// config shape (`{name: WXCTL_IDENTITY, value: …}`) or the wire shape
/// (`"WXCTL_IDENTITY=…"` string).
fn is_identity_env_entry(entry: &Value) -> bool {
    match entry {
        Value::String(s) => s.strip_prefix(IDENTITY_ENV_KEY).is_some_and(|rest| rest.starts_with('=')),
        Value::Object(_) => entry.get("name").and_then(|v| v.as_str()) == Some(IDENTITY_ENV_KEY),
        _ => false,
    }
}

/// Read an entry's marker value, from either shape (see `is_identity_env_entry`).
fn identity_env_value(entry: &Value) -> Option<&str> {
    match entry {
        Value::String(s) => s.strip_prefix(IDENTITY_ENV_KEY)?.strip_prefix('='),
        Value::Object(_) if entry.get("name").and_then(|v| v.as_str()) == Some(IDENTITY_ENV_KEY) => entry.get("value").and_then(|v| v.as_str()),
        _ => None,
    }
}

/// Inject (replacing any existing marker) a `{name: WXCTL_IDENTITY, value: <hash>}`
/// entry into a resource's local `env_variables` array — the config shape the
/// job_run handler folds into the wire's `"NAME=value"` strings at submit. Runs at
/// validation time (the `HashStorage::EnvMarker` stamp arm), AFTER the identity
/// hash is computed, so the marker never feeds back into the hash; idempotent, so
/// re-stamping can never accumulate markers.
pub fn set_identity_env_marker(resource: &mut Value, hash: &str) {
    let mut envs: Vec<Value> = resource.get("env_variables").and_then(|v| v.as_array()).cloned().unwrap_or_default().into_iter().filter(|e| !is_identity_env_entry(e)).collect();
    envs.push(json!({"name": IDENTITY_ENV_KEY, "value": hash}));
    resource["env_variables"] = json!(envs);
}

/// Remove any identity marker from a resource's `env_variables`, deleting the key
/// entirely when the marker was its only entry — so `strip(inject(data))` hashes
/// identically to the original (an empty array is present-and-non-null to
/// `identity_hash`, an absent field is omitted). The EnvMarker hash step hashes a
/// stripped copy, keeping the hash a function of user-declared inputs only.
pub fn strip_identity_env_marker(resource: &mut Value) {
    let Some(envs) = resource.get("env_variables").and_then(|v| v.as_array()) else {
        return;
    };
    let kept: Vec<Value> = envs.iter().filter(|e| !is_identity_env_entry(e)).cloned().collect();
    if kept.is_empty() {
        if let Some(obj) = resource.as_object_mut() {
            obj.remove("env_variables");
        }
    } else {
        resource["env_variables"] = json!(kept);
    }
}

/// Read the identity hash from a run item's `env_variables`, wherever the shape
/// puts them: the CAMS list/GET/201 envelope (`entity.job_run.configuration.env_variables`,
/// wire `"NAME=value"` strings — live-verified round-trip on both SaaS and CP4D,
/// 2026-07-05), a bare `configuration.env_variables`, or a top-level `env_variables`
/// (local/denormalized shape, either string or `{name, value}` entries).
pub fn extract_identity_env_marker(item: &Value) -> Option<&str> {
    let envs = item.pointer("/entity/job_run/configuration/env_variables").or_else(|| item.pointer("/configuration/env_variables")).or_else(|| item.get("env_variables"))?.as_array()?;
    envs.iter().find_map(identity_env_value)
}

/// Ordering rank for run items sharing one identity marker (duplicates are residue
/// of the pre-fix create-loop bug): Completed (0) is the stable adopt, a non-terminal
/// in-flight run (1) can be tail-polled, Failed/Canceled/CompletedWithErrors or
/// unknown (2) last. A stable sort by this key keeps list order among equals.
pub fn job_run_state_rank(item: &Value) -> u8 {
    let state = item.pointer("/entity/job_run/state").or_else(|| item.pointer("/metadata/state")).or_else(|| item.get("state")).and_then(|v| v.as_str()).unwrap_or_default();
    if state.eq_ignore_ascii_case("completed") {
        0
    } else if !state.is_empty() && !["failed", "canceled", "completedwitherrors"].iter().any(|s| state.eq_ignore_ascii_case(s)) {
        1
    } else {
        2
    }
}

/// Create deterministic ZIP file options with a fixed timestamp (1980-01-01).
/// Ensures ZIP artifacts are reproducible regardless of build time.
pub fn deterministic_zip_options() -> FileOptions<'static, ()> {
    let fixed_time = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).expect("Fixed timestamp should always be valid");
    FileOptions::default().compression_method(zip::CompressionMethod::Deflated).last_modified_time(fixed_time)
}

/// Fetch every item from a paginated IBM API list endpoint, flattening across
/// pages. `items_key` names the array field holding the page's items — e.g.
/// `"resources"` (common-core/WKC), `"items"` (Concert resilience), or
/// `"source_repos"` (Concert core). A bare-array response (no envelope) is
/// returned as-is. Two pagination styles are supported transparently:
///
/// - common-core: a `next.href` link, followed until absent;
/// - Concert: a `pagination` object carrying `page_number`/`total_pages`,
///   followed by re-requesting with the `page_number` query param bumped. The
///   initial request stays bare, so the single-page path is unchanged.
pub async fn fetch_all_pages(client: &wxctl_core::client::HttpClient, operation_id: &str, initial_endpoint: &str, items_key: &str) -> Result<Vec<serde_json::Value>> {
    // Defensive cap: some IBM APIs have been observed returning a `next.href` that
    // points back at the current page, which would loop forever without the
    // same-endpoint guard below; the cap catches any other pathological chain.
    const MAX_PAGES: usize = 1000;

    let mut all_items = Vec::new();
    let mut endpoint = initial_endpoint.to_string();

    for _page in 0..MAX_PAGES {
        let mut response: serde_json::Value = client.get(operation_id, &endpoint).await?;

        // Bare-array response: no envelope, no further pages.
        if let serde_json::Value::Array(arr) = &mut response {
            all_items.append(arr);
            return Ok(all_items);
        }

        if let Some(items) = response.get_mut(items_key).and_then(|r| r.as_array_mut()) {
            all_items.extend(std::mem::take(items));
        }

        match next_page_endpoint(&response, initial_endpoint) {
            Some(next) if next != endpoint => endpoint = next,
            Some(_) => {
                tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, endpoint = %endpoint, "pagination did not advance (next page == current) — stopping to avoid an infinite loop");
                return Ok(all_items);
            }
            None => return Ok(all_items),
        }
    }

    anyhow::bail!("[{operation_id}] pagination exceeded {MAX_PAGES} pages starting from {initial_endpoint} — aborting (likely a server-side pagination bug)")
}

/// Decide the next page's endpoint from a list response, or `None` when the
/// listing is complete. Handles common-core `next.href` links and Concert's
/// `pagination.{page_number,total_pages}` page-number scheme (applied against
/// `base_endpoint`). Pure — unit-tested below.
fn next_page_endpoint(response: &serde_json::Value, base_endpoint: &str) -> Option<String> {
    // common-core: absolute `next.href` link.
    if let Some(next_href) = response.get("next").and_then(|n| n.get("href")).and_then(|h| h.as_str()) {
        return Some(match url::Url::parse(next_href) {
            Ok(parsed) => match parsed.query() {
                Some(q) => format!("{}?{}", parsed.path(), q),
                None => parsed.path().to_string(),
            },
            Err(_) => next_href.to_string(),
        });
    }
    // Concert: page-number pagination via the `pagination` object.
    let pagination = response.get("pagination")?;
    let page = pagination.get("page_number").and_then(serde_json::Value::as_u64)?;
    let total = pagination.get("total_pages").and_then(serde_json::Value::as_u64)?;
    (page < total).then(|| with_page_number(base_endpoint, page + 1))
}

/// Set the `page_number` query param on a path (replacing any existing one),
/// preserving other query params.
fn with_page_number(endpoint: &str, page: u64) -> String {
    let (path, query) = endpoint.split_once('?').map_or((endpoint, None), |(p, q)| (p, Some(q)));
    let mut params: Vec<String> = query.into_iter().flat_map(|q| q.split('&')).filter(|kv| !kv.is_empty() && !kv.starts_with("page_number=")).map(str::to_string).collect();
    params.push(format!("page_number={page}"));
    format!("{path}?{}", params.join("&"))
}

/// One attempt's classification, returned (alongside the threaded loop state) by a
/// `poll_until` probe closure. `Done` carries the value `poll_until` returns
/// (callers that ignore it pass `Value::Null`; engine pollers pass the refreshed
/// resource; best-effort callers pass a non-`Null` marker so they can distinguish
/// terminal-Done from exhaustion). `Failed` carries the bail detail; `Pending` keeps
/// polling.
pub enum PollOutcome {
    Done(Value),
    Failed(String),
    Pending,
}

/// How `poll_until` resolves after `max_attempts` without a terminal outcome.
pub enum PollTimeout {
    /// Bail with the given message (the common status-poll behavior).
    Bail(String),
    /// Return `Ok(Value::Null)` — best-effort pollers (e.g. `sal_glossary`) that
    /// log a warning in their caller and never positively fail the apply.
    BestEffort,
}

/// Generic status-poll loop: run `probe` up to `max_attempts` times, sleeping
/// `interval` between attempts (not after the last), until it returns a terminal
/// outcome. `state` is the caller's loop-carried memo (e.g. the previous observed
/// status for change-detection logging); it is moved into `probe` and returned each
/// attempt — owned, not borrowed, because a probe future that borrowed `state`
/// across its `async` block does not satisfy `FnMut`'s lifetime rules. The probe
/// owns the GET, status extraction, log-on-status-change, and per-attempt error
/// handling — `poll_until` only sequences attempts and applies the timeout policy.
/// Returns the `Done` value, bails on `Failed`, and applies `timeout` on exhaustion.
pub async fn poll_until<S, F, Fut>(max_attempts: u32, interval: Duration, timeout: PollTimeout, mut state: S, mut probe: F) -> Result<Value>
where
    F: FnMut(u32, S) -> Fut,
    Fut: Future<Output = Result<(PollOutcome, S)>>,
{
    for attempt in 1..=max_attempts {
        let (outcome, next) = probe(attempt, state).await?;
        state = next;
        match outcome {
            PollOutcome::Done(v) => return Ok(v),
            PollOutcome::Failed(detail) => bail!("{detail}"),
            PollOutcome::Pending => {}
        }
        if attempt < max_attempts {
            tokio::time::sleep(interval).await;
        }
    }
    let _ = state;
    match timeout {
        PollTimeout::Bail(msg) => bail!("{msg}"),
        PollTimeout::BestEffort => Ok(Value::Null),
    }
}

/// Upload an artifact ZIP via `uploader`, then best-effort `remove_dir_all` the
/// artifact's parent dir on success. The uploader is a closure so callers can wrap
/// it (e.g. `tool.rs`'s TRM-race retry) while sharing the cleanup.
pub async fn upload_artifact_and_cleanup<F, Fut>(artifact_path: &str, uploader: F) -> Result<()>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    uploader().await?;

    if let Some(parent) = Path::new(artifact_path).parent() {
        let _ = std::fs::remove_dir_all(parent);
    }

    Ok(())
}

/// Shared `tool.rs` hash-gate tail for `pre_update_python` / `pre_update_openapi`:
/// compare the server vs local source-hash; if unchanged, skip the build and reuse
/// the hash; else run `build` (the per-binding artifact builder) for a fresh
/// `(path, hash)`. Set or remove the `artifact` field accordingly, then tag the
/// source hash. `build` is a closure carrying the binding-specific builder.
pub async fn reconcile_artifact_by_hash<B, BFut>(current: &Value, resource: &mut Value, build: B) -> Result<()>
where
    B: FnOnce() -> BFut,
    BFut: Future<Output = Result<(PathBuf, String)>>,
{
    let current_hash = extract_source_hash(current);
    let desired_hash = extract_source_hash(resource);

    let (artifact_path, source_hash) = match (current_hash, desired_hash) {
        (Some(current), Some(desired)) if current == desired => (None, desired.to_string()),
        _ => {
            let (path, hash) = build().await?;
            (Some(path), hash)
        }
    };

    if let Some(path) = artifact_path {
        resource["artifact"] = json!({ "path": path.to_string_lossy().to_string() });
    } else if let Some(m) = resource.as_object_mut() {
        m.remove("artifact");
    }

    set_source_hash_tag(resource, &source_hash);
    Ok(())
}

#[cfg(test)]
mod pagination_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn next_page_endpoint_follows_next_href() {
        // common-core: an absolute next.href is normalized to path?query.
        let resp = json!({"resources": [], "next": {"href": "https://host/v2/things?limit=10&start=abc"}});
        assert_eq!(next_page_endpoint(&resp, "/v2/things"), Some("/v2/things?limit=10&start=abc".to_string()));
        // Last page: no next link, no pagination object.
        assert_eq!(next_page_endpoint(&json!({"resources": []}), "/v2/things"), None);
    }

    #[test]
    fn next_page_endpoint_follows_concert_page_number() {
        // page_number < total_pages → bump page_number on the base endpoint.
        let resp = json!({"items": [], "pagination": {"page_number": 1, "total_pages": 3}});
        assert_eq!(next_page_endpoint(&resp, "/resilience/library"), Some("/resilience/library?page_number=2".to_string()));
        // Last page → done.
        let last = json!({"items": [], "pagination": {"page_number": 3, "total_pages": 3}});
        assert_eq!(next_page_endpoint(&last, "/resilience/library"), None);
        // No total_pages → treated as complete (e.g. compliance's total_items shape).
        assert_eq!(next_page_endpoint(&json!({"profiles": [], "total_items": 2}), "/x"), None);
    }

    #[test]
    fn with_page_number_sets_and_replaces() {
        assert_eq!(with_page_number("/x", 2), "/x?page_number=2");
        assert_eq!(with_page_number("/x?sort_by=name", 2), "/x?sort_by=name&page_number=2");
        // An existing page_number is replaced, not duplicated.
        assert_eq!(with_page_number("/x?page_number=1&sort_by=name", 5), "/x?sort_by=name&page_number=5");
    }
}

#[cfg(test)]
mod identity_hash_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn identity_hash_is_deterministic_and_key_order_independent() {
        let fields = vec!["training_data".to_string(), "prediction_type".to_string(), "scoring".to_string()];
        let base = json!({"training_data": "d1", "prediction_type": "binary", "scoring": "roc_auc"});
        let h = identity_hash(&base, &fields, None, 8);
        assert_eq!(h.len(), 8);

        // Reordering YAML keys → same hash.
        let reordered = json!({"scoring": "roc_auc", "training_data": "d1", "prediction_type": "binary"});
        assert_eq!(identity_hash(&reordered, &fields, None, 8), h);

        // Reordering the `fields` slice → same hash.
        let fields_rev = vec!["scoring".to_string(), "training_data".to_string(), "prediction_type".to_string()];
        assert_eq!(identity_hash(&base, &fields_rev, None, 8), h);

        // null ≈ absent for a hashed field.
        let with_null = json!({"training_data": "d1", "prediction_type": "binary", "scoring": "roc_auc", "holdout_size": null});
        let fields_plus = vec!["training_data".to_string(), "prediction_type".to_string(), "scoring".to_string(), "holdout_size".to_string()];
        assert_eq!(identity_hash(&with_null, &fields_plus, None, 8), h);

        // Changing a hashed value → different hash.
        let changed = json!({"training_data": "d1", "prediction_type": "binary", "scoring": "accuracy"});
        assert_ne!(identity_hash(&changed, &fields, None, 8), h);

        // Nonce folds in and changes the hash; bumping it changes it again.
        let g1 = json!({"training_data": "d1", "prediction_type": "binary", "scoring": "roc_auc", "generation": "1"});
        let g2 = json!({"training_data": "d1", "prediction_type": "binary", "scoring": "roc_auc", "generation": "2"});
        let hg1 = identity_hash(&g1, &fields, Some("generation"), 8);
        assert_ne!(hg1, h);
        assert_ne!(identity_hash(&g2, &fields, Some("generation"), 8), hg1);
    }

    #[test]
    fn identity_env_marker_injection_does_not_feed_back_into_the_hash() {
        let fields = vec!["job".to_string(), "env_variables".to_string(), "project_id".to_string()];

        // The EnvMarker stamp step hashes a marker-stripped copy: injecting the
        // marker (and re-injecting it) must never change the hash it carries.
        let base = json!({"job": "j-1", "env_variables": [{"name": "MODEL", "value": "m"}], "project_id": "p-1"});
        let h = identity_hash(&base, &fields, Some("generation"), 8);
        let mut stamped = base.clone();
        set_identity_env_marker(&mut stamped, &h);
        assert_eq!(extract_identity_env_marker(&stamped), Some(h.as_str()), "marker readable from the local shape");
        let mut stripped = stamped.clone();
        strip_identity_env_marker(&mut stripped);
        assert_eq!(identity_hash(&stripped, &fields, Some("generation"), 8), h, "strip(inject(data)) hashes identically — no self-reference");

        // Re-stamping replaces, never accumulates.
        set_identity_env_marker(&mut stamped, "def67890");
        let markers = stamped["env_variables"].as_array().unwrap().iter().filter(|e| e.get("name").and_then(|v| v.as_str()) == Some(IDENTITY_ENV_KEY)).count();
        assert_eq!(markers, 1);
        assert_eq!(extract_identity_env_marker(&stamped), Some("def67890"));

        // No user env_variables: injection creates the array; stripping removes the
        // key entirely so absent-vs-empty can't fork the hash.
        let bare = json!({"job": "j-1", "project_id": "p-1"});
        let hb = identity_hash(&bare, &fields, Some("generation"), 8);
        let mut bare_stamped = bare.clone();
        set_identity_env_marker(&mut bare_stamped, &hb);
        strip_identity_env_marker(&mut bare_stamped);
        assert!(bare_stamped.get("env_variables").is_none(), "marker-only array removed entirely");
        assert_eq!(identity_hash(&bare_stamped, &fields, Some("generation"), 8), hb);

        // Generation bump still changes the hash (AC6: bump → exactly one new run).
        let mut bumped = base.clone();
        bumped["generation"] = json!(2);
        assert_ne!(identity_hash(&bumped, &fields, Some("generation"), 8), h, "bumped generation → new hash → new marker");
    }

    #[test]
    fn extract_identity_env_marker_reads_remote_and_local_shapes() {
        // CAMS list/GET/201 envelope, wire "NAME=value" strings (the live shape).
        let cams = json!({"metadata": {"name": "Notebook Job"}, "entity": {"job_run": {"configuration": {"env_variables": ["MODEL=m", "WXCTL_IDENTITY=abc12345"]}}}});
        assert_eq!(extract_identity_env_marker(&cams), Some("abc12345"));

        // Bare configuration envelope and top-level local object shape.
        assert_eq!(extract_identity_env_marker(&json!({"configuration": {"env_variables": ["WXCTL_IDENTITY=beef0001"]}})), Some("beef0001"));
        assert_eq!(extract_identity_env_marker(&json!({"env_variables": [{"name": "WXCTL_IDENTITY", "value": "beef0002"}]})), Some("beef0002"));

        // Near-miss keys never match: prefix without '=', different variable.
        assert_eq!(extract_identity_env_marker(&json!({"env_variables": ["WXCTL_IDENTITY_EXTRA=x", "OTHER=y"]})), None);
        assert_eq!(extract_identity_env_marker(&json!({"metadata": {"name": "Notebook Job"}})), None);
    }

    #[test]
    fn job_run_state_rank_orders_completed_then_active_then_failed() {
        let run = |state: &str| json!({"entity": {"job_run": {"state": state}}});
        assert_eq!(job_run_state_rank(&run("Completed")), 0);
        assert_eq!(job_run_state_rank(&run("completed")), 0);
        assert_eq!(job_run_state_rank(&run("Running")), 1);
        assert_eq!(job_run_state_rank(&run("Starting")), 1);
        assert_eq!(job_run_state_rank(&run("Failed")), 2);
        assert_eq!(job_run_state_rank(&run("Canceled")), 2);
        assert_eq!(job_run_state_rank(&run("CompletedWithErrors")), 2);
        assert_eq!(job_run_state_rank(&json!({"metadata": {"state": "Completed"}})), 0, "metadata.state fallback");
        assert_eq!(job_run_state_rank(&json!({"state": "Queued"})), 1, "top-level state fallback");
        assert_eq!(job_run_state_rank(&json!({})), 2, "unknown state ranks last");
    }

    #[test]
    fn run_hash_tag_round_trips_independently_of_source_hash() {
        let mut r = json!({"name": "exp"});
        set_run_hash_tag(&mut r, "abc12345");
        assert_eq!(extract_run_hash(&r), Some("abc12345"));

        // Replacing keeps exactly one run-hash tag.
        set_run_hash_tag(&mut r, "def67890");
        assert_eq!(extract_run_hash(&r), Some("def67890"));
        let count = r.get("tags").unwrap().as_array().unwrap().iter().filter(|t| t.as_str().unwrap().starts_with("run-hash:")).count();
        assert_eq!(count, 1);

        // source-hash and run-hash coexist.
        set_source_hash_tag(&mut r, "srchash");
        assert_eq!(extract_run_hash(&r), Some("def67890"));
        assert_eq!(extract_source_hash(&r), Some("srchash"));
    }
}
