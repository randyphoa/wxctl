pub mod handlers;

/// Per-kind custom reconcilers (discovery + compare) for kinds the generic
/// schema-driven reconciler can't express. See `wxctl_providers::get_reconciler`.
pub fn get_reconciler(resource_name: &str) -> Option<::std::sync::Arc<dyn ::wxctl_core::traits::Reconciler>> {
    match resource_name {
        "asset_promotion" => Some(::std::sync::Arc::new(handlers::AssetPromotionReconciler)),
        _ => None,
    }
}

define_handlers! {
    "asset_promotion" => handlers::AssetPromotionHandler,
    "catalog" => handlers::CatalogHandler,
    "category" => handlers::CategoryHandler,
    "data_asset" => handlers::DataAssetHandler,
    "environment" => handlers::EnvironmentHandler,
    "job" => handlers::JobHandler,
    "job_run" => handlers::JobRunHandler,
    "business_term" => handlers::BusinessTermHandler,
    "business_terms" => handlers::BusinessTermsHandler,
    "rules" => handlers::RulesHandler,
    "rule" => handlers::RuleHandler,
    "package_extension" => handlers::PackageExtensionHandler,
    "project" => handlers::ProjectHandler,
    "script_asset" => handlers::ScriptAssetHandler,
    "software_specification" => handlers::SoftwareSpecificationHandler,
    "space" => handlers::SpaceHandler,
}
