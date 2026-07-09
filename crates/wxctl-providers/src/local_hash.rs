//! Local run-hash record store — the Q2 local-hash idempotency fallback for
//! non-discoverable job kinds (`identity_hash.storage: local`, i.e. `sal_*`).
//!
//! wxctl stays a stateless reconciler for every other kind; this file is the
//! single documented exception. A kind's handler records the desired
//! `identity_hash` here after a successful run; the reconciler's
//! Skip-discovery arm consults it on the next apply (recorded → the run
//! already happened → NoChange; absent → Create). Records accumulate (one
//! array entry per distinct hash — retained history), are env-scoped by a
//! hash of the client base_url (no profile names or secrets), and live in a
//! FILE in the run-records root (`runs_root()/local-hashes.json`) — safe from
//! `prune_runs`, which removes only run *directories*; `WXCTL_RUNS_DIR`
//! relocates it. Fresh machine / cleared file ⇒ one re-run, then idempotent.
//! Writes are best-effort tmp+rename (last-writer-wins; a lost record costs
//! at worst one spurious re-run — today's behavior).

use anyhow::Result;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

const FILE_NAME: &str = "local-hashes.json";

/// Environment key: truncated BLAKE3 of the service base URL. Same config
/// against two environments keeps independent records.
pub fn env_key(base_url: &str) -> String {
    blake3::hash(base_url.trim_end_matches('/').as_bytes()).to_hex().to_string().chars().take(12).collect()
}

fn store_path(root: &Path) -> PathBuf {
    root.join(FILE_NAME)
}

fn load(root: &Path) -> Value {
    std::fs::read_to_string(store_path(root)).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| json!({"version": 1, "environments": {}}))
}

/// True when `<kind>/<ref_name>` under `env` has already recorded `hash`.
/// Plain map-key lookups (not a JSON Pointer): the write path stores the raw
/// `{kind}/{ref_name}` map key, so a pointer read would need full `~0`/`~1`
/// escaping — a ref_name containing `/` or `~` would record but never match.
pub fn has_run_hash_at(root: &Path, env: &str, kind: &str, ref_name: &str, hash: &str) -> bool {
    load(root).get("environments").and_then(|e| e.get(env)).and_then(|e| e.get(format!("{kind}/{ref_name}"))).and_then(|v| v.as_array()).is_some_and(|a| a.iter().any(|h| h.as_str() == Some(hash)))
}

/// Append `hash` to `<kind>/<ref_name>` under `env` (idempotent; accumulate).
/// Atomic via tmp+rename. Callers treat errors as best-effort (warn, never
/// fail the apply).
pub fn record_run_hash_at(root: &Path, env: &str, kind: &str, ref_name: &str, hash: &str) -> Result<()> {
    std::fs::create_dir_all(root)?;
    let mut doc = load(root);
    let entry = doc
        .pointer_mut("/environments")
        .and_then(|e| e.as_object_mut())
        .map(|envs| envs.entry(env.to_string()).or_insert_with(|| json!({})))
        .and_then(|e| e.as_object_mut())
        .map(|kinds| kinds.entry(format!("{kind}/{ref_name}")).or_insert_with(|| json!([])))
        .and_then(|v| v.as_array_mut())
        .ok_or_else(|| anyhow::anyhow!("local-hash store shape corrupted"))?;
    if !entry.iter().any(|h| h.as_str() == Some(hash)) {
        entry.push(Value::String(hash.to_string()));
    }
    let tmp = store_path(root).with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_string_pretty(&doc)?)?;
    std::fs::rename(&tmp, store_path(root))?;
    Ok(())
}

/// `has_run_hash_at` against the live run-records root.
pub fn has_run_hash(env: &str, kind: &str, ref_name: &str, hash: &str) -> bool {
    has_run_hash_at(&wxctl_core::logging::run_record::runs_root(), env, kind, ref_name, hash)
}

/// `record_run_hash_at` against the live run-records root.
pub fn record_run_hash(env: &str, kind: &str, ref_name: &str, hash: &str) -> Result<()> {
    record_run_hash_at(&wxctl_core::logging::run_record::runs_root(), env, kind, ref_name, hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_accumulate_and_scoping() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        // Missing store → false (fresh-machine degradation: re-run once).
        assert!(!has_run_hash_at(root, "envA", "sal_glossary", "glossary", "aaaa1111"));
        // Record → present; idempotent re-record keeps one entry.
        record_run_hash_at(root, "envA", "sal_glossary", "glossary", "aaaa1111").unwrap();
        record_run_hash_at(root, "envA", "sal_glossary", "glossary", "aaaa1111").unwrap();
        assert!(has_run_hash_at(root, "envA", "sal_glossary", "glossary", "aaaa1111"));
        // Accumulate: a second hash coexists (retained history) — both match.
        record_run_hash_at(root, "envA", "sal_glossary", "glossary", "bbbb2222").unwrap();
        assert!(has_run_hash_at(root, "envA", "sal_glossary", "glossary", "aaaa1111"));
        assert!(has_run_hash_at(root, "envA", "sal_glossary", "glossary", "bbbb2222"));
        // Env-scoped: envB sees nothing; kind/ref scoped too.
        assert!(!has_run_hash_at(root, "envB", "sal_glossary", "glossary", "aaaa1111"));
        assert!(!has_run_hash_at(root, "envA", "sal_enrichment_job", "glossary", "aaaa1111"));
        assert!(!has_run_hash_at(root, "envA", "sal_glossary", "other", "aaaa1111"));
        // A ref_name containing '/' round-trips (map-key read matches the raw write key).
        record_run_hash_at(root, "envA", "sal_glossary", "team/glossary", "cccc3333").unwrap();
        assert!(has_run_hash_at(root, "envA", "sal_glossary", "team/glossary", "cccc3333"));
        // env_key is deterministic and slash-insensitive.
        assert_eq!(env_key("https://x/"), env_key("https://x"));
        assert_eq!(env_key("https://x").len(), 12);
    }
}
