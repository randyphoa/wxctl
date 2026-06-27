//! Logging macros for structured event emission
//!
//! These macros wrap tracing calls to ensure consistent field naming and targeting.
//! Stages use `info_span!` directly for automatic lifecycle tracking.
//! Events (decisions, dependencies, errors) use these macros for simplicity.

/// Log reconciliation decision
#[macro_export]
macro_rules! log_decision {
    ($op_id:expr, $res_type:expr, $res_name:expr, $decision:expr, $reason:expr) => {
        tracing::info!(
            target: "wxctl::decision",
            operation_id = %$op_id,
            resource_type = %$res_type,
            resource_name = %$res_name,
            decision = %$decision,
            reason = %$reason,
            changed_fields = "",
            "Decision: {} for {}.{}", $decision, $res_type, $res_name
        );
    };
    ($op_id:expr, $res_type:expr, $res_name:expr, $decision:expr, $reason:expr, $changed_fields:expr) => {
        tracing::info!(
            target: "wxctl::decision",
            operation_id = %$op_id,
            resource_type = %$res_type,
            resource_name = %$res_name,
            decision = %$decision,
            reason = %$reason,
            changed_fields = %$changed_fields,
            "Decision: {} for {}.{}", $decision, $res_type, $res_name
        );
    };
}

/// Log dependency resolution (resolved)
#[macro_export]
macro_rules! log_dependency_resolved {
    ($op_id:expr, $res_type:expr, $res_name:expr, $dep_type:expr, $dep_name:expr, $resolved_id:expr) => {
        tracing::debug!(
            target: "wxctl::dependency",
            operation_id = %$op_id,
            resource_type = %$res_type,
            resource_name = %$res_name,
            depends_on_type = %$dep_type,
            depends_on_name = %$dep_name,
            status = "resolved",
            resolved_id = %$resolved_id,
            "Dependency resolved: {}.{} -> {}", $dep_type, $dep_name, $resolved_id
        );
    };
}

/// Log dependency resolution (deferred)
#[macro_export]
macro_rules! log_dependency_deferred {
    ($op_id:expr, $res_type:expr, $res_name:expr, $dep_type:expr, $dep_name:expr) => {
        tracing::debug!(
            target: "wxctl::dependency",
            operation_id = %$op_id,
            resource_type = %$res_type,
            resource_name = %$res_name,
            depends_on_type = %$dep_type,
            depends_on_name = %$dep_name,
            status = "deferred",
            "Dependency deferred: {}.{}", $dep_type, $dep_name
        );
    };
}

/// Log error with context (no resource)
#[macro_export]
macro_rules! log_error {
    ($op_id:expr, $stage:expr, $code:expr, $msg:expr, $fix:expr) => {
        tracing::error!(
            target: "wxctl::error",
            operation_id = %$op_id,
            stage = %$stage,
            error_code = %$code,
            message = %$msg,
            fix = %$fix,
            "Error in {}: {}", $stage, $msg
        );
    };
}

/// Log error with resource context
#[macro_export]
macro_rules! log_error_resource {
    ($op_id:expr, $stage:expr, $code:expr, $res_type:expr, $res_name:expr, $msg:expr, $fix:expr) => {
        tracing::error!(
            target: "wxctl::error",
            operation_id = %$op_id,
            stage = %$stage,
            error_code = %$code,
            resource_type = %$res_type,
            resource_name = %$res_name,
            message = %$msg,
            fix = %$fix,
            "Error in {} for {}.{}: {}", $stage, $res_type, $res_name, $msg
        );
    };
}

/// Log error with field context
#[macro_export]
macro_rules! log_error_field {
    ($op_id:expr, $stage:expr, $code:expr, $res_type:expr, $res_name:expr, $field_path:expr, $msg:expr, $fix:expr) => {
        tracing::error!(
            target: "wxctl::error",
            operation_id = %$op_id,
            stage = %$stage,
            error_code = %$code,
            resource_type = %$res_type,
            resource_name = %$res_name,
            field_path = %$field_path,
            message = %$msg,
            fix = %$fix,
            "Error in {} for {}.{} field '{}': {}", $stage, $res_type, $res_name, $field_path, $msg
        );
    };
}

/// Log non-fatal warning with resource + field context (soft validation, advisory).
#[macro_export]
macro_rules! log_warn_resource_field {
    ($code:expr, $res_type:expr, $res_name:expr, $field_path:expr, $value:expr, $known_values:expr, $msg:expr) => {
        tracing::warn!(
            target: "wxctl::warning",
            error_code = %$code,
            resource_type = %$res_type,
            resource_name = %$res_name,
            field_path = %$field_path,
            value = %$value,
            known_values = ?$known_values,
            "{}: {}", $code, $msg
        );
    };
}

/// Log a single compact HTTP exchange line. `$req_body`/`$resp_body` are the
/// **already-redacted** request/response JSON values (the client redacts before
/// calling this). One canonical compact-JSON form per field — no Debug duplicate.
#[macro_export]
macro_rules! log_http_request {
    ($op_id:expr, $req_id:expr, $method:expr, $url:expr, $status:expr, $req_body:expr, $resp_body:expr) => {
        tracing::trace!(
            target: "wxctl::substage::http",
            operation_id = %$op_id,
            request_id = %$req_id,
            method = %$method,
            url = %$url,
            status = $status,
            request_json = %::serde_json::to_string($req_body).unwrap_or_default(),
            response_json = %::serde_json::to_string($resp_body).unwrap_or_default(),
            "HTTP {} {} -> {}", $method, $url, $status
        );
    };
}

/// Log error caused by upstream dependency failure (cascading)
#[macro_export]
macro_rules! log_error_cascade {
    ($op_id:expr, $stage:expr, $code:expr, $res_type:expr, $res_name:expr, $msg:expr, $fix:expr, $caused_by:expr) => {
        tracing::error!(
            target: "wxctl::error",
            operation_id = %$op_id,
            stage = %$stage,
            error_code = %$code,
            resource_type = %$res_type,
            resource_name = %$res_name,
            message = %$msg,
            fix = %$fix,
            caused_by = %$caused_by,
            "{}", $msg
        );
    };
}

/// Log error with cause, expected/actual, and context snapshot
#[macro_export]
macro_rules! log_error_context {
    ($op_id:expr, $stage:expr, $code:expr, $res_type:expr, $res_name:expr, $msg:expr, $fix:expr,
     cause: $cause:expr, expected: $expected:expr, actual: $actual:expr, context: $ctx:expr) => {
        tracing::error!(
            target: "wxctl::error",
            operation_id = %$op_id,
            stage = %$stage,
            error_code = %$code,
            resource_type = %$res_type,
            resource_name = %$res_name,
            message = %$msg,
            fix = %$fix,
            cause = %$cause,
            expected = %$expected,
            actual = %$actual,
            context = %$ctx,
            "{}", $msg
        );
    };
}

/// Log operation summary at end of command
#[macro_export]
macro_rules! log_summary {
    ($op_id:expr, $total:expr, $created:expr, $updated:expr, $deleted:expr,
     $noop:expr, $retained:expr, $failed:expr, $skipped:expr,
     $skipped_absent:expr, $skipped_deferred:expr,
     $duration_ms:expr, $errors:expr) => {
        tracing::info!(
            target: "wxctl::summary",
            operation_id = %$op_id,
            total = $total,
            created = $created,
            updated = $updated,
            deleted = $deleted,
            noop = $noop,
            retained = $retained,
            failed = $failed,
            skipped = $skipped,
            skipped_absent = $skipped_absent,
            skipped_deferred = $skipped_deferred,
            duration_ms = $duration_ms,
            error_codes = %$errors,
            "Operation complete: {}/{} succeeded, {} failed",
            $total - $failed - $skipped, $total, $failed
        );
    };
}
