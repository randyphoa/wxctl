use anyhow::{Result, anyhow};
use reqwest::{Method, StatusCode};
use std::time::Duration;

/// HTTP error with status code for retry decisions
#[derive(Debug)]
pub(crate) struct HttpError {
    pub(crate) status: Option<StatusCode>,
    pub(crate) message: String,
    pub(crate) method: Option<Method>,
}

impl HttpError {
    pub(crate) fn with_status(status: StatusCode, method: Method, message: String) -> Self {
        Self { status: Some(status), message, method: Some(method) }
    }

    pub(crate) fn without_status(message: String) -> Self {
        Self { status: None, message, method: None }
    }

    fn is_retryable(&self) -> bool {
        status_method_is_retryable(self.status, self.method.as_ref())
    }
}

/// Core retryability predicate shared by `HttpError::is_retryable` and the
/// final-failure gate in `http.rs`.  Keeping a single copy here ensures the
/// error-event emission condition and the retry-loop condition can never drift.
///
/// Returns `true` when a response with `status`/`method` would be retried by
/// `with_retry`; `false` when the failure is non-retryable and therefore final
/// at the very first attempt.
pub(crate) fn status_method_is_retryable(status: Option<StatusCode>, method: Option<&Method>) -> bool {
    match status {
        Some(status) => {
            // 429 (rate limit) is always retryable regardless of method
            if status.as_u16() == 429 {
                return true;
            }
            // 5xx errors are only retryable for idempotent methods.
            // POST is not idempotent — the server may have completed the
            // operation before returning the error (observed with the
            // Watson Orchestrate tools API returning 500 after successful
            // creation). Retrying a POST risks duplicate resources.
            let is_server_error = matches!(status.as_u16(), 500 | 502 | 503 | 504);
            let is_idempotent = method.map(|m| matches!(*m, Method::GET | Method::PUT | Method::DELETE | Method::HEAD | Method::OPTIONS)).unwrap_or(false);
            is_server_error && is_idempotent
        }
        // No status = connect/TLS/DNS failure before the server saw the request.
        // Safe to retry on any method: nothing reached the remote. Bounded by
        // `max_retries` so a persistently-down endpoint still fails fast.
        None => true,
    }
}

/// Execute an async operation with retry logic and exponential backoff
///
/// Calls `f(attempt)` up to `max_retries` times. On retryable errors (429, 5xx),
/// sleeps with exponential backoff before the next attempt. Non-retryable errors
/// and the final retry failure are returned immediately.
pub(crate) async fn with_retry<T: Send, Fut: std::future::Future<Output = Result<T, HttpError>> + Send>(max_retries: u32, mut f: impl FnMut(u32) -> Fut + Send) -> Result<T> {
    for attempt in 0..max_retries {
        match f(attempt).await {
            Ok(value) => return Ok(value),
            Err(http_err) => {
                if http_err.is_retryable() && attempt < max_retries - 1 {
                    let delay = calculate_backoff(attempt);
                    tokio::time::sleep(delay).await;
                    continue;
                }
                return Err(anyhow!(http_err.message));
            }
        }
    }
    Err(anyhow!("Max retries exceeded"))
}

/// Exponential backoff with ±12.5% jitter. Attempts 0→~1s, 1→~2s, 2→~4s.
pub(crate) fn calculate_backoff(attempt: u32) -> Duration {
    let base_ms = 1000u64;
    let exponential = base_ms * 2_u64.pow(attempt);
    let jitter = rand::random::<f64>() * 0.25;
    let with_jitter = exponential as f64 * (1.0 + jitter - 0.125);
    Duration::from_millis(with_jitter as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::{Method, StatusCode};

    #[test]
    fn is_retryable_threads_status_and_method_into_predicate() {
        // HttpError::is_retryable() only wires self.status/self.method into the shared
        // predicate (branch coverage lives in the shared_predicate_* tests below). 400/GET
        // isn't covered there, so this both proves the wiring and exercises a 4xx value.
        let err = HttpError::with_status(StatusCode::BAD_REQUEST, Method::GET, "bad request".into());
        assert!(!err.is_retryable());
    }

    // -- status_method_is_retryable (shared predicate) tests --
    // These cover the cases most relevant to the final-failure gate in http.rs:
    // 4xx config/env errors are never retryable (final at attempt 0), so the
    // ERROR event with the exchange must fire even when attempt < max_retries - 1.
    #[test]
    fn shared_predicate_retryability_matrix() {
        let cases = [
            // (status, method, retryable?, why)
            (Some(StatusCode::UNAUTHORIZED), Some(&Method::GET), false, "401 auth config error, final at attempt 0"),
            (Some(StatusCode::NOT_FOUND), Some(&Method::GET), false, "404 wrong endpoint/resource, final at attempt 0"),
            (Some(StatusCode::TOO_MANY_REQUESTS), Some(&Method::POST), true, "429 rate limit, retryable regardless of method"),
            (Some(StatusCode::INTERNAL_SERVER_ERROR), Some(&Method::GET), true, "idempotent GET + 5xx = retryable"),
            (Some(StatusCode::INTERNAL_SERVER_ERROR), Some(&Method::POST), false, "non-idempotent POST + 5xx = not retryable (dup-create risk)"),
            (None, None, true, "no status = DNS/TLS/connect failure, nothing reached server, safe to retry"),
        ];
        for (status, method, expected, why) in cases {
            assert_eq!(status_method_is_retryable(status, method), expected, "{status:?}/{method:?}: {why}");
        }
    }
}
