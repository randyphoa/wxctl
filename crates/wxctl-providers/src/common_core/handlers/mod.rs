pub mod business_term;
pub mod business_terms;
pub mod catalog;
pub mod catalog_discovery;
pub mod category;
pub mod cos_discovery;
pub mod data_asset;
pub mod package_extension;
pub mod project;
pub mod rule;
pub mod rules;
pub mod software_specification;
pub mod space;
pub mod wml_discovery;

pub use business_term::BusinessTermHandler;
pub use business_terms::BusinessTermsHandler;
pub use catalog::CatalogHandler;
pub use category::CategoryHandler;
pub use data_asset::DataAssetHandler;
pub use package_extension::PackageExtensionHandler;
pub use project::ProjectHandler;
pub use rule::RuleHandler;
pub use rules::RulesHandler;
pub use software_specification::SoftwareSpecificationHandler;
pub use space::SpaceHandler;

use wxctl_core::client::HttpClient;
use wxctl_core::types::Flavor;

/// Returns true (and emits a debug log) when the active deployment is not SaaS,
/// signalling that a discovery hook should early-return. The IBM Cloud
/// Resource Controller / Global Catalog used by these hooks is SaaS-only;
/// on Software/CPD the resource_crn / compute fields must be supplied
/// explicitly in YAML.
pub(super) fn skip_on_non_saas(client: &HttpClient, operation_id: &str, hook: &str) -> bool {
    if client.deployment().flavor() == Flavor::Saas {
        return false;
    }
    tracing::debug!(
        target: "wxctl::substage::provider",
        operation_id = %operation_id,
        deployment = %client.deployment(),
        hook = %hook,
        "skipping discovery hook on non-SaaS deployment",
    );
    true
}
