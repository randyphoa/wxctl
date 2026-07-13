use serde::ser::{Serialize, SerializeStruct, Serializer};
use wxctl_graph::ResourceKey;

use super::error_codes;

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
        /// Target kind when the missing field declares a `references:` block,
        /// letting the suggestion name the `${<kind>.<ref_name>}` shape. `None`
        /// for ordinary required fields.
        reference_kind: Option<String>,
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
    /// A `${kind.name}` template whose kind IS a known resource kind but whose
    /// referent is absent from the config. Provably unresolvable at execution
    /// time (the resolver reads only ids the config's own DAG produced), so it
    /// is a hard error. Distinct from `InvalidDependency` (kind disallowed by
    /// the field's schema).
    UnresolvedReference {
        /// JSON field path where the dangling reference was found.
        field_path: String,
        /// The kind of resource being referenced.
        ref_kind: String,
        /// The name of resource being referenced.
        ref_name: String,
        /// Transitively-required kinds that adding `ref_kind` would also pull in,
        /// each as `(missing_kind, required_by_kind, field)`. Empty when the
        /// referent's required closure is already satisfied. Populated at
        /// extraction time via `dependency_graph::missing_required_closure`.
        required_chain: Vec<(String, String, String)>,
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
            Self::UnresolvedReference { .. } => "UNRESOLVED_REFERENCE",
            Self::UnknownField { .. } => "UNKNOWN_FIELD",
            Self::Other(_) => "DEPLOYMENT_CONSTRAINT",
        }
    }

    pub fn field(&self) -> &str {
        match self {
            Self::MissingField { field, .. } => field,
            Self::ComputedFieldSet { field } => field,
            Self::TypeMismatch { .. } => "",
            Self::DuplicateName { .. } => "name",
            Self::CircularDependency { .. } => "",
            Self::UnknownResourceType { .. } => "kind",
            Self::InvalidFieldValue { field, .. } => field,
            Self::InvalidDependency { field_path, .. } => field_path,
            Self::UnresolvedReference { field_path, .. } => field_path,
            Self::UnknownField { field } => field,
            Self::Other(_) => "metadata.requires.deployment",
        }
    }

    pub fn suggestion(&self) -> String {
        match self {
            Self::MissingField { field, reference_kind } => match reference_kind {
                Some(kind) => format!("Add the required '{}' field, referencing a '{}' resource as `${{{}.<ref_name>}}`", field, kind, kind),
                None => format!("Add the required '{}' field", field),
            },
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
            Self::UnresolvedReference { ref_kind, ref_name, required_chain, .. } => {
                let mut s = format!("Add a `{}` resource with `ref_name: {}` to this config, or replace the reference with a literal value if the resource is managed outside it.", ref_kind, ref_name);
                if !required_chain.is_empty() {
                    let parts: Vec<String> = required_chain.iter().map(|(kind, req_by, field)| if field.is_empty() { format!("`{}`", kind) } else { format!("`{}` (referenced by `{}.{}`)", kind, req_by, field) }).collect();
                    s.push_str(&format!(" Adding a `{}` also requires: {}.", ref_kind, parts.join(", ")));
                }
                s
            }
            Self::UnknownField { field } => format!("Remove unknown field '{}'", field),
            Self::Other(_) => "Adjust metadata.requires.deployment or switch profiles".to_string(),
        }
    }
}

impl std::fmt::Display for ValidationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ValidationError::MissingField { field, .. } => {
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
            ValidationError::UnresolvedReference { field_path, ref_kind, ref_name, .. } => {
                write!(f, "[{}] Unresolved reference at '{}': no '{}' resource named '{}' is defined in this config", error_codes::V005, field_path, ref_kind, ref_name)
            }
            ValidationError::UnknownField { field } => {
                write!(f, "Unknown field: '{}'", field)
            }
            ValidationError::Other(msg) => write!(f, "{}", msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_field_with_reference_kind_suggests_ref_shape() {
        // AC4: a required field carrying a `references:` declaration names the target kind
        // and the ${<kind>.<ref_name>} shape.
        let e = ValidationError::MissingField { field: "llm".to_string(), reference_kind: Some("model".to_string()) };
        let s = e.suggestion();
        assert!(s.contains("model"), "suggestion must name the target kind: {s}");
        assert!(s.contains("${model.<ref_name>}"), "suggestion must show the ${{kind.ref_name}} shape: {s}");
        // A plain required field (no reference_kind) keeps the simple suggestion.
        let plain = ValidationError::MissingField { field: "name".to_string(), reference_kind: None };
        assert!(!plain.suggestion().contains("${"), "plain field suggestion must not show a ref shape");
    }

    #[test]
    fn unresolved_reference_suggestion_renders_chain() {
        // AC5: the dual-exit suggestion appends the transitive chain with (kind, req_by, field).
        let e = ValidationError::UnresolvedReference {
            field_path: "model".to_string(),
            ref_kind: "wml_model".to_string(),
            ref_name: "absent".to_string(),
            required_chain: vec![("autoai_experiment".to_string(), "wml_model".to_string(), "experiment".to_string()), ("data_asset".to_string(), "autoai_experiment".to_string(), "training_data".to_string())],
        };
        let s = e.suggestion();
        assert!(s.contains("`wml_model` resource with `ref_name: absent`"), "add-resource element: {s}");
        assert!(s.contains("replace the reference with a literal value if the resource is managed outside it"), "literal alternative: {s}");
        assert!(s.contains("Adding a `wml_model` also requires:"), "chain preamble: {s}");
        assert!(s.contains("`autoai_experiment` (referenced by `wml_model.experiment`)"), "chain hop 1: {s}");
        assert!(s.contains("`data_asset` (referenced by `autoai_experiment.training_data`)"), "chain hop 2: {s}");
        // An empty chain appends nothing.
        let no_chain = ValidationError::UnresolvedReference { field_path: "x".into(), ref_kind: "tool".into(), ref_name: "t".into(), required_chain: vec![] };
        assert!(!no_chain.suggestion().contains("also requires"), "empty chain must not render a chain clause");
    }
}
