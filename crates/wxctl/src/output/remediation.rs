//! Remediation map: `(error_code, Option<field>) -> actionable one-line fix`.
//!
//! Consulted at the single panel-error choke point (`collector.rs`
//! `error_event_to_block`) and by the top-level error renderer
//! (`panel_render.rs` `render_top_level_error`). Returns `None` for unmapped
//! codes so the caller falls back to the engine-supplied `fix` string — no
//! regression on codes not listed here. Codes compared against the same
//! `WXCTL-*` constants the engine emits (`wxctl_core::logging::error_codes`).

use wxctl_core::logging::error_codes;

/// Map an `(error_code, optional field)` pair to an actionable remediation
/// line, or `None` when the code is unmapped (caller keeps the existing fix).
pub fn fix_for(code: &str, field: Option<&str>) -> Option<String> {
    let fix = match code {
        error_codes::V003 => match field {
            Some(f) => format!("add `{f}:` to the resource (see `wxctl explain <kind>`)"),
            None => "add the missing required field to the resource (see `wxctl explain <kind>`)".to_string(),
        },
        error_codes::V301 | error_codes::V302 => "set the ${env:VAR} reference, or export it before running".to_string(),
        error_codes::V001 => "rename one of the duplicate resources so each ref_name is unique".to_string(),
        error_codes::V005 => match field {
            Some("depends_on") => "fix the depends_on reference to name an existing resource (kind.ref_name)".to_string(),
            _ => return None,
        },
        error_codes::R001 => "check network connectivity and the profile's endpoint/credentials, then retry".to_string(),
        error_codes::R004 => "this kind is not supported on the active deployment; remove it or switch profiles".to_string(),
        error_codes::R005 => "an existing resource claims this identity; rename it or import the existing one".to_string(),
        error_codes::E001 => "the create operation failed; re-run with --full-trace and inspect the run record (wxctl debug)".to_string(),
        _ => return None,
    };
    Some(fix)
}
