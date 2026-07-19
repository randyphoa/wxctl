//! Namespaced error codes for structured, grep-able error classification.
//!
//! Format: WXCTL-{STAGE_LETTER}{NUMBER}
//! - C: Configuration / profile loading
//! - V: Validation
//! - R: Reconciliation
//! - E: Execution
//! - H: HTTP/Network
//! - T: Template

// Configuration / profile loading
pub const C001: &str = "WXCTL-C001"; // Duplicate service block within a single profile

// Validation
pub const V001: &str = "WXCTL-V001"; // Duplicate resource names
pub const V002: &str = "WXCTL-V002"; // Unknown resource kind
pub const V003: &str = "WXCTL-V003"; // Schema validation failed
pub const V004: &str = "WXCTL-V004"; // Field normalization conflict
pub const V005: &str = "WXCTL-V005"; // Invalid dependency reference
pub const V006: &str = "WXCTL-V006"; // Circular dependency detected
pub const V007: &str = "WXCTL-V007"; // Post-validation hook failed
pub const V008: &str = "WXCTL-V008"; // ID dereferencing failed
pub const V009: &str = "WXCTL-V009"; // Invalid on_destroy value
pub const V301: &str = "WXCTL-V301"; // Env-var interpolation: missing/empty
pub const V302: &str = "WXCTL-V302"; // Env-var interpolation: malformed expression
pub const V401: &str = "WXCTL-V401"; // Schema validation: soft-allowed value outside known set or variant-inactive field (warn)
pub const V402: &str = "WXCTL-V402"; // Variant-scoped required field missing
pub const V403: &str = "WXCTL-V403"; // Python tool carries redundant inline input_schema/output_schema — schema.yaml is authoritative (warn)
pub const V501: &str = "WXCTL-V501"; // Cross-field oneOf violation (neither or both set)
pub const V503: &str = "WXCTL-V503"; // Cross-resource validator (e.g. storage_class enum depends on linked connection.type)
pub const V504: &str = "WXCTL-V504"; // Readiness-contract authoring error: require_ready target missing api.readiness, or malformed readiness block
pub const V505: &str = "WXCTL-V505"; // Orphaned one-sided bridge: counterpart kind absent (warn, advisory)

// Reconciliation
pub const R001: &str = "WXCTL-R001"; // Remote discovery failed
pub const R002: &str = "WXCTL-R002"; // Dependency not found remotely
pub const R003: &str = "WXCTL-R003"; // Reconciliation conflict
pub const R004: &str = "WXCTL-R004"; // Resource kind unsupported on active deployment
pub const R005: &str = "WXCTL-R005"; // Immutable-field drift rejected (reject_on_immutable_drift)
pub const R006: &str = "WXCTL-R006"; // requires.deployment not satisfied by active deployment
pub const R501: &str = "WXCTL-R501"; // Cross-type name collision at discovery: same name, different list_filter value (warn, advisory)

// Execution
pub const E001: &str = "WXCTL-E001"; // Create operation failed
pub const E002: &str = "WXCTL-E002"; // Update operation failed
pub const E003: &str = "WXCTL-E003"; // Delete operation failed
pub const E004: &str = "WXCTL-E004"; // Skipped due to dependency failure
pub const E005: &str = "WXCTL-E005"; // Rate limit exceeded

// HTTP
pub const H001: &str = "WXCTL-H001"; // HTTP 4xx client error
pub const H002: &str = "WXCTL-H002"; // HTTP 5xx server error
pub const H003: &str = "WXCTL-H003"; // Connection/timeout error
pub const H004: &str = "WXCTL-H004"; // Authentication failed (401/403)
pub const H601: &str = "WXCTL-H601"; // Idempotent 404 tolerated (resource already gone)

// Cloud Object Storage (S3 REST API)
pub const H700: &str = "WXCTL-H700"; // COS bucket create failed (generic S3 error wrapper)
pub const H701: &str = "WXCTL-H701"; // COS bucket delete rejected — non-empty and force_destroy=false
pub const H702: &str = "WXCTL-H702"; // COS bucket force-destroy exceeded object cap
pub const H703: &str = "WXCTL-H703"; // COS object source invalid (content/path constraints or file too large)
pub const H704: &str = "WXCTL-H704"; // COS instance auto-discovery ambiguous
pub const H705: &str = "WXCTL-H705"; // COS bucket exists in a different region
pub const H706: &str = "WXCTL-H706"; // COS bucket name taken by another IBM Cloud account
pub const H707: &str = "WXCTL-H707"; // COS bucket exists but is owned by a different account
pub const H708: &str = "WXCTL-H708"; // COS credentials rejected (invalid access key / bad signature)
pub const H710: &str = "WXCTL-H710"; // watsonx.data storage_registration: bucket already registered under a different catalog
pub const H711: &str = "WXCTL-H711"; // register-only bucket kind: linked storage_connection.type is the wrong family

// Handler deferral — schemas ship, handlers don't
pub const H900: &str = "WXCTL-H900"; // Handler not implemented in this build (ADLS, GCS, HDFS deferred)
pub const H901: &str = "WXCTL-H901"; // factsheets model_tracking: invalid model scope / missing required field

// Template
pub const T001: &str = "WXCTL-T001"; // Template resolution failed
pub const T002: &str = "WXCTL-T002"; // Circular template reference
