use crate::context::RuntimeIdStore;
use wxctl_core::ValidatedResource;

/// Check if all dependencies of a resource exist in the runtime store.
/// Returns a list of missing dependency keys, or empty vec if all present.
pub(crate) fn check_dependencies(resource: &ValidatedResource, runtime_store: &RuntimeIdStore) -> Vec<wxctl_core::ResourceKey> {
    resource.dependencies.iter().filter(|dep_key| !runtime_store.contains(dep_key)).cloned().collect()
}
