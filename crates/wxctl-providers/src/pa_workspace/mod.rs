//! `pa_workspace` (IBM Planning Analytics Workspace content, `/pacontent/v1`) service handlers.
//!
//! All six `paw_*` content kinds live in the one `Assets` OData store and share one CRUD contract
//! (live-proven PA 2.1.20, 2026-07-17; `docs/troubleshoot/pa-live-gateway-quirks.md`, memory
//! `pacontent-book-roundtrip`): create = `POST /Assets` (leading-slash `path`), update =
//! content-only `PUT Assets(id='{id}',type='{type}')` (any other attribute -> 400
//! `unsupported PUT attribute`), delete = `DELETE Assets(id='{id}',type='{type}')`. The default
//! materializer can express neither the composite key nor the file-loaded opaque `content`
//! document (`docs/troubleshoot/nested-api-field-not-materialized-fix.md`), so one shared
//! `AssetHandler` OWNS create/update/delete (all `HookOutcome::Handled`). Each kind binds its own
//! OData asset `type` as a constructor arg at registration below, rather than the handler sniffing
//! a `kind` field off the resource at hook time: the engine passes only materialized schema fields
//! into hooks, which carry no `kind` (`wxctl-engine/src/execution/operations/create.rs`). It loads
//! `content` from its file in `post_validate` and fetches an asset's `content` in `post_discover`
//! so `state_fields: [name, content]` round-trips.
//!
//! The `pa_workspace` profile block reuses `planning_analytics`' `auth_type: pa_session` cookie on
//! the same gateway `url` with `path_prefix: /pacontent/v1` — no new auth code
//! (`authenticate_pa_session` is reused as-is).

pub mod handlers;

/// Single in-crate source of truth for each `paw_*` kind's `/pacontent/v1` OData
/// asset type. Both the handler registry below and the schema `list_filter.equals`
/// in `wxctl-schema/src/schemas/pa_workspace/*.yaml` must agree; the
/// `paw_asset_type_matches_schema_list_filter` test pins them together (spec AC5).
pub(crate) fn paw_asset_type(kind: &str) -> Option<&'static str> {
    match kind {
        "paw_book" => Some("dashboard"),
        "paw_folder" => Some("folder"),
        "paw_application" => Some("application"),
        "paw_plan" => Some("plan"),
        "paw_view" => Some("tm1view"),
        "paw_workbench" => Some("workbench"),
        _ => None,
    }
}

define_handlers! {
    "paw_book" => handlers::AssetHandler::new(paw_asset_type("paw_book").expect("bound")),
    "paw_folder" => handlers::AssetHandler::new(paw_asset_type("paw_folder").expect("bound")),
    "paw_application" => handlers::AssetHandler::new(paw_asset_type("paw_application").expect("bound")),
    "paw_plan" => handlers::AssetHandler::new(paw_asset_type("paw_plan").expect("bound")),
    "paw_view" => handlers::AssetHandler::new(paw_asset_type("paw_view").expect("bound")),
    "paw_workbench" => handlers::AssetHandler::new(paw_asset_type("paw_workbench").expect("bound")),
}

#[cfg(test)]
mod parity {
    use super::*;

    /// AC5: every `paw_*` schema's `list_filter.equals` equals the asset type its
    /// handler is bound to, and every paw kind is actually bound. Iterates the
    /// compiled schema set so a newly added `paw_*` schema is covered automatically.
    #[test]
    fn paw_asset_type_matches_schema_list_filter() {
        let schemas = wxctl_schema::load_all_schemas().expect("load schemas");
        let mut checked = 0;
        for schema in &schemas {
            let kind = schema.resource.name.as_str();
            if !kind.starts_with("paw_") {
                continue;
            }
            checked += 1;
            let bound = paw_asset_type(kind).unwrap_or_else(|| panic!("paw kind '{kind}' has no paw_asset_type entry / handler binding"));
            assert!(get_handler(kind).is_some(), "paw kind '{kind}' is not registered in define_handlers!");
            let lf = schema.resource.reconciliation.discovery.list_filter.as_ref().unwrap_or_else(|| panic!("paw kind '{kind}' schema declares no list_filter"));
            assert_eq!(lf.field, "type", "paw kind '{kind}' list_filter.field must be 'type'");
            assert_eq!(lf.equals, bound, "drift: schema '{kind}' list_filter.equals ('{}') != handler asset type ('{}')", lf.equals, bound);
        }
        assert_eq!(checked, 6, "expected exactly six paw_* schemas, found {checked}");
    }
}
