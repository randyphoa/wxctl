use super::super::reconciliation::types::Operation;

pub struct CompiledPlan {
    pub operations: Vec<Operation>,
    /// Warn-level advisories carried through from reconciliation to the command layer.
    pub advisories: Vec<crate::Advisory>,
}
