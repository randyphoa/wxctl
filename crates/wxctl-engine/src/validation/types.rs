use wxctl_core::{DependencyEdge, IndexGraph, ResourceKey, ResourceSet, ValidatedResource};

// `AnnotatedValidationError` / `ValidationError` now live in the wasm-safe
// `wxctl-schema` crate (single source shared with the remote MCP server). Re-exported
// here so `super::types::*` and `wxctl_engine::validation::types::*` resolve unchanged.
pub use wxctl_schema::validation::{AnnotatedValidationError, ValidationError};

/// Result of validation pipeline.
///
/// Uses an enum to make the valid/invalid state explicit at the type level.
pub enum ValidationResult {
    /// Validation succeeded with the validated resources.
    Valid(ResourceSet<ValidatedResource>),
    /// Validation failed with errors.
    Invalid(Vec<AnnotatedValidationError>),
}

impl ValidationResult {
    /// Create a successful validation result with resources.
    pub fn success(resource_set: ResourceSet<ValidatedResource>) -> Self {
        Self::Valid(resource_set)
    }

    /// Create a failed validation result with annotated errors.
    pub fn failure(errors: Vec<AnnotatedValidationError>) -> Self {
        Self::Invalid(errors)
    }

    /// Check if validation succeeded.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        matches!(self, Self::Valid(_))
    }

    /// Get validation errors (empty if valid).
    #[must_use]
    pub fn errors(&self) -> &[AnnotatedValidationError] {
        match self {
            Self::Valid(_) => &[],
            Self::Invalid(errors) => errors,
        }
    }

    /// Get the resources as a slice (for read-only access).
    #[must_use]
    pub fn resources(&self) -> &[ValidatedResource] {
        match self {
            Self::Valid(rs) => rs.as_slice(),
            Self::Invalid(_) => &[],
        }
    }

    /// Take ownership of the ResourceSet (returns None if invalid).
    #[must_use]
    pub fn take_resource_set(self) -> Option<ResourceSet<ValidatedResource>> {
        match self {
            Self::Valid(rs) => Some(rs),
            Self::Invalid(_) => None,
        }
    }

    /// Consume and return all parts (resources, graph, edges).
    /// Returns None if validation failed.
    pub fn into_parts(self) -> Option<(Vec<ValidatedResource>, IndexGraph<ResourceKey>, Vec<DependencyEdge>)> {
        match self {
            Self::Valid(rs) => Some(rs.into_parts()),
            Self::Invalid(_) => None,
        }
    }
}
