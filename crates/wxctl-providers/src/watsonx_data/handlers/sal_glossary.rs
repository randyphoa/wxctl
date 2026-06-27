//! `sal_glossary` handler — multipart-uploads a business-glossary CSV into
//! watsonx.data SAL and polls the upload process to a terminal state.
//!
//! Create is **multipart** (`glossary_csv` file + optional `replace_option`
//! text field), which the schema-based reconciler's default JSON create won't
//! do — so it runs in `pre_create` and returns `HookOutcome::Handled` (exactly
//! like `business_terms::handle_import`). The flow:
//!   1. `POST /v3/sal_integration/glossary/upload_processes` (multipart) → `201 {id}`,
//!   2. poll `GET …/upload_processes/{id}/status` → `{response: "<json-string>"}`,
//!      reading the inner document's top-level `status`
//!      (`SUCCEEDED`/`COMPLETED`/`FAILED`/`TIMEOUT`, captured live on CP4D).
//!
//! The `response` field is itself a *serialized JSON* document — `{process_id,
//! status, step_number, total_steps, step_message, messages.resources[…]}` —
//! whose `messages` array carries unbounded import warnings (e.g. "Header name X
//! is not valid", "Row skipped"). So we read the inner `status` field rather than
//! substring-scanning the whole blob, which a warning would otherwise trip into a
//! false FAILED. A non-JSON `response` falls back to scanning the raw string.
//!
//! ⚠ `request_multipart` builds `base_url + path` and does NOT apply the
//! `/lakehouse/api` path_prefix — on Software the multipart endpoint must
//! include it (`client.path_prefix()`). The status poll goes through
//! `client.execute`, which DOES add the prefix, so it uses the bare `/v3/...`.
//!
//! Poll exhaustion returns Ok (best-effort — does not fail the apply); only a
//! confirmed failed marker bails. There is no DELETE endpoint — destroy is
//! carried by the resource's `on_destroy: retain`.

use anyhow::{Result, anyhow, bail};
use reqwest::Method;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct SalGlossaryHandler;

const STATUS_PATH: &str = "/v3/sal_integration/glossary/upload_processes/{id}/status";
const DONE_MARKERS: &[&str] = &["completed", "finished", "succeeded", "success"];
const FAILED_MARKERS: &[&str] = &["failed", "failure", "error", "cancel"];

impl ResourceHandler for SalGlossaryHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let csv = resource.get("glossary_csv").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("sal_glossary requires glossary_csv"))?;
            let path = Path::new(csv);
            if !path.exists() {
                bail!("glossary CSV not found: {csv} (path should be relative to the config file or absolute)");
            }

            // `replace_option` → extra multipart text field; omit when unset (do not
            // guess a default into the form).
            let mut form_data: HashMap<String, Value> = HashMap::new();
            if let Some(opt) = resource.get("replace_option").and_then(|v| v.as_str()) {
                form_data.insert("replace_option".to_string(), json!(opt));
            }

            // ⚠ request_multipart = base_url + path (no path_prefix). On Software the
            // path MUST include `/lakehouse/api`, so prepend client.path_prefix().
            let endpoint = format!("{}/v3/sal_integration/glossary/upload_processes", client.path_prefix());

            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_glossary", glossary_csv = %csv, replace_option = ?form_data.get("replace_option"), "uploading business-glossary CSV to SAL (multipart)");

            let created: Value = client.request_multipart(operation_id, Method::POST, &endpoint, form_data, vec![path], "glossary_csv").await?;

            let id = created.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("glossary upload returned no id: {created}"))?.to_string();

            wait_for_upload_terminal(client, &id, operation_id).await?;

            Ok(HookOutcome::Handled(created))
        })
    }
}

#[derive(Debug, PartialEq, Eq)]
enum UploadState {
    Done,
    Failed(String),
    Pending,
}

/// Poll the upload-process status endpoint until a terminal marker appears.
/// Poll exhaustion returns Ok (best-effort, like `sal_enrichment_job`); a
/// confirmed failed marker bails.
async fn wait_for_upload_terminal(client: &HttpClient, id: &str, operation_id: &str) -> Result<()> {
    let max_attempts = 60;

    let result = crate::util::poll_until(max_attempts, Duration::from_secs(10), crate::util::PollTimeout::BestEffort, (), |attempt, _state| async move {
        let outcome = match check_upload_state(client, id, operation_id).await {
            Ok(UploadState::Done) => {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_glossary", upload_id = %id, attempt = attempt, "glossary upload reached a terminal (completed) state");
                crate::util::PollOutcome::Done(Value::Bool(true))
            }
            Ok(UploadState::Failed(detail)) => crate::util::PollOutcome::Failed(format!("[{operation_id}] sal_glossary upload {id} reached a failed state: {detail}")),
            Ok(UploadState::Pending) => crate::util::PollOutcome::Pending,
            Err(e) => {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_glossary", upload_id = %id, error = %e, "glossary upload status poll errored; will retry");
                crate::util::PollOutcome::Pending
            }
        };
        Ok((outcome, ()))
    })
    .await?;

    if result.is_null() {
        // Opaque/eventual API: never positively saw a terminal marker (BestEffort
        // exhaustion returns Null; a terminal Done returns Bool(true)). Best-effort —
        // log and return rather than fail the apply.
        tracing::warn!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "sal_glossary", upload_id = %id, "glossary upload poll exhausted without a terminal marker; returning (best-effort)");
    }
    Ok(())
}

/// Read the upload-process status (path-prefixed via `client.execute`).
async fn check_upload_state(client: &HttpClient, id: &str, operation_id: &str) -> Result<UploadState> {
    let spec = RequestSpec::new(Method::GET, STATUS_PATH).path_var("id", id).body(BodyKind::None);
    let resp: Value = client.execute(operation_id, spec).await?;
    Ok(upload_status(&resp))
}

/// Pure: collapse an upload-process status response into a terminal state. The
/// `response` field is a serialized JSON document (`{ …, status, messages.resources[…] }`,
/// live shape on CP4D) — read its top-level `status` (`SUCCEEDED`/`COMPLETED`/`FAILED`/
/// `TIMEOUT`) rather than scanning the whole blob, whose unbounded `messages` warnings
/// ("…is not valid", "Row skipped") would otherwise false-trip a FAILED marker. A
/// non-JSON `response` falls back to scanning the raw string. A failed marker wins, else
/// a done marker, else still pending (`TIMEOUT`/in-progress ⇒ keep polling).
fn upload_status(status_body: &Value) -> UploadState {
    let response = status_body.get("response").and_then(|v| v.as_str()).unwrap_or_default();
    // When `response` is the live JSON document, trust ONLY its top-level `status`
    // — a missing `status` means in-progress (Pending), NOT a reason to scan the
    // blob, whose `messages` warnings ("…is not valid", "Row skipped", an
    // in-progress "0 errors") would false-trip a FAILED marker. The raw-string scan
    // is reserved for a genuinely non-JSON opaque `response`.
    let signal = match serde_json::from_str::<Value>(response) {
        Ok(doc) => doc.get("status").and_then(|s| s.as_str()).unwrap_or_default().to_ascii_lowercase(),
        Err(_) => response.to_ascii_lowercase(),
    };
    if FAILED_MARKERS.iter().any(|m| signal.contains(m)) {
        return UploadState::Failed(signal);
    }
    if DONE_MARKERS.iter().any(|m| signal.contains(m)) {
        return UploadState::Done;
    }
    UploadState::Pending
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn upload_status_detects_terminal() {
        // Real shape captured live on CP4D (2026-06-05): `response` is a serialized
        // JSON document whose `status` is the signal; `messages` carry import warnings.
        // The SUCCEEDED payload below intentionally includes a warning containing
        // "is not valid" — reading the inner `status` (not a whole-blob scan) keeps it Done.
        let succeeded = json!({"response": r#"{"process_id":"9171030f-9dbc-4640-81ef-af77357eb78f","status":"SUCCEEDED","step_number":13,"total_steps":13,"step_message":"Artifacts import finished","messages":{"resources":[{"code":"GIM00013E","message":"GIM00013E: Header name Category is not valid for artifact type glossary_term. Column is skipped."},{"code":"GIM00062E","message":"GIM00062E: Row skipped: The relations should have artifact id value Employee."}]}}"#});
        assert_eq!(upload_status(&succeeded), UploadState::Done);

        // COMPLETED is also terminal-done; FAILED bails; TIMEOUT / in-progress keep polling.
        assert_eq!(upload_status(&json!({"response": r#"{"status":"COMPLETED"}"#})), UploadState::Done);
        assert_eq!(upload_status(&json!({"response": r#"{"status":"FAILED","step_message":"import failed"}"#})), UploadState::Failed("failed".to_string()));
        assert_eq!(upload_status(&json!({"response": r#"{"status":"TIMEOUT"}"#})), UploadState::Pending);
        assert_eq!(upload_status(&json!({"response": r#"{"status":"RUNNING","step_number":3,"total_steps":13}"#})), UploadState::Pending);

        // Fallback: a non-JSON opaque `response` is scanned raw.
        assert_eq!(upload_status(&json!({"response": "upload finished"})), UploadState::Done);
        assert_eq!(upload_status(&json!({"response": "fatal error during import"})), UploadState::Failed("fatal error during import".to_string()));

        // Missing / empty / non-string / no-status → still pending, not a false-terminal.
        assert_eq!(upload_status(&json!({"response": ""})), UploadState::Pending);
        assert_eq!(upload_status(&json!({"response": null})), UploadState::Pending);
        assert_eq!(upload_status(&json!({})), UploadState::Pending);
        assert_eq!(upload_status(&json!({"response": "{\"step_message\":\"working\"}"})), UploadState::Pending);
        // A JSON in-progress doc with NO top-level `status` whose other fields carry an
        // "error" substring must stay Pending — only the inner `status` is the signal,
        // the blob is never scanned when `response` parses as JSON.
        assert_eq!(upload_status(&json!({"response": r#"{"step_number":3,"step_message":"Importing rows, 0 errors so far"}"#})), UploadState::Pending);
    }
}
