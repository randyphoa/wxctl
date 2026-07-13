//! Cross-resource validators — rules that depend on more than one
//! resource's fields at once.
//!
//! The per-resource validator in `schema.rs` checks each resource in
//! isolation. Some invariants require reading a linked resource's fields: for
//! example, an `s3_bucket`'s `storage_class` enum depends on the `type:` of
//! the `storage_connection` it references. Those rules live here.
//!
//! Validators run after Stage 4 of the pipeline (per-resource schema checks
//! pass, dependencies are extracted) and emit `WXCTL-V503`.
//!
//! Adding a new validator: implement a function returning
//! `Vec<AnnotatedValidationError>` and register it in `run_all`.

use super::error_codes;
use super::types::{AnnotatedValidationError, ValidationError};
use crate::resource::ValidatedResource;
use std::collections::HashMap;
use wxctl_graph::{ResourceKey, parse_reference};

/// Run every registered cross-resource validator over the validated set.
/// Each returned error is annotated with its source resource label so the
/// pipeline can log and surface it alongside per-resource errors.
pub fn run_all(resources: &[ValidatedResource]) -> Vec<AnnotatedValidationError> {
    let index = IndexedResources::new(resources);
    let mut errors = Vec::new();
    errors.extend(validate_s3_bucket_storage_class(&index));
    errors
}

/// Ergonomic lookup over the validated-resource list. Cross-resource
/// validators read a field on a linked resource by ref_name; this wrapper
/// encapsulates the `${kind.name}` / bare-name parsing plus the key lookup.
struct IndexedResources<'a> {
    by_key: HashMap<ResourceKey, &'a ValidatedResource>,
}

impl<'a> IndexedResources<'a> {
    fn new(resources: &'a [ValidatedResource]) -> Self {
        let by_key = resources.iter().map(|r| (r.key.clone(), r)).collect();
        Self { by_key }
    }

    fn iter_kind<'b>(&'b self, kind: &'b str) -> impl Iterator<Item = &'a ValidatedResource> + 'b {
        self.by_key.values().copied().filter(move |r| r.key.kind.as_ref() == kind)
    }

    /// Resolve a reference-field value to the linked resource. The value may be
    /// a `${kind.name}` template or a bare ref_name string. Returns None when
    /// the field is absent, not a string, or the target resource is not in the
    /// planning closure.
    fn resolve<'b>(&'b self, source: &ValidatedResource, field: &str, target_kind: &str) -> Option<&'b ValidatedResource> {
        let raw = source.data.get(field)?.as_str()?;
        let key = match parse_reference(raw) {
            Some(k) => k,
            None => ResourceKey::new(target_kind, raw),
        };
        if key.kind.as_ref() != target_kind {
            return None;
        }
        self.by_key.get(&key).copied()
    }
}

/// `WXCTL-V503` — `s3_bucket.storage_class` must match the backend family
/// implied by the linked `storage_connection.type`. `ibm_cos` accepts the IBM
/// COS class list; AWS-family types accept the AWS class list; MinIO / Ceph /
/// other backends use free-form strings (no check). Free-form connection
/// types (including soft-unknown values) are silently permitted so validation
/// stays forward-compatible with new backends.
fn validate_s3_bucket_storage_class(index: &IndexedResources<'_>) -> Vec<AnnotatedValidationError> {
    const IBM_COS_CLASSES: &[&str] = &["smart", "standard", "vault", "cold", "onerate_active"];
    const AWS_S3_CLASSES: &[&str] = &["STANDARD", "STANDARD_IA", "ONEZONE_IA", "GLACIER", "DEEP_ARCHIVE", "INTELLIGENT_TIERING"];

    let mut errors = Vec::new();
    for bucket in index.iter_kind("s3_bucket") {
        let Some(storage_class) = bucket.data.get("storage_class").and_then(|v| v.as_str()) else { continue };
        let Some(conn) = index.resolve(bucket, "connection", "storage_connection") else { continue };
        let Some(conn_type) = conn.data.get("type").and_then(|v| v.as_str()) else { continue };

        let allowed: &[&str] = match conn_type {
            "ibm_cos" => IBM_COS_CLASSES,
            "aws_s3" | "amazon_s3" | "s3" => AWS_S3_CLASSES,
            _ => continue, // MinIO / Ceph / unknown — free-form, no check.
        };

        if !allowed.contains(&storage_class) {
            let label = format!("{}/{}", bucket.key.kind, bucket.key.name);
            let msg = format!("[{}] WXCTL-V503: storage_class '{}' is not valid for storage_connection.type '{}' (allowed: [{}])", error_codes::V503, storage_class, conn_type, allowed.join(", "));
            errors.push(AnnotatedValidationError { resource: label, error: ValidationError::InvalidFieldValue { field: "storage_class".to_string(), message: msg } });
        }
    }
    errors
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::descriptor::ResourceDescriptor;
    use crate::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, HookDefinition, HttpMethod, ReconciliationDefinition, ResourceDefinition, ResourceSchema, SchemaDefinition, UpdateStrategy};
    use serde_json::json;
    use std::sync::Arc;

    fn make_descriptor(kind: &str) -> Arc<ResourceDescriptor> {
        let schema = ResourceSchema {
            resource: ResourceDefinition {
                name: kind.into(),
                service: "test".into(),
                kind: kind.into(),
                version: "v1".into(),
                api: ApiDefinition {
                    base_path: "/x".into(),
                    id_field: "id".into(),
                    list_endpoint: None,
                    get_endpoint: "/x/{id}".into(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: None,
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                    readiness: None,
                },
                schema: SchemaDefinition::default(),
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::Skip, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, list_map: false, id_source: "id".into() },
                    state_fields: Some(vec![]),
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec![],
                    reject_on_immutable_drift: false,
                    use_json_patch: false,
                    json_patch_path_prefix: None,
                    identity_hash: None,
                },
                hooks: HookDefinition::default(),
                deployments: None,
                unsupported_on: vec![],
                description: None,
                prompt: None,
            },
        };
        Arc::new(ResourceDescriptor::from_schema(&schema).unwrap())
    }

    fn resource(kind: &str, name: &str, data: serde_json::Value) -> ValidatedResource {
        ValidatedResource { key: ResourceKey::new(kind, name), data, descriptor: make_descriptor(kind), dependencies: vec![], on_destroy: Default::default() }
    }

    /// Build a [storage_connection, s3_bucket] pair. `bucket` is the s3_bucket data.
    fn pair(conn_type: &str, bucket: serde_json::Value) -> Vec<ValidatedResource> {
        vec![resource("storage_connection", "c1", json!({ "type": conn_type })), resource("s3_bucket", "b1", bucket)]
    }

    #[test]
    fn storage_class_compatibility_accepts() {
        // Each row: (connection type, storage_class) that must validate clean.
        let cases: &[(&str, &str)] = &[
            ("ibm_cos", "smart"),               // IBM COS class on IBM COS
            ("aws_s3", "STANDARD"),             // AWS class on AWS S3
            ("minio", "freeform_custom_class"), // MinIO accepts any freeform class
        ];
        for (conn_type, class) in cases {
            let errs = run_all(&pair(conn_type, json!({"connection": "c1", "storage_class": class})));
            assert!(errs.is_empty(), "{conn_type} + {class} should pass, got {errs:?}");
        }
    }

    #[test]
    fn storage_class_compatibility_rejects_v503() {
        // Mismatched class for the connection type → exactly one V503.
        // ibm_cos row also asserts the message names the offending class + type.
        let ibm = run_all(&pair("ibm_cos", json!({"connection": "c1", "storage_class": "STANDARD"})));
        assert_eq!(ibm.len(), 1);
        assert!(ibm[0].error.to_string().contains("WXCTL-V503"));
        assert!(ibm[0].error.to_string().contains("STANDARD"));
        assert!(ibm[0].error.to_string().contains("ibm_cos"));

        // aws_s3 with an IBM class is the symmetric reject.
        let aws = run_all(&pair("aws_s3", json!({"connection": "c1", "storage_class": "smart"})));
        assert_eq!(aws.len(), 1);
        assert!(aws[0].error.to_string().contains("WXCTL-V503"));

        // A `${kind.name}` template reference must still route the V503 check.
        let templ = run_all(&pair("ibm_cos", json!({"connection": "${storage_connection.c1}", "storage_class": "STANDARD"})));
        assert_eq!(templ.len(), 1, "template ref must still route the V503 check");
    }

    #[test]
    fn storage_class_check_silent_when_unresolvable() {
        // Broken reference — Stage 4 of the pipeline reports V005; cross-resource
        // validators must not double-report the same failure.
        let broken = vec![resource("s3_bucket", "b1", json!({"connection": "nonexistent", "storage_class": "smart"}))];
        assert!(run_all(&broken).is_empty());

        // Field has a default ("smart"); if somehow missing, no false positive.
        assert!(run_all(&pair("ibm_cos", json!({"connection": "c1"}))).is_empty());
    }
}
