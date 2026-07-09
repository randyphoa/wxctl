//! Build an agent-ready diagnosis bundle from one `RunArtifact`. Markdown is the
//! default surface (human + agent); JSON is the machine form. The fix-instructions
//! tail generalizes `validate --fix-prompt` (config + errors + an actionable
//! instruction) to any command's artifact — assembled from the bundle's own fields,
//! since `wxctl-core` cannot depend on `wxctl-compose`.

use crate::diagnose::artifact::{EventLine, RunArtifact};
use crate::diagnose::triage::{TriageClass, classify};
use crate::diagnose::troubleshoot::{TroubleshootMatch, match_troubleshoot};
use serde::Serialize;
use serde_json::Value;

/// A redacted HTTP request/response pair decoded from an error event's `context`.
#[derive(Debug, Clone, Serialize)]
pub struct Exchange {
    pub request_body: Value,
    pub response_body: Value,
}

/// One diagnosed error: identity + cause + fix + triage + the failing exchange +
/// the events that immediately preceded it + localization pointers.
#[derive(Debug, Clone, Serialize)]
pub struct ErrorBlock {
    pub error_code: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    pub fix: String,
    pub triage: TriageClass,
    pub triage_guidance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub span: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exchange: Option<Exchange>,
    /// Full anyhow context chain (from the WXCTL-E000 wrapper or this event).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub error_chain: Vec<String>,
    /// Backtrace text when present (panics, WXCTL-P001).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backtrace: Option<String>,
    /// `decision`/`dependency` events preceding this error, oldest→newest.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub preceding: Vec<String>,
}

/// The whole bundle: run identity + outcome + one block per error + matched
/// troubleshoot docs + the fix-instructions tail.
#[derive(Debug, Clone, Serialize)]
pub struct DiagnosisBundle {
    pub run_id: String,
    pub command: String,
    pub outcome: String,
    pub full_trace: bool,
    pub config_paths: Vec<String>,
    pub errors: Vec<ErrorBlock>,
    pub troubleshoot: Vec<TroubleshootMatch>,
    pub fix_instructions: String,
}

/// Decode an error event's `context` string into an `Exchange`, if present + valid.
fn decode_exchange(ev: &EventLine) -> Option<Exchange> {
    let raw = ev.get("context")?;
    let v: Value = serde_json::from_str(raw).ok()?;
    Some(Exchange { request_body: v.get("request_body").cloned().unwrap_or(Value::Null), response_body: v.get("response_body").cloned().unwrap_or(Value::Null) })
}

/// Decode the `error_chain` string (a JSON array) into a vec; empty when absent.
fn decode_chain(ev: &EventLine) -> Vec<String> {
    ev.get("error_chain").and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok()).unwrap_or_default()
}

/// Lowercased keyword set drawn from an error message — short, code-like tokens
/// dropped. Used to widen troubleshoot matching beyond the bare code.
fn keywords_from(message: &str) -> Vec<String> {
    message.split(|c: char| !c.is_alphanumeric()).filter(|w| w.len() >= 5).map(|w| w.to_lowercase()).take(8).collect()
}

/// Build the bundle. Pure: reads no files except the troubleshoot dir (via the matcher).
pub fn build_bundle(art: &RunArtifact) -> DiagnosisBundle {
    let mut blocks = Vec::new();
    let mut all_codes: Vec<String> = Vec::new();
    let mut all_keywords: Vec<String> = Vec::new();

    for (idx, ev) in art.events.iter().enumerate() {
        if !ev.is_error() {
            continue;
        }
        let error_code = ev.get("error_code").unwrap_or("UNKNOWN").to_string();
        let message = ev.get("message").unwrap_or("").to_string();
        let triage = classify(&error_code);
        let resource = match (ev.get("resource_type"), ev.get("resource_name")) {
            (Some(t), Some(n)) => Some(format!("{t}.{n}")),
            _ => None,
        };
        // Up to 8 immediately-preceding decision/dependency events, oldest→newest.
        let preceding: Vec<String> = art.events[..idx]
            .iter()
            .rev()
            .filter(|e| e.is_decision() || e.is_dependency())
            .take(8)
            .map(|e| {
                let kind = if e.is_decision() { e.get("decision").unwrap_or("decision") } else { e.get("status").unwrap_or("dependency") };
                let res = match (e.get("resource_type"), e.get("resource_name")) {
                    (Some(t), Some(n)) => format!("{t}.{n}"),
                    _ => String::new(),
                };
                let detail = e.get("reason").or_else(|| e.get("depends_on_name")).unwrap_or("");
                format!("{kind} {res} — {detail}").trim().to_string()
            })
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();

        all_codes.push(error_code.clone());
        all_keywords.extend(keywords_from(&message));

        blocks.push(ErrorBlock {
            error_code,
            resource,
            message,
            cause: ev.get("cause").map(str::to_string),
            fix: ev.get("fix").unwrap_or("").to_string(),
            triage,
            triage_guidance: triage.guidance().to_string(),
            field_path: ev.get("field_path").map(str::to_string),
            trace_id: ev.get("trace_id").map(str::to_string).filter(|t| t != "None"),
            span: ev.span.clone(),
            src: ev.src.clone(),
            exchange: decode_exchange(ev),
            error_chain: decode_chain(ev),
            backtrace: ev.get("backtrace").map(str::to_string),
            preceding,
        });
    }

    let troubleshoot = match_troubleshoot(&all_codes, &all_keywords, 5);
    let fix_instructions = build_fix_instructions(&art.manifest.command, &art.manifest.config_paths, &blocks);

    DiagnosisBundle {
        run_id: art.run_id.clone(),
        command: art.manifest.command.clone(),
        outcome: art.manifest.outcome.clone().unwrap_or_else(|| "unknown".to_string()),
        full_trace: art.manifest.full_trace,
        config_paths: art.manifest.config_paths.clone(),
        errors: blocks,
        troubleshoot,
        fix_instructions,
    }
}

/// Generalize `validate --fix-prompt`: an actionable instruction tail an agent can
/// follow to fix the run, built from the bundle's own fields (config paths + each
/// error's fix + triage class). No external template — self-contained.
fn build_fix_instructions(command: &str, config_paths: &[String], blocks: &[ErrorBlock]) -> String {
    let mut s = String::new();
    s.push_str(&format!("This `wxctl {command}` run failed. Fix the configuration and re-run.\n\n"));
    if !config_paths.is_empty() {
        s.push_str(&format!("Config file(s): {}\n\n", config_paths.join(", ")));
    }
    s.push_str("Apply each fix below, then re-run the same command:\n");
    for (i, b) in blocks.iter().enumerate() {
        let target = b.resource.as_deref().unwrap_or("(run-level)");
        let field = b.field_path.as_deref().map(|f| format!(" field `{f}`")).unwrap_or_default();
        s.push_str(&format!("{}. [{}] [{}] {}{}: {}\n", i + 1, b.error_code, b.triage.label(), target, field, b.fix));
    }
    // Self-escalation hint for suspected wxctl bugs not yet captured at full fidelity.
    if blocks.iter().any(|b| b.triage == TriageClass::SuspectedWxctlBug) {
        s.push_str("\nAt least one error is a suspected wxctl bug. If this run was not captured with --full-trace, re-run the command with --full-trace and diagnose again before editing source.\n");
    }
    s
}

impl DiagnosisBundle {
    /// Agent-facing markdown. One section per error with code, triage, cause, fix,
    /// the failing exchange, preceding events, and localization pointers; then any
    /// matched troubleshoot docs; then the fix-instructions tail.
    pub fn render_markdown(&self) -> String {
        let mut o = String::new();
        o.push_str(&format!("# Diagnosis — run {}\n\n", self.run_id));
        o.push_str(&format!("- command: `{}`\n- outcome: **{}**\n- full_trace: {}\n", self.command, self.outcome, self.full_trace));
        if !self.config_paths.is_empty() {
            o.push_str(&format!("- config: {}\n", self.config_paths.join(", ")));
        }
        o.push('\n');

        if self.errors.is_empty() {
            o.push_str("No error events were recorded for this run.\n\n");
        }
        for (i, b) in self.errors.iter().enumerate() {
            o.push_str(&format!("## Error {} — {} ({})\n\n", i + 1, b.error_code, b.triage.label()));
            if let Some(r) = &b.resource {
                o.push_str(&format!("- resource: `{r}`\n"));
            }
            if let Some(f) = &b.field_path {
                o.push_str(&format!("- field: `{f}`\n"));
            }
            o.push_str(&format!("- message: {}\n", b.message));
            if let Some(c) = &b.cause {
                o.push_str(&format!("- cause: {c}\n"));
            }
            o.push_str(&format!("- fix: {}\n", b.fix));
            o.push_str(&format!("- triage: {} — {}\n", b.triage.label(), b.triage_guidance));
            if let Some(t) = &b.trace_id {
                o.push_str(&format!("- trace_id: `{t}`\n"));
            }
            if let Some(sp) = &b.span {
                o.push_str(&format!("- span: `{sp}`\n"));
            }
            if let Some(src) = &b.src {
                o.push_str(&format!("- src: `{src}`\n"));
            }
            if !b.error_chain.is_empty() {
                o.push_str(&format!("- error_chain: {}\n", b.error_chain.join(" ⇐ ")));
            }
            if let Some(ex) = &b.exchange {
                o.push_str("\n### Failing exchange (redacted)\n\n```json\n");
                o.push_str(&serde_json::to_string_pretty(ex).unwrap_or_default());
                o.push_str("\n```\n");
            }
            if let Some(bt) = &b.backtrace {
                o.push_str("\n### Backtrace\n\n```\n");
                o.push_str(bt);
                o.push_str("\n```\n");
            }
            if !b.preceding.is_empty() {
                o.push_str("\n### Preceding events\n\n");
                for p in &b.preceding {
                    o.push_str(&format!("- {p}\n"));
                }
            }
            o.push('\n');
        }

        if !self.troubleshoot.is_empty() {
            o.push_str("## Matched troubleshooting docs\n\n");
            for m in &self.troubleshoot {
                o.push_str(&format!("- **{}** (`{}`) — matched on: {}\n", m.title, m.path, m.matched_on.join(", ")));
            }
            o.push('\n');
        }

        o.push_str("## Fix instructions\n\n");
        o.push_str(&self.fix_instructions);
        o
    }

    /// Machine form: the whole bundle as JSON.
    pub fn render_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::logging::run_record::{RunCounts, RunManifest};

    fn ev(target: &str, fields: &[(&str, &str)]) -> EventLine {
        let mut obj = serde_json::Map::new();
        obj.insert("ts".into(), "t".into());
        obj.insert("level".into(), "ERROR".into());
        obj.insert("target".into(), target.into());
        for (k, v) in fields {
            obj.insert((*k).into(), Value::String((*v).into()));
        }
        super::super::artifact::event_line_from_value(&Value::Object(obj))
    }

    fn artifact_with(events: Vec<EventLine>) -> RunArtifact {
        let manifest = RunManifest {
            run_id: "r1".into(),
            command: "apply".into(),
            args: vec![],
            profile: None,
            deployment: None,
            config_paths: vec!["c.yaml".into()],
            started: "s".into(),
            finished: Some("f".into()),
            outcome: Some("failed".into()),
            counts: RunCounts::default(),
            errors: vec![],
            full_trace: false,
            record_incomplete: false,
        };
        RunArtifact { run_id: "r1".into(), dir: std::path::PathBuf::new(), manifest, events }
    }

    #[test]
    fn bundle_has_code_fix_triage_and_exchange() {
        let _env = crate::test_env_lock();
        // Disable troubleshoot matching to keep the test hermetic.
        unsafe { std::env::set_var("WXCTL_TROUBLESHOOT_DIR", std::env::temp_dir().join("wxctl-no-such-ts-dir")) };
        let decision = ev("wxctl::decision", &[("decision", "create"), ("resource_type", "space"), ("resource_name", "dev"), ("reason", "absent")]);
        let error = ev(
            "wxctl::error",
            &[("error_code", "WXCTL-H001"), ("resource_type", "space"), ("resource_name", "dev"), ("message", "HTTP 404 instance not found"), ("fix", "check the instance guid"), ("cause", "not found"), ("context", r#"{"request_body":{"name":"dev"},"response_body":{"error":"not found"}}"#)],
        );
        let art = artifact_with(vec![decision, error]);
        let bundle = build_bundle(&art);

        assert_eq!(bundle.errors.len(), 1);
        let b = &bundle.errors[0];
        assert_eq!(b.error_code, "WXCTL-H001");
        assert_eq!(b.fix, "check the instance guid");
        assert_eq!(b.triage, TriageClass::ConfigEnv);
        assert_eq!(b.resource.as_deref(), Some("space.dev"));
        let ex = b.exchange.as_ref().expect("exchange decoded from context");
        assert_eq!(ex.response_body.get("error").and_then(Value::as_str), Some("not found"));
        assert_eq!(b.preceding.len(), 1, "the preceding decision event is captured");

        let md = bundle.render_markdown();
        assert!(md.contains("WXCTL-H001"));
        assert!(md.contains("config/env"));
        assert!(md.contains("Failing exchange"));
        assert!(md.contains("## Fix instructions"));

        let json = bundle.render_json();
        assert_eq!(json["errors"][0]["triage"], "config-env");
        unsafe { std::env::remove_var("WXCTL_TROUBLESHOOT_DIR") };
    }

    #[test]
    fn suspected_bug_adds_full_trace_escalation_to_fix_tail() {
        let _env = crate::test_env_lock();
        unsafe { std::env::set_var("WXCTL_TROUBLESHOOT_DIR", std::env::temp_dir().join("wxctl-no-such-ts-dir")) };
        let panic_ev = ev("wxctl::error", &[("error_code", "WXCTL-P001"), ("message", "panicked at x"), ("fix", "wxctl source bug"), ("backtrace", "frame0\nframe1")]);
        let art = artifact_with(vec![panic_ev]);
        let bundle = build_bundle(&art);
        assert_eq!(bundle.errors[0].triage, TriageClass::SuspectedWxctlBug);
        assert!(bundle.errors[0].backtrace.is_some());
        assert!(bundle.fix_instructions.contains("--full-trace"));
        unsafe { std::env::remove_var("WXCTL_TROUBLESHOOT_DIR") };
    }
}
