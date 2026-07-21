use std::sync::Arc;
use wxctl_core::traits::ResourceHandler;

#[macro_use]
mod macros;
pub(crate) mod util;

mod cloud_object_storage;
mod common_core;
mod concert;
mod concert_workflows;
mod factsheets;
mod instana;
pub mod local_hash;
mod openscale;
mod pa_workspace;
mod planning_analytics;
mod vault;
mod watsonx_ai;
mod watsonx_data; // was: data
mod watsonx_orchestrate; // was: orchestrate

/// Re-export of the compile-time dependency graph, now owned by `wxctl-schema`.
pub use wxctl_schema::dependency_graph;
pub use wxctl_schema::dependency_graph::PATH_FIELDS;

/// Resolve relative local file paths in config resources against `config_dir`.
///
/// Path fields are schema-declared (`is_path: true`) and surfaced via the
/// build-generated [`PATH_FIELDS`] table `(kind, field_name, parent_array_field)`
/// — adding a path field needs only `is_path: true` on the schema, never an edit
/// here. Handles scalar fields, object-array items, and bare-string array items
/// (e.g. `documents: ["file.pdf"]`). Absolute paths pass through unchanged.
///
/// The single home for both the CLI (`wxctl -f <file>`) and the local MCP server.
/// Callers register `config_dir` as an allowed path root
/// ([`wxctl_core::paths::allow_path_root`]) themselves — this function only
/// rewrites the values.
pub fn resolve_file_paths(config: &mut wxctl_core::Config, config_dir: &std::path::Path) {
    for resource in &mut config.resources {
        for &(kind, field_name, parent_array) in PATH_FIELDS {
            if resource.kind != kind {
                continue;
            }
            match parent_array {
                None => {
                    if let Some(val) = resource.data.get_mut(field_name) {
                        resolve_path_value(val, config_dir);
                    }
                }
                Some(arr_field) => {
                    if let Some(items) = resource.data.get_mut(arr_field).and_then(|v| v.as_array_mut()) {
                        for item in items {
                            match item.as_object_mut() {
                                // Object array item (e.g. `documents: [{path: ...}]`).
                                Some(obj) => {
                                    if let Some(val) = obj.get_mut(field_name) {
                                        resolve_path_value(val, config_dir);
                                    }
                                }
                                // Bare-string array item (e.g. `documents: ["file.pdf"]`).
                                None => resolve_path_value(item, config_dir),
                            }
                        }
                    }
                }
            }
        }
    }
}

/// Resolve a single path value relative to `config_dir` when it is a relative
/// path string; leave absolute paths and non-strings untouched.
fn resolve_path_value(value: &mut serde_json::Value, config_dir: &std::path::Path) {
    if let Some(path_str) = value.as_str() {
        let p = std::path::Path::new(path_str);
        if p.is_relative() {
            *value = serde_json::Value::String(config_dir.join(p).to_string_lossy().into_owned());
        }
    }
}

/// Identity-hash helpers for job-style kinds, consumed by wxctl-engine's generic
/// identity-hash validate/discover seam (spec: job-identity-input-hash).
pub use util::{IDENTITY_ENV_KEY, extract_identity_env_marker, extract_run_hash, identity_hash, job_run_state_rank, set_identity_env_marker, set_run_hash_tag, strip_identity_env_marker};

pub mod handlers {
    pub use super::cloud_object_storage::handlers as cloud_object_storage;
    pub use super::common_core::handlers as common_core;
    pub use super::concert::handlers as concert;
    pub use super::concert_workflows::handlers as concert_workflows;
    pub use super::factsheets::handlers as factsheets;
    pub use super::instana::handlers as instana;
    pub use super::openscale::handlers as openscale;
    pub use super::pa_workspace::handlers as pa_workspace;
    pub use super::planning_analytics::handlers as planning_analytics;
    pub use super::vault::handlers as vault;
    pub use super::watsonx_ai::handlers as watsonx_ai;
    pub use super::watsonx_data::handlers as watsonx_data; // was: data
    pub use super::watsonx_orchestrate::handlers as watsonx_orchestrate; // was: orchestrate
}

pub use watsonx_orchestrate::openapi::expand_openapi_resources;

/// Test-only helpers shared across the crate's unit-test modules.
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::{Mutex, MutexGuard};

    /// One process-wide lock guarding the global current directory. Any test that
    /// mutates `set_current_dir` (e.g. the python artifact tests) or reads it via a
    /// CWD-relative containment check (e.g. `validate_path` in the MCP artifact tests)
    /// must hold it, or those tests race on the shared process CWD across modules
    /// (cargo runs them on threads in one process). Poison is ignored.
    static CWD_LOCK: Mutex<()> = Mutex::new(());

    pub(crate) fn lock_cwd() -> MutexGuard<'static, ()> {
        CWD_LOCK.lock().unwrap_or_else(|p| p.into_inner())
    }
}

pub fn get_handler(resource_name: &str) -> Option<Arc<dyn ResourceHandler>> {
    common_core::get_handler(resource_name)
        .or_else(|| watsonx_data::get_handler(resource_name))
        .or_else(|| watsonx_orchestrate::get_handler(resource_name))
        .or_else(|| watsonx_ai::get_handler(resource_name))
        .or_else(|| cloud_object_storage::get_handler(resource_name))
        .or_else(|| factsheets::get_handler(resource_name))
        .or_else(|| openscale::get_handler(resource_name))
        .or_else(|| concert::get_handler(resource_name))
        .or_else(|| concert_workflows::get_handler(resource_name))
        .or_else(|| instana::get_handler(resource_name))
        .or_else(|| planning_analytics::get_handler(resource_name))
        .or_else(|| pa_workspace::get_handler(resource_name))
        .or_else(|| vault::get_handler(resource_name))
}

/// Per-kind custom [`Reconciler`]s (discovery + compare) for kinds the generic
/// schema-driven reconciler can't express (e.g. `asset_promotion`, whose
/// identity is a type-scoped CAMS name search in the target space and whose
/// cached state must carry the project-side `source_asset_id`). Registration
/// sites fall back to the engine's `SchemaBasedReconciler` when this returns
/// `None` — which is every other kind.
pub fn get_reconciler(resource_name: &str) -> Option<Arc<dyn wxctl_core::traits::Reconciler>> {
    common_core::get_reconciler(resource_name)
}

#[cfg(test)]
mod tests {
    use wxctl_core::registry::ResourceDescriptor;

    #[test]
    fn test_all_schemas_parse_into_descriptors() {
        let schemas: Vec<&'static wxctl_schema::ir::SchemaIr> = wxctl_schema::ir::RESOURCE_IR.values().copied().collect();

        assert!(!schemas.is_empty(), "Expected at least one schema to parse, got none");

        for schema in &schemas {
            let descriptor = ResourceDescriptor::from_ir(schema);

            assert!(!descriptor.name.is_empty(), "Schema has empty name");
            assert!(!descriptor.service.is_empty(), "Schema '{}' has empty service", descriptor.name);
            assert!(!descriptor.kind.is_empty(), "Schema '{}' has empty kind", descriptor.name);
            assert!(!descriptor.id_field.is_empty(), "Schema '{}' has empty id_field", descriptor.name);
            assert!(!descriptor.endpoints.get.is_empty(), "Schema '{}' has empty get endpoint", descriptor.name);
            assert!(!descriptor.endpoints.create.is_empty(), "Schema '{}' has empty create endpoint", descriptor.name);
            assert!(!descriptor.endpoints.delete.is_empty(), "Schema '{}' has empty delete endpoint", descriptor.name);
        }

        // Duplicate-kind detection moved to build time: `RESOURCE_IR` is a `phf::Map`
        // keyed by kind, so key uniqueness is structural, not something this test can
        // observe going wrong. Kept as a light sanity echo of the old assertion's intent.
        let mut kinds: Vec<_> = schemas.iter().map(|s| s.resource.kind).collect();
        kinds.sort_unstable();
        let len = kinds.len();
        kinds.dedup();
        assert_eq!(kinds.len(), len, "Duplicate kind values found across schemas");
    }

    /// AC6: the secret in `concert_credential.credentials[].value` must be redacted at
    /// emission. Redaction is driven by the schema-derived sensitive paths; assert the
    /// loaded `concert_credential` schema yields the dotted path `credentials.value` — the
    /// exact input the materializer feeds to `redact_by_schema` (whose masking is
    /// unit-tested generically in `wxctl-core/src/logging/redaction.rs`). The full
    /// live-emission check (grep of run records / WXCTL_LOG_PATH) is the Phase-4 live E2E.
    #[test]
    fn concert_credential_marks_credentials_value_sensitive() {
        let cred = wxctl_schema::ir::RESOURCE_IR.get("concert_credential").copied().expect("concert_credential in RESOURCE_IR");
        let paths = cred.resource.schema.sensitive_paths();
        assert!(paths.iter().any(|p| p == "credentials.value"), "expected 'credentials.value' in concert_credential sensitive_paths, got {:?}", paths);
    }

    /// The `pa_user.password` field must be marked sensitive so its emitted body value is
    /// redacted (AC8). Assert the loaded schema yields the dotted path `password` in
    /// `sensitive_paths` — the same schema-level guard as `concert_credential`. The full
    /// live-emission grep (run records / WXCTL_LOG_PATH) is the Phase-4 live E2E.
    #[test]
    fn pa_user_marks_password_sensitive() {
        let user = wxctl_schema::ir::RESOURCE_IR.get("pa_user").copied().expect("pa_user in RESOURCE_IR");
        let paths = user.resource.schema.sensitive_paths();
        assert!(paths.iter().any(|p| p == "password"), "expected 'password' in pa_user sensitive_paths, got {:?}", paths);
    }
}
