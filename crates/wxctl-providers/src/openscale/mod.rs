//! `openscale` (watsonx.governance OpenScale) service handlers.
//!
//! `guardrails_policy` has a custom handler (it targets the OpenScale root
//! host's Guardrails Manager API, which the schema layer can't express),
//! `subscription` has one (default data-set creation + poll to `active` +
//! optional record seeding after the schema-driven create), and
//! `monitor_instance` has one (optional first evaluation run on
//! `evaluate_on_create`). Every other openscale kind stays pure
//! schema-driven (no entry here → no handler).

pub mod handlers;

define_handlers! {
    "guardrails_policy" => handlers::GuardrailsPolicyHandler,
    "monitor_instance" => handlers::MonitorInstanceHandler,
    "subscription" => handlers::SubscriptionHandler,
}
