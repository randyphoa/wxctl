use serde::Serialize;
use wxctl_core::{DependencyEdge, IndexGraph, ResourceKey, ResourceSet, ValidatedResource};

// `AnnotatedValidationError` / `ValidationError` now live in the wasm-safe
// `wxctl-schema` crate (single source shared with the remote MCP server). Re-exported
// here so `super::types::*` and `wxctl_engine::validation::types::*` resolve unchanged.
pub use wxctl_schema::validation::{AnnotatedValidationError, ValidationError};

/// A warn-level, non-blocking validation advisory (the Phase 3 V505 bridge advisory).
/// Advisories ride alongside a `ValidationResult` but never change `valid` or the exit
/// code. Serializes to `{ code, resource, message, suggestion }`.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
pub struct ValidationAdvisory {
    /// Namespaced advisory code (e.g. `WXCTL-V505`).
    pub code: String,
    /// `"<kind>/<ref_name>"` the advisory anchors to.
    pub resource: String,
    /// Human-readable description of the advisory.
    pub message: String,
    /// Suggested follow-up action.
    pub suggestion: String,
}

/// Result of validation pipeline.
///
/// Uses an enum to make the valid/invalid state explicit at the type level. Both
/// variants carry a (usually empty) advisories list; advisories are warn-level and
/// never affect `is_valid()`.
pub enum ValidationResult {
    /// Validation succeeded with the validated resources.
    Valid { resources: ResourceSet<ValidatedResource>, advisories: Vec<ValidationAdvisory> },
    /// Validation failed with errors.
    Invalid { errors: Vec<AnnotatedValidationError>, advisories: Vec<ValidationAdvisory> },
}

impl ValidationResult {
    /// Create a successful validation result with resources (no advisories).
    pub fn success(resource_set: ResourceSet<ValidatedResource>) -> Self {
        Self::Valid { resources: resource_set, advisories: Vec::new() }
    }

    /// Create a failed validation result with annotated errors (no advisories).
    pub fn failure(errors: Vec<AnnotatedValidationError>) -> Self {
        Self::Invalid { errors, advisories: Vec::new() }
    }

    /// Attach advisories, preserving validity (builder-style). No producer calls this
    /// yet; the Phase 3 bridge-advisory scan will.
    #[must_use]
    pub fn with_advisories(mut self, advisories: Vec<ValidationAdvisory>) -> Self {
        match &mut self {
            Self::Valid { advisories: a, .. } | Self::Invalid { advisories: a, .. } => *a = advisories,
        }
        self
    }

    /// Check if validation succeeded.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid { .. })
    }

    /// Get validation errors (empty if valid).
    #[must_use]
    pub fn errors(&self) -> &[AnnotatedValidationError] {
        match self {
            Self::Valid { .. } => &[],
            Self::Invalid { errors, .. } => errors,
        }
    }

    /// Get the advisories (empty on both variants until the Phase 3 producer runs).
    #[must_use]
    pub fn advisories(&self) -> &[ValidationAdvisory] {
        match self {
            Self::Valid { advisories, .. } | Self::Invalid { advisories, .. } => advisories,
        }
    }

    /// Get the resources as a slice (for read-only access).
    #[must_use]
    pub fn resources(&self) -> &[ValidatedResource] {
        match self {
            Self::Valid { resources, .. } => resources.as_slice(),
            Self::Invalid { .. } => &[],
        }
    }

    /// Take ownership of the ResourceSet (returns None if invalid).
    #[must_use]
    pub fn take_resource_set(self) -> Option<ResourceSet<ValidatedResource>> {
        match self {
            Self::Valid { resources, .. } => Some(resources),
            Self::Invalid { .. } => None,
        }
    }

    /// Consume and return all parts (resources, graph, edges).
    /// Returns None if validation failed.
    pub fn into_parts(self) -> Option<(Vec<ValidatedResource>, IndexGraph<ResourceKey>, Vec<DependencyEdge>)> {
        match self {
            Self::Valid { resources, .. } => Some(resources.into_parts()),
            Self::Invalid { .. } => None,
        }
    }
}
