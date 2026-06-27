use crate::output::OutputCollector;
use crate::output::field_visitor::FieldCollector;
use crate::output::span_ext::{SpanMetadata, SpanType};
use parking_lot::Mutex;
use std::sync::{Arc, OnceLock};
use tracing::{Event, Id, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;
use wxctl_core::logging::*;

/// Filter directive that gates which events reach the OutputCollectorLayer.
/// Matches every wxctl-owned crate at trace level so the CLI renderer always
/// sees stage / substage / decision / dependency / error events regardless
/// of the operator's RUST_LOG.
pub const COLLECTOR_FILTER: &str = "wxctl::stage=trace,wxctl::substage=trace,wxctl::decision=trace,wxctl::dependency=trace,wxctl::error=trace,wxctl::summary=trace";

// Global slot holding the active per-command collector. The layer reads from
// this at runtime; install_collector populates it for the duration of a
// command, the returned guard clears it on drop. Keeping the layer attached
// to the global subscriber (instead of swapping subscribers per command)
// means the JSON file layer set up by main.rs stays active during commands.
static CURRENT_COLLECTOR: OnceLock<Mutex<Option<Arc<Mutex<OutputCollector>>>>> = OnceLock::new();

fn slot() -> &'static Mutex<Option<Arc<Mutex<OutputCollector>>>> {
    CURRENT_COLLECTOR.get_or_init(|| Mutex::new(None))
}

fn current_collector() -> Option<Arc<Mutex<OutputCollector>>> {
    slot().lock().clone()
}

pub struct CollectorGuard;

impl Drop for CollectorGuard {
    fn drop(&mut self) {
        *slot().lock() = None;
    }
}

pub fn install_collector(collector: Arc<Mutex<OutputCollector>>) -> CollectorGuard {
    *slot().lock() = Some(collector);
    CollectorGuard
}

/// Layer that forwards stage / substage / decision / dependency / error events
/// into the active per-command OutputCollector.
#[derive(Default)]
pub struct OutputCollectorLayer;

impl<S> Layer<S> for OutputCollectorLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &tracing::span::Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let target = attrs.metadata().target();

        // Guard: root run span should not be rendered as a progress stage spinner.
        if attrs.metadata().name() == "run" && target == "wxctl::stage::run" {
            return;
        }

        // Determine span type from target
        let span_type = if target.starts_with("wxctl::stage") {
            SpanType::Stage
        } else if target.starts_with("wxctl::substage") {
            SpanType::Substage
        } else {
            SpanType::Other
        };

        // Only track stage and substage spans
        if span_type == SpanType::Other {
            return;
        }

        // Extract fields
        let mut visitor = FieldCollector::default();
        attrs.record(&mut visitor);

        // Create metadata
        let mut metadata = SpanMetadata::new(span_type);
        metadata.operation_id = visitor.get("operation_id").map(|s| s.to_string());
        metadata.resource_type = visitor.get("resource_type").map(|s| s.to_string());
        metadata.resource_name = visitor.get("resource_name").map(|s| s.to_string());
        metadata.resource_count = visitor.get("resource_count").and_then(|s| s.parse().ok());

        // Stage/substage rendering: lock briefly to mutate state and capture a
        // render plan, drop the lock, then run indicatif outside it. Holding
        // the collector mutex across `MultiProgress::add` / `println` /
        // `enable_steady_tick` deadlocks against indicatif's own state lock.
        if let Some(collector_arc) = current_collector() {
            if span_type == SpanType::Stage
                && let Some(ref operation_id) = metadata.operation_id
            {
                let resource_count = metadata.resource_count.unwrap_or(0);
                let stage_name = attrs.metadata().name().to_string();
                let event = StageEvent { operation_id: operation_id.clone(), stage: stage_name.clone(), status: "started".to_string(), resource_count, duration_ms: None };
                let plan = collector_arc.lock().add_stage_state(event);
                let pb = plan.execute();
                collector_arc.lock().install_stage_spinner_pb(pb, stage_name == "execution");
                // Prefill the ▌ Execution skeleton (one dim `pending` row per resource) so the
                // full scope shows up front. Lock-safe split: build the plan under the lock,
                // run the `multi.add` calls outside it, then store the bars back under the lock.
                if stage_name == "execution" {
                    let prefill = collector_arc.lock().prefill_exec_rows_plan();
                    let installed = prefill.execute();
                    collector_arc.lock().install_prefilled_rows(installed);
                }
            } else if span_type == SpanType::Substage
                && let (Some(resource_type), Some(resource_name)) = (&metadata.resource_type, &metadata.resource_name)
            {
                let action = attrs.metadata().name();
                let desc = format!("{} {}.{}", action, resource_type, resource_name);
                let plan = collector_arc.lock().add_substage_state(&desc, None);
                plan.execute();
            }
        }

        // Store metadata in span extensions (moves metadata)
        if let Some(span) = ctx.span(id) {
            span.extensions_mut().insert(metadata);
        }
    }

    fn on_record(&self, id: &Id, values: &tracing::span::Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut visitor = FieldCollector::default();
        values.record(&mut visitor);
        if let Some(status) = visitor.get("status")
            && let Some(metadata) = span.extensions_mut().get_mut::<SpanMetadata>()
        {
            metadata.status = Some(status.to_string());
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        // Skip span/extension/duration work when no command is active.
        let Some(collector_arc) = current_collector() else { return };

        let span = match ctx.span(&id) {
            Some(s) => s,
            None => return,
        };

        let extensions = span.extensions();
        let metadata = match extensions.get::<SpanMetadata>() {
            Some(m) => m,
            None => return,
        };

        if metadata.span_type == SpanType::Stage
            && let Some(ref operation_id) = metadata.operation_id
        {
            let resource_count = metadata.resource_count.unwrap_or(0);
            let duration_ms = metadata.duration_ms();
            let stage_name = span.metadata().name().to_string();
            let is_execution = stage_name == "execution";
            let event = StageEvent { operation_id: operation_id.clone(), stage: stage_name, status: metadata.status.clone().unwrap_or_else(|| "completed".to_string()), resource_count, duration_ms: Some(duration_ms) };
            let (plan, cleanup) = {
                let mut collector = collector_arc.lock();
                let plan = collector.add_stage_state(event);
                let cleanup = is_execution.then(|| collector.drain_execution_cleanup());
                (plan, cleanup)
            };
            let _ = plan.execute();
            if let Some(cleanup) = cleanup {
                cleanup.execute();
            }
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        // Dispatch by target before any lock or field extraction. The collector lock
        // is held by call sites like CliProgressObserver while they emit tracing
        // events (e.g. wxctl::substage::execution from log_start) — acquiring it
        // here for events that don't dispatch to a handler would re-lock the
        // non-reentrant parking_lot::Mutex on the same thread and deadlock.
        let target = event.metadata().target();
        if !(target.starts_with("wxctl::decision") || target.starts_with("wxctl::dependency") || target.starts_with("wxctl::error") || target.starts_with("wxctl::summary")) {
            return;
        }

        let Some(collector_arc) = current_collector() else { return };

        let mut visitor = FieldCollector::default();
        event.record(&mut visitor);

        // Try to get operation_id from current span context if not in event
        let operation_id = visitor.get("operation_id").map(|s| s.to_string()).or_else(|| {
            ctx.event_span(event).and_then(|span| {
                let ext = span.extensions();
                ext.get::<SpanMetadata>().and_then(|m| m.operation_id.clone())
            })
        });

        let operation_id = match operation_id {
            Some(id) => id,
            None => return,
        };

        let mut collector = collector_arc.lock();
        match target {
            t if t.starts_with("wxctl::decision") => {
                self.handle_decision_event(&mut collector, &operation_id, &visitor);
            }
            t if t.starts_with("wxctl::dependency") => {
                self.handle_dependency_event(&mut collector, &operation_id, &visitor);
            }
            t if t.starts_with("wxctl::error") => {
                self.handle_error_event(&mut collector, &operation_id, &visitor);
            }
            t if t.starts_with("wxctl::summary") => {
                // Summary events are logged to JSON output but not displayed in CLI
                // (the CLI already shows its own summary via OutputCollector)
            }
            _ => {}
        }
    }
}

// Helper methods for event handling
impl OutputCollectorLayer {
    fn handle_decision_event(&self, collector: &mut OutputCollector, operation_id: &str, visitor: &FieldCollector) {
        let resource_type = match visitor.get("resource_type") {
            Some(v) => v.to_string(),
            None => return,
        };
        let resource_name = match visitor.get("resource_name") {
            Some(v) => v.to_string(),
            None => return,
        };
        let decision = match visitor.get("decision") {
            Some(v) => v.to_string(),
            None => return,
        };
        let reason = match visitor.get("reason") {
            Some(v) => v.to_string(),
            None => return,
        };

        // Parse changed_fields from comma-separated string into FieldDiff entries
        let field_diffs = match visitor.get("changed_fields") {
            Some(v) => {
                let s = v.to_string();
                if s.is_empty() { Vec::new() } else { s.split(',').map(|f| FieldDiff { path: f.to_string(), local: serde_json::Value::Null, remote: serde_json::Value::Null }).collect() }
            }
            None => Vec::new(),
        };

        collector.add_decision(DecisionEvent { operation_id: operation_id.to_string(), resource_type, resource_name, decision, reason, field_diffs });
    }

    fn handle_dependency_event(&self, collector: &mut OutputCollector, operation_id: &str, visitor: &FieldCollector) {
        let resource_type = match visitor.get("resource_type") {
            Some(v) => v.to_string(),
            None => return,
        };
        let resource_name = match visitor.get("resource_name") {
            Some(v) => v.to_string(),
            None => return,
        };
        let depends_on_type = match visitor.get("depends_on_type") {
            Some(v) => v.to_string(),
            None => return,
        };
        let depends_on_name = match visitor.get("depends_on_name") {
            Some(v) => v.to_string(),
            None => return,
        };
        let status = match visitor.get("status") {
            Some(v) => v.to_string(),
            None => return,
        };

        collector.add_dependency(DependencyEvent {
            operation_id: operation_id.to_string(),
            resource_type,
            resource_name,
            depends_on_type,
            depends_on_name,
            status,
            resolved_id: visitor.get("resolved_id").map(|s| s.to_string()),
            deferred_reason: visitor.get("deferred_reason").map(|s| s.to_string()),
        });
    }

    fn handle_error_event(&self, collector: &mut OutputCollector, operation_id: &str, visitor: &FieldCollector) {
        let stage = match visitor.get("stage") {
            Some(v) => v.to_string(),
            None => return,
        };
        let error_code = match visitor.get("error_code") {
            Some(v) => v.to_string(),
            None => return,
        };
        let message = match visitor.get("message") {
            Some(v) => v.to_string(),
            None => return,
        };
        let fix = match visitor.get("fix") {
            Some(v) => v.to_string(),
            None => return,
        };

        collector.add_error(ErrorEvent {
            operation_id: operation_id.to_string(),
            stage,
            error_code,
            resource_type: visitor.get("resource_type").map(|s| s.to_string()),
            resource_name: visitor.get("resource_name").map(|s| s.to_string()),
            field_path: visitor.get("field_path").map(|s| s.to_string()),
            message,
            cause: visitor.get("cause").map(|s| s.to_string()),
            caused_by: visitor.get("caused_by").map(|s| s.to_string()),
            expected: visitor.get("expected").map(|s| s.to_string()),
            actual: visitor.get("actual").map(|s| s.to_string()),
            context: visitor.get("context").and_then(|s| serde_json::from_str(s).ok()),
            fix,
        });
    }
}
