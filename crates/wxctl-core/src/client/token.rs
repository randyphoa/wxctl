use anyhow::{Context, Result};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use reqwest::Client;
use serde::Deserialize;
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tokio::sync::Mutex;

/// Token response from IAM authentication
#[derive(Debug, Deserialize)]
struct TokenResponse {
    pub access_token: String,
    #[serde(default = "default_expires_in")]
    pub expires_in: u64,
}

fn default_expires_in() -> u64 {
    3600 // 1 hour default
}

/// Cached authentication token with expiration
#[derive(Debug, Clone)]
struct CachedToken {
    pub token: String,
    pub expires_at: SystemTime,
}

impl CachedToken {
    fn new(token: String, expires_in: u64) -> Self {
        let expires_at = SystemTime::now() + Duration::from_secs(expires_in);
        Self { token, expires_at }
    }

    fn is_valid(&self) -> bool {
        if let Ok(duration) = self.expires_at.duration_since(SystemTime::now()) {
            duration.as_secs() > 60 // 60 second buffer
        } else {
            false
        }
    }
}

/// Token manager for handling authentication
pub struct TokenManager {
    pub(crate) auth_token: String,
    auth_type: String,
    base_url: Option<String>,
    cached_token: Arc<Mutex<Option<CachedToken>>>,
    refresh_lock: Arc<Mutex<()>>,
}

impl TokenManager {
    pub fn new(auth_token: String, auth_type: String) -> Self {
        Self { auth_token, auth_type, base_url: None, cached_token: Arc::new(Mutex::new(None)), refresh_lock: Arc::new(Mutex::new(())) }
    }

    pub fn with_base_url(auth_token: String, auth_type: String, base_url: String) -> Self {
        Self { auth_token, auth_type, base_url: Some(base_url), cached_token: Arc::new(Mutex::new(None)), refresh_lock: Arc::new(Mutex::new(())) }
    }

    /// Get a valid token, refreshing if necessary
    pub async fn get_token<'a>(&'a self, client: &'a Client) -> Result<String> {
        // Fast path: check if we have a valid cached token
        {
            let cached = self.cached_token.lock().await;
            if let Some(ref token) = *cached
                && token.is_valid()
            {
                return Ok(token.token.clone());
            }
        }

        // Token is expired or missing, need to refresh
        let _lock = self.refresh_lock.lock().await;

        // Double-check after acquiring lock (another thread may have refreshed)
        {
            let cached = self.cached_token.lock().await;
            if let Some(ref token) = *cached
                && token.is_valid()
            {
                return Ok(token.token.clone());
            }
        }

        // Still need to refresh
        self.refresh_token(client).await
    }

    async fn refresh_token<'a>(&'a self, client: &'a Client) -> Result<String> {
        match self.auth_type.as_str() {
            "apikey" => {
                // Exchange API key for IAM access token
                let token = self.authenticate_with_iam_apikey(client, &self.auth_token).await?;
                Ok(token)
            }
            "cp4d" | "icp4d" => {
                // Exchange username/password for CP4D token
                let token = self.authenticate_with_cp4d(client, &self.auth_token).await?;
                Ok(token)
            }
            "basic" => {
                // For basic auth, use the token directly (it's already base64 encoded username:password)
                let cached = CachedToken::new(self.auth_token.clone(), 86400);
                *self.cached_token.lock().await = Some(cached);
                Ok(self.auth_token.clone())
            }
            "zenapikey" => {
                let encoded = BASE64_STANDARD.encode(&self.auth_token);
                let cached = CachedToken::new(encoded.clone(), 365 * 24 * 3600);
                *self.cached_token.lock().await = Some(cached);
                Ok(encoded)
            }
            _ => {
                // Bearer token - use directly
                let cached = CachedToken::new(self.auth_token.clone(), 86400);
                *self.cached_token.lock().await = Some(cached);
                Ok(self.auth_token.clone())
            }
        }
    }

    async fn authenticate_with_iam_apikey<'a>(&'a self, client: &'a Client, apikey: &'a str) -> Result<String> {
        let iam_url = "https://iam.cloud.ibm.com/identity/token";

        // Parallel test startup can stampede IAM and 5xx transiently. Bounded retry with
        // exponential backoff + jitter, re-checking the cached token between attempts so
        // a sibling caller that already refreshed short-circuits the re-POST.
        let max_attempts = 3u32;
        let mut last_err: Option<anyhow::Error> = None;
        for attempt in 0..max_attempts {
            if let Some(ref token) = *self.cached_token.lock().await
                && token.is_valid()
            {
                return Ok(token.token.clone());
            }

            let result = client.post(iam_url).form(&[("grant_type", "urn:ibm:params:oauth:grant-type:apikey"), ("apikey", apikey)]).send().await;

            let response = match result {
                Ok(r) => r,
                Err(e) => {
                    last_err = Some(anyhow::Error::new(e).context("Failed to request IAM token"));
                    if attempt + 1 < max_attempts {
                        tokio::time::sleep(super::retry::calculate_backoff(attempt)).await;
                        continue;
                    }
                    break;
                }
            };

            let status = response.status();
            if status.is_success() {
                let token_response: TokenResponse = response.json().await.context("Failed to parse IAM token response")?;
                let cached = CachedToken::new(token_response.access_token.clone(), token_response.expires_in);
                *self.cached_token.lock().await = Some(cached);
                return Ok(token_response.access_token);
            }

            let error_body = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            let err = anyhow::anyhow!("IAM authentication failed with status {}: {}", status, error_body);

            // Retry transient server errors; bail immediately on client errors (401/400 etc).
            let retryable = status.is_server_error() || status.as_u16() == 429;
            if retryable && attempt + 1 < max_attempts {
                last_err = Some(err);
                tokio::time::sleep(super::retry::calculate_backoff(attempt)).await;
                continue;
            }
            return Err(err);
        }
        Err(last_err.unwrap_or_else(|| anyhow::anyhow!("IAM authentication failed: exhausted {} attempts", max_attempts)))
    }

    async fn authenticate_with_cp4d<'a>(&'a self, client: &'a Client, credentials: &'a str) -> Result<String> {
        // CP4D requires base_url to construct the authorize endpoint
        let base_url = self.base_url.as_ref().ok_or_else(|| anyhow::anyhow!("base_url required for CP4D authentication"))?;

        // Parse username:password from credentials
        let parts: Vec<&str> = credentials.split(':').collect();
        if parts.len() != 2 {
            return Err(anyhow::anyhow!("Invalid credentials format for CP4D auth (expected username:password)"));
        }
        let (username, password) = (parts[0], parts[1]);

        // Build authorize endpoint URL
        let authorize_url = format!("{}/icp4d-api/v1/authorize", base_url);

        // Build JSON request body
        let body = serde_json::json!({
            "username": username,
            "password": password
        });

        let response = client.post(&authorize_url).header("Content-Type", "application/json").json(&body).send().await.context("Failed to request CP4D token")?;

        if !response.status().is_success() {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_else(|_| "Unknown error".to_string());
            return Err(anyhow::anyhow!("CP4D authentication failed with status {}: {}", status, error_body));
        }

        // Parse response to extract token
        #[derive(Deserialize)]
        struct Cp4dTokenResponse {
            token: String,
        }

        let token_response: Cp4dTokenResponse = response.json().await.context("Failed to parse CP4D token response")?;

        // Cache the token (CP4D tokens typically expire after 12 hours, using conservative 10 hour TTL)
        let cached = CachedToken::new(token_response.token.clone(), 36000);
        *self.cached_token.lock().await = Some(cached);

        Ok(token_response.token)
    }
}

#[cfg(test)]
mod zenapikey_token_tests {
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
    use reqwest::Client;

    // refresh_token is private; tests exercise it via the public get_token entry point,
    // which falls through to refresh_token on a cold cache.

    #[tokio::test]
    async fn zenapikey_get_token_returns_base64_and_caches_it() {
        // Cold cache returns base64(username:apikey); the second call returns the
        // same cached value.
        let mgr = TokenManager::new("alice:KEY-123".to_string(), "zenapikey".to_string());
        let client = Client::new();
        let first = mgr.get_token(&client).await.expect("zenapikey get_token should succeed on cold cache");
        assert_eq!(first, BASE64_STANDARD.encode("alice:KEY-123"));
        let second = mgr.get_token(&client).await.expect("second call");
        assert_eq!(first, second, "second call returns cached value");
    }

    #[tokio::test]
    async fn zenapikey_makes_no_outbound_http_calls() {
        // Point at a non-routable port; if the zenapikey arm issued an HTTP request, the short timeout would error the call.
        let mgr = TokenManager::with_base_url("alice:KEY-123".to_string(), "zenapikey".to_string(), "http://127.0.0.1:1".to_string());
        let client = Client::builder().timeout(std::time::Duration::from_millis(200)).build().expect("client");
        let token = mgr.get_token(&client).await.expect("no HTTP call expected");
        assert_eq!(token, BASE64_STANDARD.encode("alice:KEY-123"));
    }
}
