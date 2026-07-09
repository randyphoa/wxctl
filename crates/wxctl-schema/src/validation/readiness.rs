//! Schema-set readiness-contract validation (Phase 1 of the reference-readiness
//! gate). Pure-data, wasm-safe. Two authoring rules, both surfaced as
//! `WXCTL-V504`:
//!   1. A kind that declares `api.readiness` must give a non-empty `state_path`
//!      and a non-empty `ready` list.
//!   2. A reference marked `require_ready: true` must target a kind that
//!      declares an `api.readiness` block.
//!
//! Errors on the first violation. Nested object schemas are walked so a
//! `require_ready` reference at any depth is checked (mirrors
//! `dependency::collect_allowed_kinds`).

use super::error_codes;
use crate::schema::{FieldDefinition, ResourceSchema};
use anyhow::{Result, bail};
use std::collections::HashSet;

/// Collect `(field_name, target_kind)` for every reference marked
/// `require_ready: true`, recursing into nested object schemas.
fn collect_require_ready<'a>(fields: &'a [FieldDefinition], out: &mut Vec<(&'a str, &'a str)>) {
    for field in fields {
        if let Some(refs) = &field.references
            && refs.require_ready
        {
            out.push((field.name.as_str(), refs.resource.as_str()));
        }
        if let Some(nested) = &field.schema {
            collect_require_ready(&nested.fields, out);
        }
    }
}

/// Validate readiness contracts across a schema set. Returns the first
/// violation as a `WXCTL-V504` error, else `Ok(())`.
pub fn validate_readiness(schemas: &[ResourceSchema]) -> Result<()> {
    // Rule 1: every declared readiness block is well-formed. Collect the set
    // of kinds that declare one for Rule 2.
    let mut ready_kinds: HashSet<&str> = HashSet::new();
    for s in schemas {
        if let Some(readiness) = &s.resource.api.readiness {
            let kind = s.resource.kind.as_str();
            if readiness.state_path.trim().is_empty() {
                bail!("[{}] kind '{}' declares an api.readiness block with an empty state_path", error_codes::V504, kind);
            }
            if readiness.ready.is_empty() {
                bail!("[{}] kind '{}' declares an api.readiness block with an empty ready list", error_codes::V504, kind);
            }
            ready_kinds.insert(kind);
        }
    }

    // Rule 2: every require_ready reference targets a kind with a readiness block.
    for s in schemas {
        let mut refs = Vec::new();
        collect_require_ready(&s.resource.schema.fields, &mut refs);
        for (field, target) in refs {
            if !ready_kinds.contains(target) {
                bail!("[{}] reference field '{}' on kind '{}' sets require_ready: true but target kind '{}' declares no api.readiness block", error_codes::V504, field, s.resource.kind, target);
            }
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::*;

    fn api(readiness: Option<ReadinessDefinition>) -> ApiDefinition {
        ApiDefinition { base_path: "/v2/x".into(), id_field: "id".into(), list_endpoint: None, get_endpoint: "/v2/x/{id}".into(), create_endpoint: None, create_method: HttpMethod::Post, update_endpoint: None, update_method: None, delete_endpoint: None, delete_method: HttpMethod::Delete, readiness }
    }

    fn field_ref(name: &str, target: &str, require_ready: bool) -> FieldDefinition {
        FieldDefinition {
            name: name.into(),
            field_type: FieldType::String,
            required: false,
            immutable: false,
            location: FieldLocation::Body,
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references: Some(FieldReferences { resource: target.into(), field: "id".into(), also_allows: vec![], optional: false, require_ready }),
            api_field: None,
            sensitive: false,
            also_query: false,
            properties: None,
            is_path: false,
            synthesize: None,
            synth_shape: None,
        }
    }

    fn schema(kind: &str, api: ApiDefinition, fields: Vec<FieldDefinition>) -> ResourceSchema {
        ResourceSchema {
            resource: ResourceDefinition {
                name: kind.into(),
                service: "openscale".into(),
                kind: kind.into(),
                version: "v1".into(),
                api,
                schema: SchemaDefinition { fields, ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, id_source: "id".into() },
                    state_fields: None,
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec![],
                    reject_on_immutable_drift: false,
                    use_json_patch: true,
                    json_patch_path_prefix: None,
                    identity_hash: None,
                },
                hooks: HookDefinition::default(),
                deployments: None,
                unsupported_on: vec![],
                description: None,
                prompt: None,
            },
        }
    }

    fn readiness_ok() -> ReadinessDefinition {
        ReadinessDefinition { state_path: "entity.status.state".into(), ready: vec!["active".into()], failed: vec![], timeout_env: None, timeout_default: 300, interval_secs: 5 }
    }

    #[test]
    fn require_ready_with_target_readiness_ok() {
        let mart = schema("data_mart", api(Some(readiness_ok())), vec![]);
        let monitor = schema("monitor_instance", api(None), vec![field_ref("data_mart_id", "data_mart", true)]);
        validate_readiness(&[mart, monitor]).expect("require_ready targeting a readiness-declaring kind is valid");
    }

    #[test]
    fn require_ready_without_target_readiness_rejected() {
        // spec AC 7: target kind declares no readiness block.
        let mart = schema("data_mart", api(None), vec![]);
        let monitor = schema("monitor_instance", api(None), vec![field_ref("data_mart_id", "data_mart", true)]);
        let err = validate_readiness(&[mart, monitor]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("WXCTL-V504"), "expected WXCTL-V504, got: {msg}");
        assert!(msg.contains("data_mart_id") && msg.contains("data_mart"), "message names field + target: {msg}");
    }

    #[test]
    fn no_opt_in_needs_no_readiness() {
        // A plain reference (require_ready: false) to a kind with no readiness is fine.
        let mart = schema("data_mart", api(None), vec![]);
        let monitor = schema("monitor_instance", api(None), vec![field_ref("data_mart_id", "data_mart", false)]);
        validate_readiness(&[mart, monitor]).expect("references without require_ready need no readiness block");
    }

    #[test]
    fn readiness_empty_state_path_rejected() {
        let bad = ReadinessDefinition { state_path: "  ".into(), ready: vec!["active".into()], failed: vec![], timeout_env: None, timeout_default: 300, interval_secs: 5 };
        let err = validate_readiness(&[schema("data_mart", api(Some(bad)), vec![])]).unwrap_err();
        assert!(err.to_string().contains("WXCTL-V504"), "got: {err}");
        assert!(err.to_string().contains("state_path"), "got: {err}");
    }

    #[test]
    fn readiness_empty_ready_rejected() {
        let bad = ReadinessDefinition { state_path: "entity.status.state".into(), ready: vec![], failed: vec![], timeout_env: None, timeout_default: 300, interval_secs: 5 };
        let err = validate_readiness(&[schema("data_mart", api(Some(bad)), vec![])]).unwrap_err();
        assert!(err.to_string().contains("WXCTL-V504"), "got: {err}");
        assert!(err.to_string().contains("ready list"), "got: {err}");
    }

    /// Load-path binding for AC 7 over the SHIPPED schemas. `validate_readiness`
    /// is otherwise reached only by synthetic tests; this runs it against the
    /// real embedded set so the Phase 3 wiring (data_mart `api.readiness` +
    /// monitor_instance `data_mart_id` `require_ready`) is proven consistent and
    /// any future `require_ready` added against a readiness-less kind fails
    /// `cargo test -p wxctl-schema`.
    #[test]
    fn shipped_schema_set_is_readiness_consistent() {
        let schemas = crate::load_all_schemas().expect("all embedded schemas parse");
        validate_readiness(&schemas).expect("shipped schema set must satisfy the readiness contract (WXCTL-V504)");
    }
}
