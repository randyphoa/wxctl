use super::resource::RawResource;
use crate::interpolation::{EnvReader, ProcessEnv, interpolate};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize)]
pub struct Config {
    pub resources: Vec<RawResource>,
}

impl Config {
    /// Parse a multi-document YAML string (documents separated by `---`)
    /// into a Config with a list of resources. Expands `${env:VAR}` against
    /// process env before deserialising into `RawResource` so every
    /// downstream stage sees resolved values.
    pub fn from_yaml(content: &str) -> Result<Self, anyhow::Error> {
        Self::from_yaml_with_env(content, &ProcessEnv)
    }

    /// Same as `from_yaml` but with an injected env reader; used by tests
    /// to avoid touching the real process environment.
    pub fn from_yaml_with_env(content: &str, env: &dyn EnvReader) -> Result<Self, anyhow::Error> {
        let mut resources = Vec::new();
        for document in serde_norway::Deserializer::from_str(content) {
            let mut value = serde_norway::Value::deserialize(document)?;
            // Skip empty/null documents. A stray `---` separator — common in
            // LLM-generated multi-doc YAML and when concatenating document streams
            // (e.g. appending a generated test suite) — deserializes to Null and
            // would otherwise fail RawResource parsing with a misleading
            // "missing field `kind`". A non-empty doc that lacks `kind` still errors.
            if value.is_null() {
                continue;
            }
            interpolate(&mut value, env)?;
            let resource: RawResource = serde_norway::from_value(value)?;
            resources.push(resource);
        }
        Ok(Config { resources })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Profile {
    /// Top-level deployment default (raw string, parsed by ClientFactory).
    /// Per-service override via `ServiceConfig.deployment`.
    /// Format: `"saas"` or `"software-X.Y.Z"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<String>,
    #[serde(flatten)]
    pub services: HashMap<String, ServiceConfig>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ServiceConfig {
    /// Base URL. Optional so profiles can include auxiliary service entries
    /// (`cos`, `db2`, etc.) that don't have a URL-based API — wxctl only
    /// demands a URL at `create_client` time for the services it actually uses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// When unset, defaulted from `Deployment` in `ClientFactory::create_client`:
    /// `Saas → "apikey"`, `Software → "cp4d"`. Existing profiles that set this
    /// explicitly continue to win.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apikey: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Service instance identifier (CRN). When set, sent as `AuthInstanceId` on every
    /// request — required by watsonx.data lakehouse APIs.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<String>,
    /// Per-service deployment override; falls back to `Profile.deployment`.
    /// Same format as `Profile.deployment`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment: Option<String>,
    /// Leading URL-path segment prepended to every request after host (e.g. `/zen-data-api`).
    /// Phase 1: profile-only. Phase 2 adds build-time-derived defaults.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// HMAC access key (IBM COS SigV4 mode). Ignored by other services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_key: Option<String>,
    /// HMAC secret key (IBM COS SigV4 mode). Ignored by other services.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_key: Option<String>,
    /// IBM COS service-instance CRN. Populated into `ibm-service-instance-id`
    /// on bucket CREATE and ListBuckets. When omitted with apikey auth, the
    /// handler auto-discovers via the Resource Controller API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cos_instance_crn: Option<String>,
    /// Bucket-name prefix used by live tests to namespace ephemeral buckets
    /// (e.g. `wxctl-test`). Optional; only read by the SDK test harness.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_name_prefix: Option<String>,
    /// Explicit regional S3 endpoint (e.g. `s3.eu-gb.cloud-object-storage.appdomain.cloud`).
    /// When omitted wxctl derives it from the bucket's `region` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// HashiCorp Vault namespace (HCP/Enterprise multi-tenancy). When set, sent as
    /// the `X-Vault-Namespace` header on every request to this service. Non-secret.
    /// OSS Vault has no namespaces — omit it there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

/// Manual `Debug`: masks credential fields so a Debug-formatted profile can
/// never leak plaintext secrets into logs or error chains. `Serialize` stays
/// derived — profile writing needs the real values.
impl std::fmt::Debug for ServiceConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        fn mask(v: &Option<String>) -> Option<&'static str> {
            v.as_ref().map(|_| "***REDACTED***")
        }
        f.debug_struct("ServiceConfig")
            .field("url", &self.url)
            .field("auth_type", &self.auth_type)
            .field("apikey", &mask(&self.apikey))
            .field("username", &self.username)
            .field("password", &mask(&self.password))
            .field("instance_id", &self.instance_id)
            .field("deployment", &self.deployment)
            .field("path_prefix", &self.path_prefix)
            .field("namespace", &self.namespace)
            .field("access_key", &mask(&self.access_key))
            .field("secret_key", &mask(&self.secret_key))
            .field("cos_instance_crn", &self.cos_instance_crn)
            .field("bucket_name_prefix", &self.bucket_name_prefix)
            .field("endpoint", &self.endpoint)
            .finish()
    }
}

#[cfg(test)]
mod from_yaml_tests {
    use super::*;
    use crate::interpolation::EnvReader;

    struct NoEnv;
    impl EnvReader for NoEnv {
        fn get(&self, _key: &str) -> Option<String> {
            None
        }
    }

    #[test]
    fn skips_empty_documents_from_stray_separators() {
        // Leading, trailing, and interior stray `---` (as LLM output + a `\n---\n`
        // join produce) must not break parsing with "missing field `kind`".
        let yaml = "---\nkind: test\nref_name: a\n---\n\n---\nkind: test\nref_name: b\n---\n";
        let cfg = Config::from_yaml_with_env(yaml, &NoEnv).expect("stray separators should be skipped");
        assert_eq!(cfg.resources.len(), 2);
        assert_eq!(cfg.resources[0].kind, "test");
        assert_eq!(cfg.resources[1].kind, "test");
    }

    #[test]
    fn non_empty_doc_missing_kind_still_errors() {
        // A real mapping without `kind` is a genuine error — not masked by the skip.
        let yaml = "ref_name: a\nname: a\n";
        assert!(Config::from_yaml_with_env(yaml, &NoEnv).is_err());
    }
}

#[cfg(test)]
mod debug_masking_tests {
    use super::*;

    #[test]
    fn profile_debug_masks_credentials_but_serialize_keeps_them() {
        let json = r#"{"deployment":"saas","watsonx_ai":{"url":"https://h.example","apikey":"SEEDED-APIKEY","username":"alice","password":"SEEDED-PASSWORD","access_key":"SEEDED-ACCESS","secret_key":"SEEDED-SECRET"}}"#;
        let profile: Profile = serde_json::from_str(json).unwrap();
        // Debug (incl. Profile's derived Debug over the services map) leaks no secret.
        let dbg = format!("{profile:?}");
        for secret in ["SEEDED-APIKEY", "SEEDED-PASSWORD", "SEEDED-ACCESS", "SEEDED-SECRET"] {
            assert!(!dbg.contains(secret), "Debug leaked {secret}: {dbg}");
        }
        assert!(dbg.contains("***REDACTED***"), "masked marker present: {dbg}");
        assert!(dbg.contains("https://h.example"), "non-secret fields still rendered: {dbg}");
        // Serialize is untouched — profile writing round-trips real values.
        let ser = serde_json::to_string(&profile).unwrap();
        assert!(ser.contains("SEEDED-APIKEY"), "Serialize must keep real values: {ser}");
    }
}
