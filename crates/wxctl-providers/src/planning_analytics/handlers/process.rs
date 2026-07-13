//! `pa_process` handler — TM1 rewrites TurboIntegrator procedure text to CRLF (`\r\n`) line
//! endings server-side, so a discovered process's `prolog_procedure`/`metadata_procedure`/
//! `data_procedure`/`epilog_procedure` never round-trips against declared LF (`\n`) text: every
//! re-plan shows a phantom `~ update [~prolog_procedure]` (live 2026-07-03;
//! `docs/troubleshoot/pa-live-gateway-quirks.md`).
//!
//! `ProcessHandler` normalizes CRLF -> LF in `post_discover` so the comparator sees equal
//! strings. State comparison (`SchemaBasedReconciler::compare`,
//! `wxctl-engine/src/reconciliation/schema_reconciler.rs`) reads `get_nested_field(&remote.data,
//! state_field)` where `state_field` is the schema's snake_case field name (e.g.
//! `prolog_procedure`) — by the time `post_discover` runs, `denormalize_api_response` has
//! already cloned the server's PascalCase `PrologProcedure` value into that snake_case key
//! (`enrich_and_cache` in `wxctl-engine/src/reconciliation/pipeline.rs` calls `post_discover`
//! AFTER `discover()`'s denormalization step), so this normalizes the snake_case keys the
//! comparator actually reads. The raw PascalCase keys are normalized too for consistency,
//! though nothing else currently reads them.
//!
//! No other planning_analytics kind needs a custom hook for pure schema-driven fields — see
//! `handlers/mod.rs` for the full handler roster.

use anyhow::Result;
use serde_json::Value;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::HttpClient;
use wxctl_core::traits::ResourceHandler;

const PROCEDURE_KEYS: [&str; 8] = ["prolog_procedure", "metadata_procedure", "data_procedure", "epilog_procedure", "PrologProcedure", "MetadataProcedure", "DataProcedure", "EpilogProcedure"];

pub struct ProcessHandler;

impl ResourceHandler for ProcessHandler {
    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            normalize_procedure_line_endings(remote_data);
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, "pa_process discovered; procedure line endings normalized CRLF->LF for NoChange compare");
            Ok(())
        })
    }
}

/// Rewrite `\r\n` -> `\n` in-place for every procedure-text field present on `value` (both the
/// denormalized snake_case keys and the raw PascalCase keys). No-op for any key that is absent
/// or not a string.
fn normalize_procedure_line_endings(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else { return };
    for key in PROCEDURE_KEYS {
        if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
            let normalized = s.replace("\r\n", "\n");
            obj.insert(key.to_string(), Value::String(normalized));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Pure-function unit tests of the normalizer (no I/O) — matches the co-located test
    // convention of every existing handler (e.g. concert/handlers/source_repo.rs).
    // CRLF->LF is applied to every present procedure-text key (snake_case + PascalCase);
    // absent or LF-only text is left byte-identical so re-plan stays NoChange.
    #[test]
    fn normalize_procedure_line_endings_cases() {
        let cases: &[(&str, Value, Value)] = &[
            (
                "CRLF converted across snake_case + PascalCase keys",
                json!({
                    "name": "wxctlLoadSales",
                    "prolog_procedure": "# wxctl demo process — no-op prolog.\r\nProcessQuit;\r\n",
                    "PrologProcedure": "# wxctl demo process — no-op prolog.\r\nProcessQuit;\r\n",
                    "metadata_procedure": "Foo\r\nBar",
                }),
                json!({
                    "name": "wxctlLoadSales",
                    "prolog_procedure": "# wxctl demo process — no-op prolog.\nProcessQuit;\n",
                    "PrologProcedure": "# wxctl demo process — no-op prolog.\nProcessQuit;\n",
                    "metadata_procedure": "Foo\nBar",
                }),
            ),
            ("no-op when procedure keys absent", json!({"name": "wxctlLoadSales"}), json!({"name": "wxctlLoadSales"})),
            ("LF-only text left unchanged", json!({"prolog_procedure": "ProcessQuit;\n"}), json!({"prolog_procedure": "ProcessQuit;\n"})),
        ];
        for (label, mut value, expected) in cases.iter().map(|(l, i, e)| (*l, i.clone(), e.clone())) {
            normalize_procedure_line_endings(&mut value);
            assert_eq!(value, expected, "case={label}");
        }
    }
}
