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
use crate::ir::{FieldIr, SchemaIr};
use anyhow::{Result, bail};
use std::collections::HashSet;

/// Collect `(field_name, target_kind)` for every reference marked
/// `require_ready: true`, recursing into nested object schemas.
fn collect_require_ready<'a>(fields: &'a [FieldIr], out: &mut Vec<(&'a str, &'a str)>) {
    for field in fields {
        if let Some(refs) = &field.references
            && refs.require_ready
        {
            out.push((field.name, refs.resource));
        }
        if let Some(nested) = field.schema {
            collect_require_ready(nested.fields, out);
        }
    }
}

/// Validate readiness contracts across a schema set. Returns the first
/// violation as a `WXCTL-V504` error, else `Ok(())`.
pub fn validate_readiness(schemas: &[&'static SchemaIr]) -> Result<()> {
    // Rule 1: every declared readiness block is well-formed. Collect the set
    // of kinds that declare one for Rule 2.
    let mut ready_kinds: HashSet<&str> = HashSet::new();
    for s in schemas {
        if let Some(readiness) = &s.resource.api.readiness {
            let kind = s.resource.kind;
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
        collect_require_ready(s.resource.schema.fields, &mut refs);
        for (field, target) in refs {
            if !ready_kinds.contains(target) {
                bail!("[{}] reference field '{}' on kind '{}' sets require_ready: true but target kind '{}' declares no api.readiness block", error_codes::V504, field, s.resource.kind, target);
            }
        }
    }

    Ok(())
}

#[cfg(all(test, feature = "test-support"))]
mod tests {
    use super::*;
    use crate::ir_support::compile_to_static_ir;

    /// Shared shell: an `openscale`-service kind named `__KIND__`, with
    /// `__READINESS__` spliced under `api:` (blank for none) and `__FIELDS__`
    /// spliced in for the `schema:` body.
    const SCHEMA_TEMPLATE: &str = "
resource:
  name: __KIND__
  service: openscale
  kind: __KIND__
  version: v1
  api:
    base_path: /v2/x
    id_field: id
    get_endpoint: /v2/x/{id}
    create_method: POST
    delete_method: DELETE
__READINESS__
  schema:
__FIELDS__
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    update_strategy: patch
";

    fn schema_ir(kind: &str, readiness_block: &str, fields_block: &str) -> &'static SchemaIr {
        let yaml = SCHEMA_TEMPLATE.replace("__KIND__", kind).replace("__READINESS__", readiness_block).replace("__FIELDS__", fields_block);
        compile_to_static_ir(&yaml).expect("test schema compiles")
    }

    const NO_READINESS: &str = "";
    const NO_FIELDS: &str = "    fields: []\n";

    /// A `readiness:` block (nested under `api:`) with the given `state_path`
    /// (quoted, so whitespace-only values survive) and `ready` list (`[]` when empty).
    fn readiness_block(state_path: &str, ready: &[&str]) -> String {
        let mut out = format!("    readiness:\n      state_path: \"{state_path}\"\n");
        if ready.is_empty() {
            out.push_str("      ready: []\n");
        } else {
            out.push_str("      ready:\n");
            for r in ready {
                out.push_str(&format!("        - {r}\n"));
            }
        }
        out
    }

    /// A single-field `fields:` block: a string field named `name` carrying a
    /// `references: { resource: target, field: id, require_ready }`.
    fn field_ref_block(name: &str, target: &str, require_ready: bool) -> String {
        format!("    fields:\n      - name: {name}\n        type: string\n        references:\n          resource: {target}\n          field: id\n          require_ready: {require_ready}\n")
    }

    #[test]
    fn require_ready_with_target_readiness_ok() {
        let mart = schema_ir("data_mart", &readiness_block("entity.status.state", &["active"]), NO_FIELDS);
        let monitor = schema_ir("monitor_instance", NO_READINESS, &field_ref_block("data_mart_id", "data_mart", true));
        validate_readiness(&[mart, monitor]).expect("require_ready targeting a readiness-declaring kind is valid");
    }

    #[test]
    fn require_ready_without_target_readiness_rejected() {
        // spec AC 7: target kind declares no readiness block.
        let mart = schema_ir("data_mart", NO_READINESS, NO_FIELDS);
        let monitor = schema_ir("monitor_instance", NO_READINESS, &field_ref_block("data_mart_id", "data_mart", true));
        let err = validate_readiness(&[mart, monitor]).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("WXCTL-V504"), "expected WXCTL-V504, got: {msg}");
        assert!(msg.contains("data_mart_id") && msg.contains("data_mart"), "message names field + target: {msg}");
    }

    #[test]
    fn no_opt_in_needs_no_readiness() {
        // A plain reference (require_ready: false) to a kind with no readiness is fine.
        let mart = schema_ir("data_mart", NO_READINESS, NO_FIELDS);
        let monitor = schema_ir("monitor_instance", NO_READINESS, &field_ref_block("data_mart_id", "data_mart", false));
        validate_readiness(&[mart, monitor]).expect("references without require_ready need no readiness block");
    }

    #[test]
    fn readiness_empty_state_path_rejected() {
        let mart = schema_ir("data_mart", &readiness_block("  ", &["active"]), NO_FIELDS);
        let err = validate_readiness(&[mart]).unwrap_err();
        assert!(err.to_string().contains("WXCTL-V504"), "got: {err}");
        assert!(err.to_string().contains("state_path"), "got: {err}");
    }

    #[test]
    fn readiness_empty_ready_rejected() {
        let mart = schema_ir("data_mart", &readiness_block("entity.status.state", &[]), NO_FIELDS);
        let err = validate_readiness(&[mart]).unwrap_err();
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
        let schemas: Vec<&'static SchemaIr> = crate::ir::RESOURCE_IR.values().copied().collect();
        validate_readiness(&schemas).expect("shipped schema set must satisfy the readiness contract (WXCTL-V504)");
    }
}
