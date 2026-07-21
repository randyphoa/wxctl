//! Per-deployment overlay resolution for `ResourceDefinition`.
//!
//! Picks the most-specific matching overlay key from `ResourceDefinition.deployments`,
//! deep-merges its YAML form onto the base, and re-deserializes. Also exposes
//! `is_unsupported_on` for the `unsupported_on` constraint check.

use crate::definition::ResourceDefinition;
use crate::deployment::{Deployment, select_overlay_key};
use crate::merge::deep_merge;
use anyhow::{Context, Result};

/// Resolve the effective `ResourceDefinition` for the active deployment.
/// Picks the most-specific matching overlay key from `definition.deployments`,
/// deep-merges its YAML form onto the base, and re-deserializes. When no
/// overlay matches (or the map is absent), returns the base unchanged.
pub fn effective_definition(base: &ResourceDefinition, deployment: &Deployment) -> Result<ResourceDefinition> {
    let Some(deployments) = &base.deployments else {
        return Ok(base.clone());
    };
    let key = match select_overlay_key(deployments.keys().map(String::as_str), deployment) {
        Some(k) => k.to_string(),
        None => return Ok(base.clone()),
    };
    let Some(overlay) = deployments.get(&key) else {
        return Ok(base.clone());
    };
    if overlay.is_empty() {
        return Ok(base.clone());
    }

    let mut base_yaml = serde_norway::to_value(base).context("serialize base ResourceDefinition")?;
    let mut overlay_yaml = serde_norway::Mapping::new();
    if !overlay.api.is_null() {
        overlay_yaml.insert("api".into(), overlay.api.clone());
    }
    if !overlay.schema.is_null() {
        overlay_yaml.insert("schema".into(), overlay.schema.clone());
    }
    if !overlay.reconciliation.is_null() {
        overlay_yaml.insert("reconciliation".into(), overlay.reconciliation.clone());
    }
    if !overlay.hooks.is_null() {
        overlay_yaml.insert("hooks".into(), overlay.hooks.clone());
    }
    deep_merge(&mut base_yaml, &serde_norway::Value::Mapping(overlay_yaml));
    serde_norway::from_value(base_yaml).context("re-deserialize merged ResourceDefinition")
}

/// Returns true when this resource kind is unsupported on the active deployment.
pub fn is_unsupported_on(base: &ResourceDefinition, deployment: &Deployment) -> bool {
    base.unsupported_on.iter().any(|c| deployment.matches(c))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::definition::*;
    use crate::deployment::DeploymentConstraint;
    use semver::Version;
    use std::collections::HashMap;

    fn make_base() -> ResourceDefinition {
        ResourceDefinition {
            name: "test".to_string(),
            service: "svc".to_string(),
            kind: "test_kind".to_string(),
            version: "v1".to_string(),
            api: ApiDefinition {
                base_path: "/v1/tests".to_string(),
                id_field: "id".to_string(),
                list_endpoint: None,
                get_endpoint: "/v1/tests/{id}".to_string(),
                create_endpoint: None,
                create_method: HttpMethod::Post,
                update_endpoint: None,
                update_method: Some(HttpMethod::Patch),
                delete_endpoint: None,
                delete_method: HttpMethod::Delete,
                readiness: None,
            },
            schema: SchemaDefinition { fields: vec![], ..Default::default() },
            reconciliation: ReconciliationDefinition {
                discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, list_map: false, list_filter: None, id_source: "id".to_string() },
                state_fields: None,
                update_strategy: UpdateStrategy::Patch,
                immutable_fields: vec![],
                reject_on_immutable_drift: false,
                use_json_patch: true,
                json_patch_path_prefix: None,
                identity_hash: None,
            },
            hooks: HookDefinition::default(),
            deployments: None,
            unsupported_on: vec![],
            description: None,
            prompt: None,
        }
    }

    /// Add a single-key `deployments` overlay that rewrites `api.base_path`.
    fn with_overlay(key: &str) -> ResourceDefinition {
        let mut base = make_base();
        let mut map = HashMap::new();
        let overlay_yaml: serde_norway::Value = serde_norway::from_str("base_path: /zen/v1/tests").unwrap();
        map.insert(key.to_string(), DeploymentOverlay { api: overlay_yaml, ..Default::default() });
        base.deployments = Some(map);
        base
    }

    #[test]
    fn effective_definition_overlay_selection() {
        // No deployments → base unchanged.
        assert_eq!(effective_definition(&make_base(), &Deployment::Saas).unwrap().api.base_path, "/v1/tests");

        // Matching overlay key ("saas") → overlay applied.
        assert_eq!(effective_definition(&with_overlay("saas"), &Deployment::Saas).unwrap().api.base_path, "/zen/v1/tests");

        // Non-matching overlay key ("software" vs active saas) → base unchanged.
        assert_eq!(effective_definition(&with_overlay("software"), &Deployment::Saas).unwrap().api.base_path, "/v1/tests");
    }

    #[test]
    fn is_unsupported_on_constraint_matching() {
        // Empty `unsupported_on` → supported everywhere.
        assert!(!is_unsupported_on(&make_base(), &Deployment::Saas));

        // `unsupported_on: [saas]` → blocks saas, allows software.
        let mut base = make_base();
        base.unsupported_on = vec!["saas".parse::<DeploymentConstraint>().unwrap()];
        assert!(is_unsupported_on(&base, &Deployment::Saas));
        assert!(!is_unsupported_on(&base, &Deployment::Software { version: Version::parse("5.3.0").unwrap() }));
    }
}
