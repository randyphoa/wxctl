pub mod definition;
pub mod merge;
pub mod overlay;
pub mod parser;

pub use definition::{
    AbsentWhen, ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldLocation, FieldReferences, FieldType, HashStorage, HookDefinition, HttpMethod, IdentityHash, IdentityMatch, ListFilter, ReadinessDefinition, ReconciliationDefinition, ResourceDefinition, ResourceSchema,
    SchemaDefinition, UpdateStrategy, ValidationRules,
};
pub use merge::deep_merge;
pub use overlay::{effective_definition, is_unsupported_on};
pub use parser::SchemaParser;
