use wxctl_core::{RemoteResource, ResourceKey, ValidatedResource};

/// Status of remote resource discovery during reconciliation.
#[derive(Debug, Clone)]
pub enum DiscoveryStatus {
    /// One or more resources were discovered remotely
    Discovered {
        /// All discovered remote resources matching the name
        remotes: Vec<RemoteResource>,
    },
    /// Resource does not exist remotely
    NotFound,
    /// Discovery deferred due to unresolved dependencies
    Deferred {
        /// Keys of dependencies that are not yet in the cache
        missing_dependencies: Vec<ResourceKey>,
    },
}

pub struct ReconciliationPlan {
    pub operations: Vec<Operation>,
    pub errors: Vec<ReconciliationError>,
    /// Warn-level, non-blocking advisories raised during discovery (e.g. R501).
    pub advisories: Vec<crate::Advisory>,
}

#[derive(Debug, Clone)]
pub struct ReconciliationError {
    pub kind: String,
    pub name: String,
    pub error: String,
}

#[derive(Debug, Clone)]
pub struct Operation {
    pub key: ResourceKey,
    pub op_type: OperationType,
    pub local: Option<ValidatedResource>,
    pub remote: Option<RemoteResource>,
}

/// Why a destroy-mode reconciliation step produced no operation.
/// Surfaces in the plan/summary as `skipped (<reason>)` so the user can
/// tell the difference between "already absent remotely" and "wxctl
/// couldn't resolve deps well enough to even attempt discovery."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// Remote discovery returned NotFound; nothing to delete.
    Absent,
    /// Dependencies or templates could not be resolved; wxctl never
    /// attempted discovery.
    Deferred,
}

#[derive(Debug, Clone)]
pub enum OperationType {
    Create,
    Update {
        fields: Vec<String>,
    },
    Delete,
    Recreate,
    NoOp,
    /// Destroy-time retention: emitted when the local config has
    /// `on_destroy: retain`. Executed as a structural no-op (no handler
    /// dispatch, no API call) so the resource survives `wxctl destroy`.
    Retain,
    /// Destroy-time skip: reconciliation could not or should not emit
    /// a Delete for this resource. `Absent` = remote already gone;
    /// `Deferred` = dependency/template resolution failed. Executed as
    /// a structural no-op like `Retain`.
    Skip {
        reason: SkipReason,
    },
}

impl std::fmt::Display for OperationType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            OperationType::Create => write!(f, "create"),
            OperationType::Update { .. } => write!(f, "update"),
            OperationType::Delete => write!(f, "delete"),
            OperationType::Recreate => write!(f, "recreate"),
            OperationType::NoOp => write!(f, "no-op"),
            OperationType::Retain => write!(f, "retain"),
            OperationType::Skip { reason: SkipReason::Absent } => write!(f, "skip (absent)"),
            OperationType::Skip { reason: SkipReason::Deferred } => write!(f, "skip (deferred)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skip_display_renders_reason_suffix() {
        assert_eq!(OperationType::Skip { reason: SkipReason::Absent }.to_string(), "skip (absent)");
        assert_eq!(OperationType::Skip { reason: SkipReason::Deferred }.to_string(), "skip (deferred)");
    }
}
