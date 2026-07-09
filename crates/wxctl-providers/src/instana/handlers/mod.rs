//! Custom `instana` resource handlers.
//!
//! `website_config` — Instana creates an EUM website via
//! `POST /website-monitoring/config?name=<name>` (name is a QUERY param, no
//! body), so `WebsiteConfigHandler` owns the POST via `HookOutcome::Handled` and
//! records the server-assigned `id`.
//!
//! `maintenance_window` — no POST; create is an idempotent
//! `PUT /settings/v2/maintenance/{id}` upsert with a client-supplied id, owned by
//! `MaintenanceWindowHandler.pre_create`; discovery is `get_by_id`.

mod maintenance_window;
mod website_config;

pub use maintenance_window::MaintenanceWindowHandler;
pub use website_config::WebsiteConfigHandler;
