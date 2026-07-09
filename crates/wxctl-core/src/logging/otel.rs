//! Optional OpenTelemetry tracing layer.
//!
//! Activated by environment variables (checked in this order):
//! - `OTEL_EXPORTER_OTLP_ENDPOINT`: export via OTLP/gRPC (batched).
//! - `WXCTL_OTEL_FILE`: export spans as JSON Lines to the named file (one JSON
//!   object per span, written synchronously as each span ends).
//!
//! The layer carries its own `EnvFilter` (default `info`, wxctl crates `debug`,
//! plus the HTTP substage at `trace` so per-request spans are exported). All
//! spans nest under the per-command root `run` span, so an exported run is a
//! single coherent trace.

use opentelemetry::trace::TracerProvider as _;
use opentelemetry::{KeyValue, SpanId};
use opentelemetry_otlp::WithExportConfig;
use opentelemetry_sdk::Resource;
use opentelemetry_sdk::error::{OTelSdkError, OTelSdkResult};
use opentelemetry_sdk::trace::{SdkTracer, SdkTracerProvider, SpanData, SpanExporter};
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{Mutex, OnceLock};
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer as _;
use tracing_subscriber::filter::Filtered;
use tracing_subscriber::registry::LookupSpan;

/// Holds the active provider so `shutdown` can flush pending spans on exit.
/// opentelemetry 0.30+ removed the global `shutdown_tracer_provider`, so the
/// caller keeps a provider handle and shuts it down explicitly.
static PROVIDER: OnceLock<Mutex<Option<SdkTracerProvider>>> = OnceLock::new();

/// Layer-level filter: default `info`; wxctl crates at `debug`; the HTTP
/// substage at `trace` so the per-request `http_request` span (emitted with
/// `trace_span!`) reaches the exporter. Without the trace directive the
/// semconv HTTP spans would be filtered out before export.
const OTEL_FILTER: &str = "info,wxctl=debug,wxctl_core=debug,wxctl_engine=debug,wxctl_providers=debug,wxctl::substage::http=trace";

/// Type alias to avoid clippy `type_complexity` on the return type.
pub type OtelLayer<S> = Filtered<OpenTelemetryLayer<S, SdkTracer>, EnvFilter, S>;

fn resource() -> Resource {
    Resource::builder().with_service_name("wxctl").with_attribute(KeyValue::new("service.version", env!("CARGO_PKG_VERSION"))).build()
}

/// A `SpanExporter` that serializes each span to a one-line JSON object and
/// appends it to a file (JSON Lines). Synchronous; used via `SimpleSpanProcessor`.
#[derive(Debug)]
struct JsonFileExporter {
    file: Mutex<File>,
}

impl JsonFileExporter {
    fn new(path: &str) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).write(true).truncate(true).open(path)?;
        Ok(Self { file: Mutex::new(file) })
    }
}

fn span_to_json(span: &SpanData) -> serde_json::Value {
    let attrs: serde_json::Map<String, serde_json::Value> = span.attributes.iter().map(|kv| (kv.key.as_str().to_string(), serde_json::Value::String(kv.value.to_string()))).collect();
    let parent = if span.parent_span_id == SpanId::INVALID { serde_json::Value::Null } else { serde_json::Value::String(span.parent_span_id.to_string()) };
    let start = span.start_time.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let end = span.end_time.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    serde_json::json!({
        "name": span.name.as_ref(),
        "trace_id": span.span_context.trace_id().to_string(),
        "span_id": span.span_context.span_id().to_string(),
        "parent_span_id": parent,
        "kind": format!("{:?}", span.span_kind),
        "status": format!("{:?}", span.status),
        "start_unix_s": start,
        "end_unix_s": end,
        "attributes": attrs,
    })
}

impl SpanExporter for JsonFileExporter {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let mut file = self.file.lock().map_err(|e| OTelSdkError::InternalFailure(format!("OTel file exporter mutex poisoned: {e}")))?;
        for span in &batch {
            let line = serde_json::to_string(&span_to_json(span)).map_err(|e| OTelSdkError::InternalFailure(format!("OTel span JSON serialization failed: {e}")))?;
            writeln!(file, "{line}").map_err(|e| OTelSdkError::InternalFailure(format!("OTel file write failed: {e}")))?;
        }
        file.flush().map_err(|e| OTelSdkError::InternalFailure(format!("OTel file flush failed: {e}")))?;
        Ok(())
    }
}

/// Build an optional OTel tracing layer wrapped in its own `EnvFilter`.
/// Returns `None` if no OTel env vars are set, if the OTLP exporter cannot be
/// built from `OTEL_EXPORTER_OTLP_ENDPOINT`, or if `WXCTL_OTEL_FILE` names a
/// path that cannot be opened (a one-time eprintln warns; the command continues).
pub fn otel_layer<S>() -> Option<OtelLayer<S>>
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    let provider = if let Ok(endpoint) = std::env::var("OTEL_EXPORTER_OTLP_ENDPOINT") {
        match opentelemetry_otlp::SpanExporter::builder().with_tonic().with_endpoint(&endpoint).build() {
            Ok(exporter) => SdkTracerProvider::builder().with_batch_exporter(exporter).with_resource(resource()).build(),
            Err(e) => {
                eprintln!("warning: OTEL_EXPORTER_OTLP_ENDPOINT='{endpoint}' could not build the OTLP span exporter ({e}); OTel export disabled");
                return None;
            }
        }
    } else if let Ok(path) = std::env::var("WXCTL_OTEL_FILE") {
        match JsonFileExporter::new(&path) {
            Ok(exporter) => SdkTracerProvider::builder().with_simple_exporter(exporter).with_resource(resource()).build(),
            Err(e) => {
                eprintln!("warning: WXCTL_OTEL_FILE='{path}' could not be opened ({e}); OTel export disabled");
                return None;
            }
        }
    } else {
        return None;
    };

    let tracer = provider.tracer("wxctl");
    PROVIDER.get_or_init(|| Mutex::new(None)).lock().expect("OTel provider mutex poisoned").replace(provider);
    Some(tracing_opentelemetry::layer().with_tracer(tracer).with_filter(EnvFilter::new(OTEL_FILTER)))
}

/// Shutdown OpenTelemetry tracer (flush pending spans).
pub fn shutdown() {
    if let Some(provider) = PROVIDER.get().and_then(|m| m.lock().expect("OTel provider mutex poisoned").take()) {
        let _ = provider.shutdown();
    }
}
