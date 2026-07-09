use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::LazyLock;
use tracing::{Instrument, debug, warn};
use wxctl_core::client::HttpClient;
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

/// Maximum number of status polling attempts
const MAX_POLLING_ATTEMPTS: u32 = 60;

/// Interval between status polling attempts in seconds
const POLLING_INTERVAL_SECS: u64 = 10;

/// Internal JSON key used to shuttle document paths from post_validate to pre_create/pre_update.
const INTERNAL_DOCUMENTS_KEY: &str = "_documents";

pub struct KnowledgeBaseHandler;

impl ResourceHandler for KnowledgeBaseHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let documents = extract_documents(resource)?;

            // Strip internal field before filtering — must not leak into API body
            if let Some(obj) = resource.as_object_mut() {
                obj.remove(INTERNAL_DOCUMENTS_KEY);
            }

            let filtered = wxctl_core::filter_request_fields(resource, fields)?;

            let file_paths: Vec<PathBuf> = documents.iter().map(PathBuf::from).collect();
            let file_refs: Vec<&Path> = file_paths.iter().map(|p| p.as_path()).collect();

            let response: Value = client.create_multipart(operation_id, endpoint, "knowledge_base", filtered, file_refs, "files").await.context("Failed to create knowledge base")?;

            let normalized = normalize_kb_response(response);

            // Only poll for readiness when documents were uploaded.
            // A KB without documents will never reach "ready" status.
            if !documents.is_empty() {
                let kb_id = extract_kb_id(&normalized, "create")?;
                wait_for_knowledge_base_ready(client, kb_id, operation_id).await.context("Failed waiting for knowledge base to be ready after create")?;
            }

            Ok(HookOutcome::Handled(normalized))
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, desired: &'a mut Value, fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let id = current.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No id in current knowledge_base for update"))?;
            let resolved_endpoint = endpoint.replace("{id}", id);

            // Compare document hashes to decide whether to re-upload files
            let remote_hash = current.get("description").and_then(|v| v.as_str()).and_then(extract_doc_hash);
            let local_hash = desired.get("description").and_then(|v| v.as_str()).and_then(extract_doc_hash);

            let docs_changed = match (local_hash, remote_hash) {
                (Some(local), Some(remote)) => local != remote,
                (Some(_), None) => true, // local has docs, remote doesn't
                (None, _) => false,      // no local docs (external vector index)
            };

            let documents = extract_documents(desired)?;

            // Strip internal field before filtering — must not leak into API body
            if let Some(obj) = desired.as_object_mut() {
                obj.remove(INTERNAL_DOCUMENTS_KEY);
            }

            let filtered = wxctl_core::filter_request_fields(desired, fields)?;

            let response: Value = if docs_changed {
                let file_paths: Vec<PathBuf> = documents.iter().map(PathBuf::from).collect();
                let file_refs: Vec<&Path> = file_paths.iter().map(|p| p.as_path()).collect();

                debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "knowledge_base",
                    num_files = file_refs.len(),
                    "knowledge_base update: uploading documents"
                );

                client.update_multipart(operation_id, &resolved_endpoint, "knowledge_base", filtered, file_refs, "files").await.context("Failed to update knowledge base with documents")?
            } else {
                debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "knowledge_base",
                    "knowledge_base update: metadata-only (documents unchanged)"
                );

                let no_files: Vec<&Path> = vec![];
                client.update_multipart(operation_id, &resolved_endpoint, "knowledge_base", filtered, no_files, "files").await.context("Failed to update knowledge base metadata")?
            };

            let normalized = normalize_kb_response(response);

            // Only poll for readiness when documents were re-uploaded.
            // Metadata-only updates don't trigger reindexing.
            if docs_changed {
                let kb_id = extract_kb_id(&normalized, "update")?;
                wait_for_knowledge_base_ready(client, kb_id, operation_id).await.context("Failed waiting for knowledge base to be ready after update")?;
            }

            Ok(HookOutcome::Handled(normalized))
        })
    }

    fn post_validate<'a>(&'a self, resource: &'a mut Value, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            let documents = extract_documents(resource)?;

            if !documents.is_empty() {
                // Compute hash and inject into description
                let hash = compute_documents_hash(&documents)?;
                let current_desc = resource.get("description").and_then(|v| v.as_str());
                let new_desc = append_doc_hash(current_desc, &hash);
                resource["description"] = Value::String(new_desc);

                // Move documents to internal field for pre_create/pre_update
                let docs_value = resource["documents"].take();
                resource[INTERNAL_DOCUMENTS_KEY] = docs_value;
            }

            Ok(())
        })
    }
}

impl KnowledgeBaseHandler {
    /// Ingest additional documents into existing knowledge base
    ///
    /// This is separate from update - it only uploads files without modifying
    /// any knowledge base fields. Uses PUT method per API specification.
    pub async fn ingest_documents<'a>(&'a self, knowledge_base_id: &'a str, file_paths: Vec<String>, client: &'a HttpClient, operation_id: &'a str) -> Result<Value> {
        let endpoint = format!("/v1/orchestrate/knowledge-bases/{}/documents", knowledge_base_id);

        let paths: Vec<PathBuf> = file_paths.iter().map(PathBuf::from).collect();
        let file_refs: Vec<&Path> = paths.iter().map(|p| p.as_path()).collect();

        let response = client.ingest_multipart(operation_id, &endpoint, file_refs, "files").await.context("Failed to ingest additional documents")?;

        // Wait for processing
        wait_for_knowledge_base_ready(client, knowledge_base_id, operation_id).await?;

        Ok(response)
    }
}

/// Compute BLAKE3 hash of document file contents.
/// Files are sorted by path for deterministic ordering.
/// Returns first 12 hex chars formatted as "xxxx-xxxx-xxxx".
fn compute_documents_hash(file_paths: &[String]) -> Result<String> {
    use blake3::Hasher;

    let mut sorted_paths: Vec<&String> = file_paths.iter().collect();
    sorted_paths.sort();

    let mut hasher = Hasher::new();
    for path in &sorted_paths {
        let content = std::fs::read(path).with_context(|| format!("Failed to read document for hashing: {}", path))?;
        // Length-prefix each file to prevent boundary collisions
        // (e.g. files ["ab","c"] vs ["a","bc"] must hash differently)
        hasher.update(&(content.len() as u64).to_le_bytes());
        hasher.update(&content);
    }

    let hex = hasher.finalize().to_hex().to_string();
    let short = &hex[..12];
    Ok(format!("{}-{}-{}", &short[0..4], &short[4..8], &short[8..12]))
}

/// Regex matching the doc hash suffix: [xxxx-xxxx-xxxx] at end of string
static DOC_HASH_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\s*\[([0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4})\]\s*$").unwrap());

/// Append document hash to description.
/// "My KB" + "a1b2-c3d4-e5f6" -> "My KB [a1b2-c3d4-e5f6]"
/// None + "a1b2-c3d4-e5f6" -> "[a1b2-c3d4-e5f6]"
fn append_doc_hash(description: Option<&str>, hash: &str) -> String {
    match description.filter(|d| !d.is_empty()) {
        Some(desc) => {
            let stripped = strip_doc_hash(desc);
            if stripped.is_empty() { format!("[{}]", hash) } else { format!("{} [{}]", stripped, hash) }
        }
        None => format!("[{}]", hash),
    }
}

/// Extract document hash from description.
/// "My KB [a1b2-c3d4-e5f6]" -> Some("a1b2-c3d4-e5f6")
/// "My KB" -> None
fn extract_doc_hash(description: &str) -> Option<&str> {
    DOC_HASH_RE.captures(description).map(|caps| caps.get(1).unwrap().as_str())
}

/// Strip document hash from description.
/// "My KB [a1b2-c3d4-e5f6]" -> "My KB"
/// "[a1b2-c3d4-e5f6]" -> ""
fn strip_doc_hash(description: &str) -> &str {
    match DOC_HASH_RE.find(description) {
        Some(m) => description[..m.start()].trim_end(),
        None => description,
    }
}

/// Extract the knowledge base ID from a normalized API response.
/// The API returns the ID as "knowledge_base" (POST) or "id" (GET);
/// `normalize_kb_response` copies the former into the latter, so "id"
/// should always be present — but we keep the fallback for safety.
fn extract_kb_id<'a>(response: &'a Value, context: &str) -> Result<&'a str> {
    response.get("knowledge_base").or_else(|| response.get("id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No knowledge base ID in {} response", context))
}

/// Extract document file paths from payload.
/// Checks both "_documents" (post-post_validate) and "documents" (pre-post_validate).
fn extract_documents(payload: &Value) -> Result<Vec<String>> {
    let mut file_paths = Vec::new();

    let docs = payload.get(INTERNAL_DOCUMENTS_KEY).or_else(|| payload.get("documents")).and_then(|v| v.as_array());

    if let Some(docs) = docs {
        for doc in docs {
            if let Some(obj) = doc.as_object() {
                if let Some(path) = obj.get("path").and_then(|v| v.as_str()) {
                    file_paths.push(path.to_string());
                }
            } else if let Some(path) = doc.as_str() {
                file_paths.push(path.to_string());
            }
        }
    }

    Ok(file_paths)
}

/// Poll knowledge base status endpoint until ready or timeout
///
/// Implements resilient polling with:
/// - Retry on transient errors (429, 5xx, network failures)
/// - Exponential backoff with jitter
/// - Clear timeout semantics
fn wait_for_knowledge_base_ready<'a>(client: &'a HttpClient, knowledge_base_id: &'a str, operation_id: &'a str) -> impl Future<Output = Result<()>> + Send + 'a {
    use tokio::time::{Duration, sleep};

    let span = tracing::debug_span!(
        target: "wxctl::substage::provider",
        "wait_knowledge_base_ready",
        operation_id = %operation_id,
        knowledge_base_id = %knowledge_base_id,
        max_attempts = MAX_POLLING_ATTEMPTS
    );

    async move {
        let mut consecutive_errors = 0u32;
        let mut prev_index_status: Option<String> = None;
        const MAX_CONSECUTIVE_ERRORS: u32 = 5;

        for attempt in 1..=MAX_POLLING_ATTEMPTS {
            let status_endpoint = format!("/v1/orchestrate/knowledge-bases/{}/status", knowledge_base_id);

            match client.get::<Value>(operation_id, &status_endpoint).await {
                Ok(status) => {
                    // Reset error counter on successful response
                    consecutive_errors = 0;

                    let is_ready = status.get("ready").and_then(|v| v.as_bool()).unwrap_or(false);
                    let index_status = status.get("built_in_index_status").and_then(|v| v.as_str()).unwrap_or("unknown");
                    let status_msg = status.get("built_in_index_status_msg").and_then(|v| v.as_str()).unwrap_or("");

                    if prev_index_status.as_deref() != Some(index_status) {
                        debug!(
                            target: "wxctl::substage::provider",
                            operation_id = %operation_id,
                            resource_type = "knowledge_base",
                            knowledge_base_id = %knowledge_base_id,
                            attempt = attempt,
                            max_attempts = MAX_POLLING_ATTEMPTS,
                            ready = is_ready,
                            index_status = %index_status,
                            status_msg = %status_msg,
                            "knowledge_base status observed"
                        );
                        prev_index_status = Some(index_status.to_string());
                    }

                    if is_ready && index_status == "ready" {
                        return Ok(());
                    }

                    // Terminal failure states: the indexer reports "error" or "not_ready"
                    // when ingestion can't proceed (e.g. no built-in vector store on
                    // Software/CP4D). Surface the indexer message instead of polling to a
                    // generic timeout. Matches the ADK's _poll_knowledge_base_status.
                    if index_status == "error" || index_status == "not_ready" {
                        let detail = if status_msg.is_empty() { String::new() } else { format!(": {status_msg}") };
                        return Err(anyhow::anyhow!("Knowledge base {knowledge_base_id} indexing failed (built_in_index_status={index_status}){detail}"));
                    }

                    // Not ready yet, wait before next poll
                    if attempt < MAX_POLLING_ATTEMPTS {
                        sleep(Duration::from_secs(POLLING_INTERVAL_SECS)).await;
                    }
                }
                Err(e) => {
                    consecutive_errors += 1;

                    // Check if error is transient (retryable)
                    let is_transient = is_transient_error(&e);

                    if is_transient && consecutive_errors <= MAX_CONSECUTIVE_ERRORS && attempt < MAX_POLLING_ATTEMPTS {
                        // Transient error - retry with backoff
                        let backoff_secs = calculate_polling_backoff(consecutive_errors);

                        warn!(
                            target: "wxctl::substage::provider",
                            operation_id = %operation_id,
                            resource_type = "knowledge_base",
                            knowledge_base_id = %knowledge_base_id,
                            attempt = attempt,
                            consecutive_errors = consecutive_errors,
                            backoff_secs = backoff_secs,
                            error = %e,
                            "transient error during status polling; retrying with backoff"
                        );

                        sleep(Duration::from_secs(backoff_secs)).await;
                    } else {
                        // Non-transient error or too many consecutive errors
                        return Err(e).context(format!("Failed to poll knowledge base status after {} consecutive errors", consecutive_errors));
                    }
                }
            }
        }

        // Timeout reached
        Err(anyhow::anyhow!("Knowledge base {} not ready after {} attempts ({} seconds) - TIMEOUT", knowledge_base_id, MAX_POLLING_ATTEMPTS, MAX_POLLING_ATTEMPTS as u64 * POLLING_INTERVAL_SECS))
    }
    .instrument(span)
}

/// Check if an error is transient and should be retried.
///
/// Matches the `"HTTP {status}"` prefix used by wxctl-core's HttpClient error formatting
/// (see wxctl-core http.rs). Typed matching via `HttpError` is not possible because it is
/// `pub(crate)` in wxctl-core.
fn is_transient_error(error: &anyhow::Error) -> bool {
    let error_msg = error.to_string().to_lowercase();

    // Retryable HTTP status codes — match "http 4xx/5xx" prefix to avoid false positives
    error_msg.contains("http 429") ||
    error_msg.contains("http 500") ||
    error_msg.contains("http 502") ||
    error_msg.contains("http 503") ||
    error_msg.contains("http 504") ||
    // Network errors
    error_msg.contains("timeout") ||
    error_msg.contains("connection") ||
    error_msg.contains("dns")
}

/// Calculate backoff delay for polling retries
/// Uses exponential backoff: min(POLLING_INTERVAL_SECS * 2^(errors-1), 60)
fn calculate_polling_backoff(consecutive_errors: u32) -> u64 {
    let base_secs = POLLING_INTERVAL_SECS;
    let exponential = base_secs * 2u64.pow(consecutive_errors.saturating_sub(1));

    // Cap at 60 seconds maximum backoff
    std::cmp::min(exponential, 60)
}

/// Normalize knowledge base API response to ensure consistent field names
/// The POST /documents endpoint returns the KB ID in "knowledge_base" field,
/// but GET /knowledge-bases returns it in "id" field. This function ensures
/// "id" is always present for consistent reference resolution.
fn normalize_kb_response(mut response: Value) -> Value {
    if let Some(obj) = response.as_object_mut() {
        // If response has "knowledge_base" but not "id", copy it
        if obj.contains_key("knowledge_base")
            && !obj.contains_key("id")
            && let Some(kb_id) = obj.get("knowledge_base").cloned()
        {
            obj.insert("id".to_string(), kb_id);
        }
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_documents_hash_properties() {
        let dir = tempfile::tempdir().unwrap();
        let f1 = dir.path().join("a.txt");
        let f2 = dir.path().join("b.txt");
        std::fs::write(&f1, "content one").unwrap();
        std::fs::write(&f2, "content two").unwrap();
        let (p1, p2) = (f1.to_string_lossy().to_string(), f2.to_string_lossy().to_string());

        // Deterministic + format xxxx-xxxx-xxxx.
        let hash = compute_documents_hash(&[p1.clone(), p2.clone()]).unwrap();
        assert_eq!(hash, compute_documents_hash(&[p1.clone(), p2.clone()]).unwrap(), "deterministic");
        assert_eq!(hash.len(), 14);
        assert_eq!(&hash[4..5], "-");
        assert_eq!(&hash[9..10], "-");

        // Order-independent (paths sorted internally).
        assert_eq!(compute_documents_hash(&[p1.clone(), p2.clone()]).unwrap(), compute_documents_hash(&[p2, p1.clone()]).unwrap(), "order-independent");

        // Changes when file content changes.
        std::fs::write(&f1, "version two").unwrap();
        assert_ne!(hash, compute_documents_hash(&[p1]).unwrap(), "changes on content change");
    }

    #[test]
    fn test_append_doc_hash() {
        assert_eq!(append_doc_hash(Some("My KB"), "a1b2-c3d4-e5f6"), "My KB [a1b2-c3d4-e5f6]");
        assert_eq!(append_doc_hash(None, "a1b2-c3d4-e5f6"), "[a1b2-c3d4-e5f6]");
        assert_eq!(append_doc_hash(Some(""), "a1b2-c3d4-e5f6"), "[a1b2-c3d4-e5f6]");
        // Replaces existing hash
        assert_eq!(append_doc_hash(Some("My KB [0000-0000-0000]"), "a1b2-c3d4-e5f6"), "My KB [a1b2-c3d4-e5f6]");
    }

    #[test]
    fn test_extract_doc_hash() {
        assert_eq!(extract_doc_hash("My KB [a1b2-c3d4-e5f6]"), Some("a1b2-c3d4-e5f6"));
        assert_eq!(extract_doc_hash("[a1b2-c3d4-e5f6]"), Some("a1b2-c3d4-e5f6"));
        assert_eq!(extract_doc_hash("My KB"), None);
        assert_eq!(extract_doc_hash("My KB [not-a-hash]"), None);
        assert_eq!(extract_doc_hash(""), None);
    }

    #[test]
    fn test_strip_doc_hash() {
        assert_eq!(strip_doc_hash("My KB [a1b2-c3d4-e5f6]"), "My KB");
        assert_eq!(strip_doc_hash("[a1b2-c3d4-e5f6]"), "");
        assert_eq!(strip_doc_hash("My KB"), "My KB");
    }

    #[test]
    fn test_extract_documents() {
        // Reads paths from the internal `_documents` field.
        let from_internal = serde_json::json!({"_documents": [{"path": "a.pdf"}, {"path": "b.txt"}]});
        assert_eq!(extract_documents(&from_internal).unwrap(), vec!["a.pdf", "b.txt"], "reads _documents");

        // `_documents` (post-post_validate) is preferred over `documents`.
        let both = serde_json::json!({"documents": [{"path": "original.pdf"}], "_documents": [{"path": "moved.pdf"}]});
        assert_eq!(extract_documents(&both).unwrap(), vec!["moved.pdf"], "_documents preferred over documents");
    }
}
