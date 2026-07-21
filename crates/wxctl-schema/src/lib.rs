//! Wasm-safe schema knowledge for wxctl: deployment types, the schema descriptor
//! model, the compile-time static IR (`ir`/`ir_views`) and its phf dependency
//! graph, the markdown schema-reference renderer, and the structured `explain`
//! projection. No `reqwest`/`tokio`/`std::fs`/`std::time::Instant` — this crate
//! compiles for native **and** `wasm32-unknown-unknown`.

pub mod dependency_graph;
pub mod deployment;
pub mod descriptor;
pub mod explain;
pub mod graph_export;
pub mod ir;
#[cfg(feature = "test-support")]
pub mod ir_support;
pub mod ir_views;
pub mod render;
pub mod resource;
pub mod validation;

pub use dependency_graph::{PATH_FIELDS, SYNTH_FIELDS};
pub use descriptor::{Endpoints, FieldDescriptor, ResourceDescriptor};
pub use explain::{Authoring, ExplainView, KindSummary, authoring_overview, explain_kind, list_kinds};
pub use graph_export::{BridgeRecord, EdgeRecord, FieldMap, GRAPH_FORMAT_VERSION, GraphDoc, NodeRecord, RecipeRecord, RecipeRequires, export_graph};
pub use render::render_kinds_markdown;
pub use resource::{OnDestroyPolicy, RawResource, ValidatedResource};
pub use validation::{AnnotatedValidationError, ValidationError, ValidationReport, validate_config};
