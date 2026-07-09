//! Wasm-safe schema knowledge for wxctl: deployment types, the schema descriptor
//! model + parser, the compile-time schema set (`load_all_schemas`) and its phf
//! dependency graph, the markdown schema-reference renderer, and the structured
//! `explain` projection. No `reqwest`/`tokio`/`std::fs`/`std::time::Instant` — this
//! crate compiles for native **and** `wasm32-unknown-unknown`.

pub mod dependency_graph;
pub mod deployment;
pub mod descriptor;
pub mod explain;
pub mod render;
pub mod resource;
pub mod schema;
pub mod validation;

pub use dependency_graph::{PATH_FIELDS, SYNTH_FIELDS, load_all_schemas};
pub use descriptor::{Endpoints, FieldDescriptor, ResourceDescriptor};
pub use explain::{Authoring, ExplainView, KindSummary, authoring_overview, explain_kind, list_kinds};
pub use render::render_kinds_markdown;
pub use resource::{OnDestroyPolicy, RawResource, ValidatedResource};
pub use schema::{ResourceDefinition, ResourceSchema, SchemaParser};
pub use validation::{AnnotatedValidationError, ValidationError, ValidationReport, validate_config};
