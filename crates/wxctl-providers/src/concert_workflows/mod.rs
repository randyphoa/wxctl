//! `concert_workflows` (IBM Concert Workflows / Pliant engine) service handlers.
//!
//! A separate service from `concert` core: its own host + Basic-auth realm under the
//! `/workflows/api` path prefix. `concert_worker_group`'s create/update return no body, so
//! `WorkerGroupHandler` GETs the group after each write to capture server state (the join
//! `secret`, worker statistics). `concert_workflow` has no JSON create/update at all — a flow is
//! multipart-imported from a zip, so `WorkflowHandler` owns the write and hoists a computed
//! `flow_uri` for downstream kinds to reference. `concert_workflow_schedule` (id-in-path per
//! user) and `concert_workflow_role` (inline include* query flags) are pure schema-driven — no
//! entry here → no handler.

pub mod handlers;

define_handlers! {
    "concert_workflow" => handlers::WorkflowHandler,
    "concert_worker_group" => handlers::WorkerGroupHandler,
}
