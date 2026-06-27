pub mod handlers;

define_handlers! {
    "catalog" => handlers::CatalogHandler,
    "category" => handlers::CategoryHandler,
    "data_asset" => handlers::DataAssetHandler,
    "business_term" => handlers::BusinessTermHandler,
    "business_terms" => handlers::BusinessTermsHandler,
    "rules" => handlers::RulesHandler,
    "rule" => handlers::RuleHandler,
    "package_extension" => handlers::PackageExtensionHandler,
    "project" => handlers::ProjectHandler,
    "software_specification" => handlers::SoftwareSpecificationHandler,
    "space" => handlers::SpaceHandler,
}
