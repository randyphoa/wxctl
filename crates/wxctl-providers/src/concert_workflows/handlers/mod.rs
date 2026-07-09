//! Custom `concert_workflows` resource handlers.
//!
//! `worker_group` — Pliant's worker-group create (POST /v1/worker-group) and update
//! (PUT /v1/worker-group/{name}) return NO body, so `WorkerGroupHandler` GETs the group after
//! each write and replaces the response with the fetched object — so `id_field: name`, state
//! comparison, and `${concert_worker_group.<ref>.secret}` all resolve. A failed read-back fails
//! the op (never report green on a blind write — spec Error Handling).
//!
//! `workflow` — a Pliant flow has no JSON create/update; `WorkflowHandler` multipart-imports a
//! zip and hoists a computed `flow_uri` (create-side in pre_create/pre_update, discovery-side in
//! post_discover) for downstream kinds to reference.

mod worker_group;
mod workflow;

pub use worker_group::WorkerGroupHandler;
pub use workflow::WorkflowHandler;
