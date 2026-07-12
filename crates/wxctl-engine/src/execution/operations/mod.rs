pub(super) mod create;
mod delete;
mod recreate;
mod update;

use super::ExecutionState;
use super::types::ExecutionResult;
use crate::reconciliation::types::{Operation, OperationType};
use anyhow::{Result, anyhow};
use std::future::Future;
use tracing::{Instrument, debug_span};

pub(super) fn execute_single_operation<'a>(planned_op: &'a Operation, state: &'a ExecutionState) -> impl Future<Output = Result<ExecutionResult>> + Send + 'a {
    let op = planned_op;

    let span = debug_span!(
        target: "wxctl::substage::execution",
        "execute_resource",
        kind = %op.key.kind,
        name = %op.key.name,
        action = ?op.op_type
    );

    async {
        match &op.op_type {
            OperationType::Create => create::execute(planned_op, state).await,
            OperationType::Update { .. } => update::execute(planned_op, state).await,
            OperationType::Delete => delete::execute(planned_op, state).await,
            OperationType::Recreate => recreate::execute(planned_op, state).await,
            OperationType::NoOp => Err(anyhow!("NoOp operation reached execute_single_operation - this is a bug")),
            OperationType::Retain => Err(anyhow!("Retain operation reached execute_single_operation - this is a bug")),
            OperationType::Skip { .. } => Err(anyhow!("Skip operation reached execute_single_operation - this is a bug")),
        }
    }
    .instrument(span)
}
