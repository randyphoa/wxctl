use anyhow::Result;
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec, error_matches};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

/// watsonx.data ingestion jobs use the identity-hash model (spec:
/// job-identity-input-hash). The schema's `identity_hash` block folds the
/// ingestion inputs (+ optional `generation` nonce) into the job id as
/// `<id>-<hash8>` at validation time via the generic `storage: name_suffix`
/// step (`discovery.name_field: id`). Discovery (`get_by_id`) then finds a prior
/// run by that suffixed id → NoChange; a changed input or bumped `generation`
/// yields a new id → 404 → Create (a new, retained run). `recover_from_create_error`
/// below stays as the server-side idempotency backstop for the uniqueness-registry
/// edge case where GET 404s but POST 500s "already exists". No per-handler
/// identity code is needed — the id-suffix and match are generic.
pub struct IngestionJobHandler;

const BASE_PATH: &str = "/v3/lhingestion/api/v1/ingestion/jobs";
// Live-observed terminal success value from the watsonx.data ingestion API is
// `finished` (confirmed via a real job whose engine_logs end with "Ingestion
// completed successfully."). `completed`/`succeeded`/`success` are kept as
// defensive aliases in case other API versions use different spellings.
const DONE_STATES: &[&str] = &["finished", "completed", "succeeded", "success"];
const FAILED_STATES: &[&str] = &["failed", "error"];

fn matches_status(status: &str, candidates: &[&str]) -> bool {
    candidates.iter().any(|s| s.eq_ignore_ascii_case(status))
}

impl ResourceHandler for IngestionJobHandler {
    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            // `id` is client-supplied (schema `required: true`) and lives on the
            // resolved request body (`resource`), not the raw create response —
            // the create API's response body only ever echoes `job_id`, never
            // `id`, so checking `response` here always missed and silently
            // skipped polling. Read from `resource` first (with a `job_id`
            // fallback on `response` for safety) so post_create actually polls
            // the job to a terminal state instead of reporting Create done as
            // soon as the POST returns.
            let job_id = resource.get("id").and_then(|v| v.as_str()).or_else(|| response.get("job_id").and_then(|v| v.as_str()));
            let Some(job_id) = job_id else {
                tracing::debug!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    resource_type = "ingestion_job",
                    reason = "missing_id_in_create_response",
                    "skipping polling"
                );
                return Ok(());
            };
            wait_for_job_terminal(client, job_id, operation_id).await
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, client: &'a HttpClient, operation_id: &'a str, is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        // If the previous apply was interrupted after the POST but before the
        // post_create poller reached a terminal state, rediscovery on re-apply
        // sees a matching job via list_and_get and reconciles to NoOp — the user
        // would see apply return while the job is still running server-side.
        // On the apply path only, block on wait_for_job_terminal so re-apply
        // tails the already-running job to completion. The plan path returns
        // immediately so `wxctl plan` stays non-blocking.
        Box::pin(async move {
            if !is_apply {
                return Ok(());
            }
            let Some(job_id) = remote_data.get("id").and_then(|v| v.as_str()) else {
                return Ok(());
            };
            let status = remote_data.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if matches_status(status, DONE_STATES) || matches_status(status, FAILED_STATES) {
                return Ok(());
            }
            wait_for_job_terminal(client, job_id, operation_id).await
        })
    }

    // The watsonx.data ingestion service tracks job IDs in a uniqueness registry
    // that outlives GET visibility: a completed job may return 404 from
    // `GET /v3/.../jobs/{id}` while `POST` still rejects the same id with
    // `500 "Ingestion job id already exists"`. Reconciliation can't detect this
    // from discovery, so on that specific conflict, synthesize a Handled
    // response rather than fail the whole apply.
    fn recover_from_create_error<'a>(&'a self, resource: &'a Value, error: &'a anyhow::Error, _client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        Box::pin(async move {
            if !error_matches(error, 500, &["already exists"]) {
                return Ok(None);
            }
            let Some(job_id) = resource.get("id").and_then(|v| v.as_str()) else {
                return Ok(None);
            };
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "ingestion_job",
                job_id = %job_id,
                "adopt: server reports id already exists; treating as idempotent create"
            );
            // The wire response shape only needs the id fields the engine reads
            // downstream (extract_resource_id looks for `id`, handler looks for
            // `job_id`). No real remote to fetch — GET returns 404.
            Ok(Some(json!({"id": job_id, "job_id": job_id})))
        })
    }

    fn pre_delete<'a>(&'a self, _resource: &'a Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        // The watsonx.data ingestion API has no DELETE endpoint, so destroy is a
        // client-side no-op.
        Box::pin(async move {
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "ingestion_job",
                reason = "no_delete_endpoint",
                "destroy is a client-side no-op"
            );
            Ok(HookOutcome::Handled(json!({})))
        })
    }
}

async fn wait_for_job_terminal(client: &HttpClient, job_id: &str, operation_id: &str) -> Result<()> {
    let max_attempts = 120;

    crate::util::poll_until(max_attempts, Duration::from_secs(10), crate::util::PollTimeout::Bail(format!("[{operation_id}] timed out waiting for ingestion_job {job_id} to reach a terminal state")), None::<String>, |attempt, mut prev_status| async move {
        let spec = RequestSpec::new(Method::GET, format!("{BASE_PATH}/{job_id}")).body(BodyKind::None);
        let resp: Value = client.execute(operation_id, spec).await?;
        let status = resp.get("status").and_then(|v| v.as_str()).unwrap_or("unknown");

        if prev_status.as_deref() != Some(status) {
            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                resource_type = "ingestion_job",
                job_id = %job_id,
                status = %status,
                attempt = attempt,
                max_attempts = max_attempts,
                "ingestion_job status observed"
            );
            prev_status = Some(status.to_string());
        }

        let outcome = if matches_status(status, DONE_STATES) {
            crate::util::PollOutcome::Done(Value::Null)
        } else if matches_status(status, FAILED_STATES) {
            let reason = wxctl_core::logging::extract_api_error_message(&resp);
            let trace = wxctl_core::logging::extract_trace_id(&resp).map(|t| format!(" [trace_id={t}]")).unwrap_or_default();
            crate::util::PollOutcome::Failed(format!("[{operation_id}] ingestion_job {job_id} failed (status={status}): {reason}{trace}"))
        } else {
            crate::util::PollOutcome::Pending
        };
        Ok((outcome, prev_status))
    })
    .await
    .map(|_| ())
}
