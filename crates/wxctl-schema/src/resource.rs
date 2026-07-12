//! Wasm-safe resource value types shared by the offline validator and the
//! reconciliation engine. `RawResource` is the parsed user YAML; `ValidatedResource`
//! is the post-validation record carrying its descriptor + extracted dependencies.
//! `RemoteResource` (a reconciliation concern) stays in `wxctl-core`.

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wxctl_graph::ResourceKey;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RawResource {
    pub kind: String,
    #[serde(flatten)]
    pub data: Value,
}

/// Per-resource teardown policy. Mirrors CloudFormation `DeletionPolicy`,
/// Pulumi `retainOnDelete`, and Helm `resource-policy: keep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OnDestroyPolicy {
    #[default]
    Delete,
    Retain,
}

#[derive(Debug, Clone)]
pub struct ValidatedResource {
    pub key: ResourceKey,
    pub data: Value,
    pub descriptor: std::sync::Arc<crate::descriptor::ResourceDescriptor>,
    /// Actual dependencies extracted from ${kind.name} references.
    /// Computed once during validation and reused in planning/execution.
    pub dependencies: Vec<ResourceKey>,
    /// Teardown policy: `Delete` (default) runs the normal destroy path;
    /// `Retain` short-circuits to a structural no-op during destroy.
    pub on_destroy: OnDestroyPolicy,
}

impl wxctl_graph::Resource for ValidatedResource {
    fn key(&self) -> &ResourceKey {
        &self.key
    }

    fn data(&self) -> &Value {
        &self.data
    }

    fn dependencies(&self) -> &[ResourceKey] {
        &self.dependencies
    }
}

impl RawResource {
    /// Extract `metadata.requires.deployment` as a parsed `DeploymentConstraintList`.
    /// Returns `Ok(None)` when the resource has no `metadata.requires.deployment`
    /// pin, `Err(_)` when the pin is present but unparseable.
    pub fn required_deployment(&self) -> Result<Option<crate::deployment::DeploymentConstraintList>, anyhow::Error> {
        let Some(metadata) = self.data.get("metadata") else {
            return Ok(None);
        };
        let Some(requires) = metadata.get("requires") else {
            return Ok(None);
        };
        let Some(deployment) = requires.get("deployment") else {
            return Ok(None);
        };
        let parsed: crate::deployment::DeploymentConstraintList = serde_json::from_value(deployment.clone()).context("metadata.requires.deployment failed to parse")?;
        Ok(Some(parsed))
    }

    /// Optional `ref_name` accessor used for error messages. Returns `"<unnamed>"`
    /// when the resource has no `ref_name`.
    pub fn ref_name(&self) -> &str {
        self.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("<unnamed>")
    }

    /// Parse and remove the top-level `depends_on` meta-field from `data`.
    ///
    /// `depends_on` is a YAML list of bare `kind.ref_name` strings declaring
    /// ordering-only prerequisites (no value is resolved or injected). This
    /// helper extracts the entries into `Vec<ResourceKey>` and **removes the
    /// key from `data`** so it never reaches an API request body.
    ///
    /// Shape errors (returned as `Err`) — caller maps these to a validation error:
    /// - `depends_on` present but not a YAML list.
    /// - An entry that is not a string.
    /// - An entry in `${...}` template form (use a field `${ref}` for value flow).
    /// - An entry that is not exactly two non-empty `kind.ref_name` segments.
    ///
    /// Returns `Ok(vec![])` when `depends_on` is absent (and leaves `data`
    /// untouched). Existence-of-target and self-dependency checks are the
    /// caller's responsibility — this helper only validates entry shape.
    pub fn take_depends_on(&mut self) -> Result<Vec<ResourceKey>, anyhow::Error> {
        let ref_name = self.data.get("ref_name").and_then(|v| v.as_str()).unwrap_or("<unnamed>").to_string();
        let Some(obj) = self.data.as_object_mut() else {
            return Ok(Vec::new());
        };
        let Some(raw) = obj.remove("depends_on") else {
            return Ok(Vec::new());
        };
        let Value::Array(items) = raw else {
            anyhow::bail!("resource '{}:{}': depends_on must be a list of 'kind.ref_name' strings", self.kind, ref_name);
        };
        let mut keys = Vec::with_capacity(items.len());
        for item in items {
            let Value::String(entry) = item else {
                anyhow::bail!("resource '{}:{}': each depends_on entry must be a 'kind.ref_name' string, got {}", self.kind, ref_name, item);
            };
            if entry.contains("${") {
                anyhow::bail!("resource '{}:{}': depends_on entry '{}' must be a bare 'kind.ref_name', not a ${{...}} template — use a field reference when a value flows", self.kind, ref_name, entry);
            }
            let mut parts = entry.splitn(2, '.');
            let kind = parts.next().unwrap_or("");
            let name = parts.next().unwrap_or("");
            if kind.is_empty() || name.is_empty() || name.contains('.') {
                anyhow::bail!("resource '{}:{}': depends_on entry '{}' is not a well-formed 'kind.ref_name'", self.kind, ref_name, entry);
            }
            keys.push(ResourceKey::new(kind, name));
        }
        Ok(keys)
    }
}

#[cfg(test)]
mod depends_on_tests {
    use super::*;
    use serde_json::json;

    fn raw(data: serde_json::Value) -> RawResource {
        RawResource { kind: "tool".into(), data }
    }

    #[test]
    fn absent_returns_empty_and_leaves_data() {
        let mut r = raw(json!({"ref_name": "b", "app_id": "x"}));
        let keys = r.take_depends_on().unwrap();
        assert!(keys.is_empty());
        assert_eq!(r.data.get("app_id"), Some(&json!("x")));
    }

    #[test]
    fn parses_entries_and_strips_key() {
        let mut r = raw(json!({"ref_name": "b", "depends_on": ["catalog.a", "space.s"]}));
        let keys = r.take_depends_on().unwrap();
        assert_eq!(keys, vec![ResourceKey::new("catalog", "a"), ResourceKey::new("space", "s")]);
        assert!(r.data.get("depends_on").is_none(), "depends_on must be stripped from data");
    }

    #[test]
    fn rejects_malformed_depends_on() {
        // Each row: (depends_on value, why it's invalid). `take_depends_on` must Err.
        let cases: &[(serde_json::Value, &str)] =
            &[(json!("catalog.a"), "not a list"), (json!([42]), "non-string entry"), (json!(["${catalog.a}"]), "template form (must be bare kind.name)"), (json!(["catalog"]), "missing segments (needs kind.name)"), (json!(["catalog.a.field"]), "too many segments (three)")];
        for (val, why) in cases {
            let mut r = raw(json!({ "ref_name": "b", "depends_on": val }));
            assert!(r.take_depends_on().is_err(), "depends_on={val:?} must be rejected: {why}");
        }
    }
}
