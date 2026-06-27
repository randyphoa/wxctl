pub mod config;
pub mod error;
pub mod resource;

pub use config::{AuthConfig, Config, Profile, ServiceConfig};
pub use error::error_chain_vec;
pub use resource::{IStr, OnDestroyPolicy, RawResource, RemoteResource, ResourceKey, ValidatedResource, istr};
pub use wxctl_schema::deployment::{Deployment, DeploymentConstraint, DeploymentConstraintList, Flavor, select_overlay_key};
