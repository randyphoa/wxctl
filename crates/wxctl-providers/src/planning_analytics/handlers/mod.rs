//! Custom `planning_analytics` resource handlers.
//!
//! `dimension` — the default materializer doesn't apply nested `api_field` mappings inside
//! object arrays (only top-level declared fields), so a dimension's inline `hierarchies[]`
//! (with nested `elements[]`/`edges[]`) would ride the wire in snake_case and TM1 would reject
//! it (live-proven: error 278 "Missing Hierarchy name."). `DimensionHandler` owns the create
//! POST and builds the fully nested PascalCase body itself
//! (docs/troubleshoot/nested-api-field-not-materialized-fix.md).
//!
//! `hierarchy` — same nested-`api_field` gap as `dimension`, for a standalone hierarchy's
//! `elements[]`/`edges[]`. `HierarchyHandler` owns the create POST.
//!
//! `cube` — TM1 cube create binds its dimension list via the OData `Dimensions@odata.bind` key
//! (dotted -> not a declarable `api_field`), so `CubeHandler` owns the create POST.
//!
//! `chore` — TM1 chore create/update bind `Tasks[].Process@odata.bind` and toggle `active` via
//! the tm1.Activate/tm1.Deactivate actions, so `ChoreHandler` owns create + update and
//! deactivates before delete.
//!
//! `subset` — a static subset binds `Elements@odata.bind`, so `SubsetHandler` owns the create
//! POST for static subsets (MDX subsets fall through to the default materializer).
//!
//! `view` — a cube view is an abstract OData type; `ViewHandler` injects the `@odata.type`
//! discriminator (`NativeView`/`MDXView`) and passes native axes through.
//!
//! `user` — a user's group memberships bind via `Groups@odata.bind` and its `password` is
//! write-only, so `UserHandler` owns the create POST and marks the password path sensitive.
//!
//! `process` — TM1 rewrites TurboIntegrator procedure text to CRLF line endings server-side, so
//! a discovered process's procedure fields never round-trip against declared LF text.
//! `ProcessHandler` normalizes CRLF -> LF in `post_discover` (docs/troubleshoot/pa-live-gateway-quirks.md).

mod chore;
mod cube;
mod dimension;
mod hierarchy;
mod odata;
mod process;
mod subset;
mod user;
mod view;

pub use chore::ChoreHandler;
pub use cube::CubeHandler;
pub use dimension::DimensionHandler;
pub use hierarchy::HierarchyHandler;
pub use process::ProcessHandler;
pub use subset::SubsetHandler;
pub use user::UserHandler;
pub use view::ViewHandler;
