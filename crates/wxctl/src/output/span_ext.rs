use std::time::Instant;

/// Metadata stored in span extensions
#[derive(Debug)]
pub struct SpanMetadata {
    pub start_time: Instant,
    pub span_type: SpanType,
    pub operation_id: Option<String>,
    pub resource_type: Option<String>,
    pub resource_name: Option<String>,
    pub resource_count: Option<usize>,
    pub status: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanType {
    Stage,    // validation, reconciliation, planning, execution
    Substage, // individual resource operations
    Other,
}

impl SpanMetadata {
    pub fn new(span_type: SpanType) -> Self {
        Self { start_time: Instant::now(), span_type, operation_id: None, resource_type: None, resource_name: None, resource_count: None, status: None }
    }

    pub fn duration_ms(&self) -> u64 {
        self.start_time.elapsed().as_millis() as u64
    }
}
