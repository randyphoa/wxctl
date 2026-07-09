use serde::ser::{Serialize, SerializeStruct, Serializer};
use wxctl_graph::ResourceKey;

/// A validation error paired with the resource that caused it.
#[derive(Debug)]
pub struct AnnotatedValidationError {
    /// Resource identifier (e.g. "tool/my_tool"), empty for global errors.
    pub resource: String,
    pub error: ValidationError,
}

impl std::fmt::Display for AnnotatedValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.resource.is_empty() { write!(f, "{}", self.error) } else { write!(f, "[{}] {}", self.resource, self.error) }
    }
}

impl Serialize for AnnotatedValidationError {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut state = serializer.serialize_struct("AnnotatedValidationError", 5)?;
        state.serialize_field("resource", &self.resource)?;
        state.serialize_field("field", self.error.field())?;
        state.serialize_field("code", self.error.code())?;
        state.serialize_field("message", &self.error.to_string())?;
        state.serialize_field("suggestion", &self.error.suggestion())?;
        state.end()
    }
}

/// Validation errors that can occur during resource validation.
///
/// Uses `String` consistently for all string fields for simplicity.
/// Errors are relatively rare, so the allocation cost is negligible.
#[derive(Debug)]
pub enum ValidationError {
    MissingField {
        field: String,
    },
    ComputedFieldSet {
        field: String,
    },
    TypeMismatch {
        expected: &'static str,
        got: String,
    },
    DuplicateName {
        kind: String,
        name: String,
    },
    CircularDependency {
        path: Vec<ResourceKey>,
    },
    UnknownResourceType {
        kind: String,
    },
    InvalidFieldValue {
        field: String,
        message: String,
    },
    /// Invalid dependency reference in a field.
    InvalidDependency {
        /// JSON field path where the invalid reference was found.
        field_path: String,
        /// The kind of resource being referenced.
        ref_kind: String,
        /// The name of resource being referenced.
        ref_name: String,
        /// Allowed dependency kinds from schema.
        allowed_kinds: Vec<String>,
    },
    UnknownField {
        field: String,
    },
    /// Free-form error string, used for R006 deployment-constraint violations.
    Other(String),
}

impl ValidationError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::MissingField { .. } => "MISSING_FIELD",
            Self::ComputedFieldSet { .. } => "INVALID_FIELD_VALUE",
            Self::TypeMismatch { .. } => "INVALID_FIELD_VALUE",
            Self::DuplicateName { .. } => "DUPLICATE_NAME",
            Self::CircularDependency { .. } => "CIRCULAR_DEPENDENCY",
            Self::UnknownResourceType { .. } => "UNKNOWN_RESOURCE_KIND",
            Self::InvalidFieldValue { .. } => "INVALID_FIELD_VALUE",
            Self::InvalidDependency { .. } => "UNRESOLVED_REFERENCE",
            Self::UnknownField { .. } => "UNKNOWN_FIELD",
            Self::Other(_) => "DEPLOYMENT_CONSTRAINT",
        }
    }

    pub fn field(&self) -> &str {
        match self {
            Self::MissingField { field } => field,
            Self::ComputedFieldSet { field } => field,
            Self::TypeMismatch { .. } => "",
            Self::DuplicateName { .. } => "name",
            Self::CircularDependency { .. } => "",
            Self::UnknownResourceType { .. } => "kind",
            Self::InvalidFieldValue { field, .. } => field,
            Self::InvalidDependency { field_path, .. } => field_path,
            Self::UnknownField { field } => field,
            Self::Other(_) => "metadata.requires.deployment",
        }
    }

    pub fn suggestion(&self) -> String {
        match self {
            Self::MissingField { field } => format!("Add the required '{}' field", field),
            Self::ComputedFieldSet { field } => {
                format!("Remove '{}' — it is computed automatically", field)
            }
            Self::TypeMismatch { expected, .. } => {
                format!("Change the value to type {}", expected)
            }
            Self::DuplicateName { kind, name } => {
                format!("Rename one of the {} resources named '{}'", kind, name)
            }
            Self::CircularDependency { .. } => "Remove or restructure the circular reference chain".to_string(),
            Self::UnknownResourceType { kind } => {
                format!("Check that '{}' is a valid resource kind", kind)
            }
            Self::InvalidFieldValue { field, .. } => {
                format!("Check that '{}' has a valid value", field)
            }
            Self::InvalidDependency { ref_kind, ref_name, allowed_kinds, .. } => {
                format!("{} '{}' not found. Allowed kinds: {}", ref_kind, ref_name, allowed_kinds.join(", "))
            }
            Self::UnknownField { field } => format!("Remove unknown field '{}'", field),
            Self::Other(_) => "Adjust metadata.requires.deployment or switch profiles".to_string(),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::MissingField { field } => {
                write!(f, "Missing required field: {}", field)
            }
            ValidationError::ComputedFieldSet { field } => {
                write!(f, "Computed field cannot be set: {}", field)
            }
            ValidationError::TypeMismatch { expected, got } => {
                write!(f, "Type mismatch: expected {}, got {}", expected, got)
            }
            ValidationError::DuplicateName { kind, name } => {
                write!(f, "Duplicate resource: {} \"{}\"", kind, name)
            }
            ValidationError::CircularDependency { path } => {
                let cycle: Vec<_> = path.iter().map(|k| format!("{}.{}", k.kind, k.name)).collect();
                write!(f, "Circular dependency: {}", cycle.join(" -> "))
            }
            ValidationError::UnknownResourceType { kind } => {
                write!(f, "Unknown resource type: {}", kind)
            }
            ValidationError::InvalidFieldValue { field, message } => {
                write!(f, "Invalid value for field '{}': {}", field, message)
            }
            ValidationError::InvalidDependency { field_path, ref_kind, ref_name, allowed_kinds } => {
                write!(f, "Invalid dependency at '{}': {} \"{}\" not allowed (allowed: {})", field_path, ref_kind, ref_name, allowed_kinds.join(", "))
            }
            ValidationError::UnknownField { field } => {
                write!(f, "Unknown field: '{}'", field)
            }
            ValidationError::Other(msg) => write!(f, "{}", msg),
        }
    }
}
