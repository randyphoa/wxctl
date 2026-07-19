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
//!
//! `alert` — no POST; create AND update are the same idempotent
//! `PUT /events/settings/alerts/{id}` upsert with a client-supplied id, owned by
//! `AlertHandler.pre_create` + `pre_update`; discovery is `get_by_id`.
//!
//! `automation_action` — ADOPT-ONLY (the API is GET-only for actions):
//! `AutomationActionHandler.pre_create` adopts an existing action by `name` or
//! errors, never POSTs; `pre_delete` is a no-op.
//!
//! `builtin_event_spec` — ADOPT + TOGGLE: `BuiltinEventSpecHandler.pre_create`
//! adopts a built-in by id (GET /{id}; a miss errors) and `pre_create`/`pre_update`
//! converge `enabled` via POST `/{id}/enable`|`/{id}/disable`; destroy is the
//! schema-driven DELETE /{id}.
//!
//! `custom_payload_configuration` — tenant-GLOBAL singleton, no POST and no id
//! in the GET/PUT response: create AND update are the same idempotent whole-set
//! `PUT /events/settings/custom-payload-configurations` upsert, owned by
//! `CustomPayloadConfigurationHandler.pre_create` + `pre_update`; `pre_delete`
//! also owns the id-less DELETE outright (default update/delete paths need
//! `id_field` from remote data, which this endpoint never returns).

mod alert;
mod automation_action;
mod builtin_event_spec;
mod custom_payload_configuration;
mod maintenance_window;
mod website_config;

pub use alert::AlertHandler;
pub use automation_action::AutomationActionHandler;
pub use builtin_event_spec::BuiltinEventSpecHandler;
pub use custom_payload_configuration::CustomPayloadConfigurationHandler;
pub use maintenance_window::MaintenanceWindowHandler;
pub use website_config::WebsiteConfigHandler;
