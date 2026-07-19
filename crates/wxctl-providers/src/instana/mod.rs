//! `instana` (IBM Instana) service handlers.
//!
//! `instana_website_config` (query-param POST create),
//! `instana_maintenance_window` (PUT-upsert create, client-supplied id),
//! `instana_alert` (PUT-upsert create AND update, client-supplied id),
//! `instana_automation_action` (adopt-only: pre_create adopts by name, never
//! POSTs; pre_delete no-op), and `instana_custom_payload_configuration`
//! (id-less whole-set PUT-upsert create AND update; pre_delete owns the id-less
//! DELETE) need custom handlers; every other instana kind stays pure
//! schema-driven (no entry here → no handler).

pub mod handlers;

define_handlers! {
    "instana_website_config" => handlers::WebsiteConfigHandler,
    "instana_maintenance_window" => handlers::MaintenanceWindowHandler,
    "instana_alert" => handlers::AlertHandler,
    "instana_automation_action" => handlers::AutomationActionHandler,
    "instana_builtin_event_spec" => handlers::BuiltinEventSpecHandler,
    "instana_custom_payload_configuration" => handlers::CustomPayloadConfigurationHandler,
}
