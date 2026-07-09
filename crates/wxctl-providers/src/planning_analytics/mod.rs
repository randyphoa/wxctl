//! `planning_analytics` (IBM Planning Analytics / TM1 Database 12) service handlers.
//!
//! Custom handlers cover the dotted `@odata.bind` create bodies (and the `@odata.type`
//! discriminator) the default materializer would drop: `pa_cube` binds `Dimensions@odata.bind`;
//! `pa_chore` binds `Tasks[].Process@odata.bind` and reconciles `active` via the
//! tm1.Activate/tm1.Deactivate OData actions; `pa_subset` binds `Elements@odata.bind` for static
//! subsets; `pa_view` injects the `@odata.type` (NativeView/MDXView) discriminator; `pa_user`
//! binds `Groups@odata.bind` and carries a write-only `password` marked sensitive on the
//! request. `pa_dimension` and `pa_hierarchy` are NOT pure schema-driven despite having no
//! `@odata.bind` key: their inline `elements`/`edges` arrays need nested `api_field` mapping to
//! PascalCase that the default materializer doesn't apply (it only maps top-level declared
//! fields, not keys inside object-array items — live-proven, TM1 error 278 "Missing Hierarchy
//! name." on a snake_case body; docs/troubleshoot/nested-api-field-not-materialized-fix.md), so
//! `DimensionHandler`/`HierarchyHandler` own their create POSTs. `pa_process` normalizes
//! CRLF -> LF procedure text in `post_discover` (TM1 rewrites TI procedure line endings
//! server-side; docs/troubleshoot/pa-live-gateway-quirks.md) — otherwise pure schema-driven, no
//! create/update body reshape needed. `pa_cube` also hoists its dimension list in
//! `post_discover` (the default GET carries no `Dimensions` key; see `handlers/cube.rs`). Every
//! other planning_analytics kind (pa_group, pa_sql_data_source) is pure schema-driven — no
//! entry here -> no handler.
//!
//! The `planning_analytics` profile block shape (auth_type `pa_session`, `path_prefix`
//! `/tm1/<database>/api/v1`, `apikey: ${env:PA_SESSION}`) is documented on the `pa_dimension`
//! schema (`wxctl-schema/src/schemas/planning_analytics/pa_dimension.yaml`).

pub mod handlers;

define_handlers! {
    "pa_dimension" => handlers::DimensionHandler,
    "pa_hierarchy" => handlers::HierarchyHandler,
    "pa_cube" => handlers::CubeHandler,
    "pa_process" => handlers::ProcessHandler,
    "pa_chore" => handlers::ChoreHandler,
    "pa_subset" => handlers::SubsetHandler,
    "pa_view" => handlers::ViewHandler,
    "pa_user" => handlers::UserHandler,
}
