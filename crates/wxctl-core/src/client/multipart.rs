use super::http::HttpClient;
use super::retry::{self, HttpError};
use anyhow::{Result, anyhow};
use reqwest::Method;
use reqwest::multipart::{Form, Part};
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use tokio::fs::File;
use tracing::Instrument;
use uuid::Uuid;

/// Maximum file size for multipart uploads (100MB)
/// Files larger than this will be rejected to prevent memory exhaustion
const MAX_FILE_SIZE_BYTES: u64 = 100 * 1024 * 1024; // 100MB

/// Maximum total size for all files in a single request (500MB)
const MAX_TOTAL_SIZE_BYTES: u64 = 500 * 1024 * 1024; // 500MB

/// Detect MIME type based on file extension
fn detect_mime_type(file_path: &Path) -> &'static str {
    let extension = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    match extension.to_lowercase().as_str() {
        "pdf" => "application/pdf",
        "txt" => "text/plain",
        "json" => "application/json",
        "xml" => "application/xml",
        "csv" => "text/csv",
        "doc" => "application/msword",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xls" => "application/vnd.ms-excel",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "ppt" => "application/vnd.ms-powerpoint",
        "pptx" => "application/vnd.openxmlformats-officedocument.presentationml.presentation",
        "html" | "htm" => "text/html",
        "md" => "text/markdown",
        "zip" => "application/zip",
        _ => "application/octet-stream", // fallback for unknown types
    }
}

impl HttpClient {
    /// Execute multipart/form-data request with file uploads
    /// Automatically acquires a concurrency permit before executing
    ///
    /// Enforces file size limits to prevent memory exhaustion:
    /// - Per-file limit: 100MB
    /// - Total request limit: 500MB
    pub async fn request_multipart<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, method: Method, path: &'a str, form_data: HashMap<String, Value>, files: Vec<&'a Path>, file_field_name: &'a str) -> Result<T> {
        let _permit = self.capacity.acquire(&self.service).await.map_err(|_| anyhow!("Capacity semaphore closed"))?;
        self.request_multipart_internal(operation_id, method, path, form_data, files, file_field_name).await
    }

    /// Internal multipart request without permit acquisition
    async fn request_multipart_internal<'a, T: DeserializeOwned + Send + 'a>(&'a self, operation_id: &'a str, method: Method, path: &'a str, form_data: HashMap<String, Value>, files: Vec<&'a Path>, file_field_name: &'a str) -> Result<T> {
        let url = format!("{}{}", self.base_url, path);
        let request_id = Uuid::new_v4().to_string();

        // Collect form field names for logging
        let form_data_keys: Vec<&String> = form_data.keys().collect();

        // Collect file names for logging
        let file_names_vec: Vec<String> = files.iter().filter_map(|p| p.file_name()?.to_str().map(|s| s.to_string())).collect();

        retry::with_retry(self.max_retries, async |attempt| {
            let span = tracing::trace_span!(
                target: "wxctl::substage::http",
                "http_multipart_request",
                operation_id = %operation_id,
                request_id = %request_id,
                method = %method.as_str(),
                path = %path,
                form_data = ?form_data_keys,
                file_count = files.len(),
                file_names = ?file_names_vec,
                attempt = attempt + 1,
                max_retries = self.max_retries,
                status = tracing::field::Empty,
                response_body = tracing::field::Empty
            );

            async {
                let token = self.token_manager.get_token(&self.client).await.map_err(|e| HttpError::without_status(e.to_string()))?;

                // Build multipart form
                let mut form = Form::new();

                // Add form fields. A bare JSON string is sent as a `text/plain` part carrying
                // the raw value — APIs validating a scalar form field reject the JSON quotes
                // (e.g. SAL's `replace_option`, validated against `^[A-Za-z0-9_-]+$`). Objects /
                // arrays / numbers are sent as `application/json` so structured body fields keep
                // proper schema validation (e.g. wxO knowledge_base metadata).
                for (key, value) in &form_data {
                    let part = match value {
                        Value::String(s) => Part::text(s.clone()),
                        _ => {
                            let json_bytes = serde_json::to_vec(value).map_err(|e| HttpError::without_status(format!("Failed to serialize {} field: {}", key, e)))?;
                            Part::bytes(json_bytes).mime_str("application/json").map_err(|e| HttpError::without_status(format!("Failed to set JSON mime type: {}", e)))?
                        }
                    };
                    form = form.part(key.clone(), part);
                }

                // Validate and add files with size enforcement
                let mut total_size: u64 = 0;

                for file_path in &files {
                    let file_name = file_path.file_name().and_then(|n| n.to_str()).ok_or_else(|| HttpError::without_status("Invalid file name".to_string()))?;

                    // Check file size before reading
                    let metadata = tokio::fs::metadata(file_path).await.map_err(|e| HttpError::without_status(format!("Failed to get file metadata {:?}: {}", file_path, e)))?;
                    let file_size = metadata.len();

                    // Enforce per-file size limit
                    if file_size > MAX_FILE_SIZE_BYTES {
                        return Err(HttpError::without_status(format!("File '{}' exceeds maximum size: {} bytes (limit: {} bytes / {}MB)", file_name, file_size, MAX_FILE_SIZE_BYTES, MAX_FILE_SIZE_BYTES / (1024 * 1024))));
                    }

                    // Enforce total size limit
                    total_size += file_size;
                    if total_size > MAX_TOTAL_SIZE_BYTES {
                        return Err(HttpError::without_status(format!("Total file size exceeds maximum: {} bytes (limit: {} bytes / {}MB)", total_size, MAX_TOTAL_SIZE_BYTES, MAX_TOTAL_SIZE_BYTES / (1024 * 1024))));
                    }

                    // Stream file instead of reading into memory for better memory efficiency
                    let file = File::open(file_path).await.map_err(|e| HttpError::without_status(format!("Failed to open file {:?}: {}", file_path, e)))?;

                    // Detect MIME type based on file extension
                    let mime_type = detect_mime_type(file_path);

                    // Create part from file stream
                    let part = Part::stream(file).file_name(file_name.to_string()).mime_str(mime_type).map_err(|e| HttpError::without_status(format!("Failed to set MIME type: {}", e)))?;
                    form = form.part(file_field_name.to_string(), part);
                }

                let mut req = self.client.request(method.clone(), &url);

                // Instance-scoped APIs (e.g. watsonx.data Software) convey the instance via
                // the `AuthInstanceId` header rather than the URL. `execute_internal` adds it;
                // mirror that here so multipart uploads to those surfaces aren't rejected with
                // "missing crn or account_id in header" (e.g. SAL glossary upload on Software).
                if let Some(instance_id) = &self.instance_id {
                    req = req.header("AuthInstanceId", instance_id);
                }

                req = self.apply_auth(req, &token)?;

                req = req.multipart(form);

                let resp = req.send().await.map_err(|e| HttpError::without_status(e.to_string()))?;
                let status = resp.status();

                // Record status on current span
                tracing::Span::current().record("status", status.as_u16());

                if status.is_success() {
                    let text = resp.text().await.map_err(|e| HttpError::without_status(format!("Failed to read response body: {}", e)))?;

                    // Parse raw text as JSON Value for logging before deserializing to T
                    let response_value = serde_json::from_str::<Value>(&text);
                    if let Ok(ref rv) = response_value {
                        tracing::Span::current().record("response_body", tracing::field::debug(rv));

                        crate::log_http_request!(operation_id, &request_id, method.as_str(), &url, status.as_u16(), &form_data, rv);
                    }

                    let body: T = serde_json::from_str(&text).map_err(|e| HttpError::without_status(format!("Failed to parse response: {}", e)))?;
                    return Ok(body);
                }

                // Non-success status
                let error_body = resp.text().await.unwrap_or_else(|_| String::from("Unable to read error body"));
                tracing::Span::current().record("response_body", tracing::field::debug(&error_body));

                crate::log_http_request!(operation_id, &request_id, method.as_str(), &url, status.as_u16(), &form_data, &error_body);

                Err(HttpError::with_status(status, method.clone(), format!("HTTP {} - {}", status, error_body)))
            }
            .instrument(span)
            .await
        })
        .await
    }

    /// Create resource with multipart/form-data
    pub async fn create_multipart<'a>(&'a self, operation_id: &'a str, endpoint: &'a str, body_field_name: &'a str, body: Value, files: Vec<&'a Path>, file_field_name: &'a str) -> Result<Value> {
        let mut form_data = HashMap::new();
        form_data.insert(body_field_name.to_string(), body);

        self.request_multipart(operation_id, Method::POST, endpoint, form_data, files, file_field_name).await
    }

    /// Update resource with multipart/form-data
    pub async fn update_multipart<'a>(&'a self, operation_id: &'a str, endpoint: &'a str, body_field_name: &'a str, body: Value, files: Vec<&'a Path>, file_field_name: &'a str) -> Result<Value> {
        let mut form_data = HashMap::new();
        form_data.insert(body_field_name.to_string(), body);

        self.request_multipart(operation_id, Method::PATCH, endpoint, form_data, files, file_field_name).await
    }

    /// Ingest additional files into existing resource (files-only, no JSON body)
    pub async fn ingest_multipart<'a>(&'a self, operation_id: &'a str, endpoint: &'a str, files: Vec<&'a Path>, file_field_name: &'a str) -> Result<Value> {
        // Empty form data - only files are uploaded
        let form_data = HashMap::new();

        self.request_multipart(operation_id, Method::PUT, endpoint, form_data, files, file_field_name).await
    }

    /// Upload a single file using POST (for artifact uploads)
    pub async fn upload_file<'a>(&'a self, operation_id: &'a str, endpoint: &'a str, file_path: &'a Path, file_field_name: &'a str) -> Result<Value> {
        // Empty form data - only a single file is uploaded
        let form_data = HashMap::new();

        self.request_multipart(operation_id, Method::POST, endpoint, form_data, vec![file_path], file_field_name).await
    }
}
