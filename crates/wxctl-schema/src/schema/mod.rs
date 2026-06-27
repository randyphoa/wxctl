pub mod definition;
pub mod merge;
pub mod overlay;
pub mod parser;

pub use definition::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldLocation, FieldReferences, FieldType, HookDefinition, HttpMethod, IdentityMatch, ReconciliationDefinition, ResourceDefinition, ResourceSchema, SchemaDefinition, UpdateStrategy, ValidationRules};
pub use merge::deep_merge;
pub use overlay::{effective_definition, is_unsupported_on};
pub use parser::SchemaParser;
