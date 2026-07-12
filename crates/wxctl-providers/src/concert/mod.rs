//! `concert` (IBM Concert) service handlers.
//!
//! `concert_application` (nested release-id hoist so `application_release_id` resolves
//! from `associations.releases[0].id`), `concert_source_repo` (bulk create),
//! `concert_credential` (collection delete), `concert_automation_rule` (collection delete),
//! `concert_compliance_profile` (id recovery via list-and-match), and the three
//! `concert_resilience_*` kinds `library`/`profile`/`posture` (create-response `<x>_id → id`
//! mapping; the profile additionally has no update verb — drift recreates) need custom
//! handlers; every other concert kind stays pure schema-driven (no entry here → no handler).

pub mod handlers;

define_handlers! {
    "concert_application" => handlers::ApplicationHandler,
    "concert_source_repo" => handlers::SourceRepoHandler,
    "concert_credential" => handlers::CredentialHandler,
    "concert_automation_rule" => handlers::AutomationRuleHandler,
    "concert_compliance_profile" => handlers::ComplianceProfileHandler,
    "concert_resilience_library" => handlers::ResilienceLibraryHandler,
    "concert_resilience_profile" => handlers::ResilienceProfileHandler,
    "concert_resilience_posture" => handlers::ResiliencePostureHandler,
}
