use anyhow::{Context, Result, anyhow, bail};
use futures::future::join_all;
use serde_json::{Value, json};
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
/// Ensures the path is canonical and does not escape the current working directory.
pub fn validate_path(source_path: &Path) -> Result<PathBuf> {
    let canonical = std::fs::canonicalize(source_path).context(format!("Failed to canonicalize source path: {}", source_path.display()))?;

    let cwd = std::env::current_dir().context("Failed to get current working directory")?;

    let canonical_cwd = std::fs::canonicalize(&cwd).context("Failed to canonicalize working directory")?;

    if !canonical.starts_with(&canonical_cwd) {
        return Err(anyhow!("Path traversal detected: {} is outside working directory {}", canonical.display(), canonical_cwd.display()));
    }

    Ok(canonical)
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
pub fn resource_id(value: &Value) -> Option<&str> {
    value.pointer("/metadata/id").or_else(|| value.get("id")).and_then(|v| v.as_str())
}

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

/// Create deterministic ZIP file options with a fixed timestamp (1980-01-01).
/// Ensures ZIP artifacts are reproducible regardless of build time.
pub fn deterministic_zip_options() -> FileOptions<'static, ()> {
    let fixed_time = zip::DateTime::from_date_and_time(1980, 1, 1, 0, 0, 0).expect("Fixed timestamp should always be valid");
    FileOptions::default().compression_method(zip::CompressionMethod::Deflated).last_modified_time(fixed_time)
}

/// Fetch all resources from a paginated IBM API list endpoint.
/// Follows `next.href` links until all pages are consumed.
pub async fn fetch_all_pages(client: &wxctl_core::client::HttpClient, operation_id: &str, initial_endpoint: &str) -> Result<Vec<serde_json::Value>> {
    let mut all_resources = Vec::new();
    let mut endpoint = initial_endpoint.to_string();

    loop {
        let mut response: serde_json::Value = client.get(operation_id, &endpoint).await?;

        if let Some(resources) = response.get_mut("resources").and_then(|r| r.as_array_mut()) {
            all_resources.extend(std::mem::take(resources));
        }

        match response.get("next").and_then(|n| n.get("href")).and_then(|h| h.as_str()) {
            Some(next_href) => {
                endpoint = match url::Url::parse(next_href) {
                    Ok(parsed) => {
                        let path = parsed.path();
                        match parsed.query() {
                            Some(q) => format!("{}?{}", path, q),
                            None => path.to_string(),
                        }
                    }
                    Err(_) => next_href.to_string(),
                };
            }
            None => break,
        }
    }

    Ok(all_resources)
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
