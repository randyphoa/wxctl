//! `instana` (IBM Instana) service handlers.
//!
//! `instana_website_config` (query-param POST create),
//! `instana_maintenance_window` (PUT-upsert create, client-supplied id), and
//! `instana_alert` (PUT-upsert create AND update, client-supplied id) need
//! custom handlers; every other instana kind stays pure schema-driven (no entry
//! here → no handler).

pub mod handlers;

define_handlers! {
    "instana_website_config" => handlers::WebsiteConfigHandler,
    "instana_maintenance_window" => handlers::MaintenanceWindowHandler,
    "instana_alert" => handlers::AlertHandler,
}
