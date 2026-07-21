pub mod client;
pub mod concurrency;
pub mod diagnose;
pub mod interpolation;
pub mod logging;
pub mod paths;
pub mod registry;
pub mod response_id;
pub mod traits;
pub mod types;

/// Serializes tests that mutate process-global env vars (e.g. `WXCTL_RUNS_DIR`,
/// `WXCTL_RUNS_KEEP`, `WXCTL_TROUBLESHOOT_DIR`, `WXCTL_CONCURRENCY_GLOBAL`).
/// Env vars are process-global while cargo runs unit tests on parallel threads —
/// any test that calls `set_var`/`remove_var` must hold this guard for its duration.
#[cfg(test)]
pub(crate) fn test_env_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub use client::{ClientFactory, HttpClient, extract_nested, load_color_preference, lookup_nested, wxctl_config_dir};
pub use concurrency::{CapacityManager, ConcurrencyConfig};
pub use response_id::{first_string_field, resource_id, resource_id_field};

// Re-export from wxctl-graph for backward compatibility
pub use wxctl_graph::{CycleError, DependencyEdge, IndexGraph, ParsedReference, Resource, ResourceSet, ResourceSetBuilder, extract_references, extract_references_with_path, parse_reference, parse_reference_with_path};

pub use diagnose::{DiagnosisBundle, RunArtifact, RunSummary, TriageClass, build_bundle, find_latest_failed, list_runs, load_artifact, match_troubleshoot};
pub use registry::filters::filter_request_fields;
pub use registry::{ResourceDescriptor, ResourceRegistry};
pub use traits::{AdvisorySink, NoOpAdvisorySink, Reconciler, ResourceHandler, StateComparison};
pub use types::{Config, IStr, OnDestroyPolicy, Profile, RawResource, RemoteResource, ResourceKey, ValidatedResource, error_chain_vec, istr};
