//! `factsheets` (AI governance model inventory) service handlers.

pub mod handlers;

define_handlers! {
    "inventory" => handlers::InventoryHandler,
    "model_tracking" => handlers::ModelTrackingHandler,
}
