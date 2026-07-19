//! `pa_workspace` resource handlers. One `AssetHandler` backs all six `paw_*` content kinds (see
//! asset.rs) — each registration binds its own OData asset `type` via `AssetHandler::new`.

mod asset;

pub use asset::AssetHandler;
