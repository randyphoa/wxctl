use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use tokio::fs;
use wxctl_core::client::error_has_status;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::util::join_all_ok;

pub struct RulesHandler;

/// Strip a trailing `/{placeholder}` path-template segment from a delete/get
/// endpoint, returning the collection base path.
///
/// The bulk `rules` kind declares no `delete_endpoint`, so the engine falls back
/// to `get_endpoint: /v3/enforcement/rules/{name}` and hands that templated path
/// to `pre_delete`. Listing + deleting must run against the BASE path
/// (`/v3/enforcement/rules`) — a literal GET on `/v3/enforcement/rules/{name}`
/// 404s (the `{name}` is never substituted for a bulk resource). Only a final
/// `{...}`-wrapped segment is stripped; a path with no template is returned
/// unchanged.
fn rules_base_endpoint(endpoint: &str) -> &str {
    let trimmed = endpoint.trim_end_matches('/');
    match trimmed.rsplit_once('/') {
        Some((base, last)) if last.starts_with('{') && last.ends_with('}') => base,
        _ => trimmed,
    }
}

impl ResourceHandler for RulesHandler {
    /// Handle bulk rule operations with two modes:
    /// 1. Inline rules: Apply per-rule reconciliation (check exists → PUT or POST)
    /// 2. Import file: Upload to import endpoint, skip reconciliation
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // Check if import_file mode is specified
            if let Some(import_file) = resource.get("import_file").and_then(|v| v.as_str()) {
                return self.handle_import(client, import_file, operation_id).await;
            }

            // Inline rules mode: apply per-rule reconciliation
            let rules = resource.get("rules").and_then(|r| r.as_array()).ok_or_else(|| anyhow!("rules requires either 'rules' array or 'import_file'"))?.clone();

            // Fetch existing rules for reconciliation (name → guid map)
            let existing_rules = self.fetch_existing_rules(client, endpoint, operation_id).await?;

            // Split rules into updates and creates
            let mut updates: Vec<(Value, String)> = Vec::new();
            let mut creates: Vec<Value> = Vec::new();

            for rule in rules {
                let rule_name = rule.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if let Some(guid) = existing_rules.get(rule_name) {
                    updates.push((rule, guid.clone()));
                } else {
                    creates.push(rule);
                }
            }

            let update_count = updates.len();
            let create_count = creates.len();

            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                update_count = update_count,
                create_count = create_count,
                "Processing rules in parallel"
            );

            let mut responses: Vec<Value> = Vec::new();

            // Execute updates in parallel
            if !updates.is_empty() {
                let update_futures = updates.iter().map(|(rule, guid)| {
                    let client = client.clone();
                    let endpoint = endpoint.to_string();
                    let op_id = operation_id.to_string();
                    let rule = rule.clone();
                    let guid = guid.clone();

                    async move {
                        tracing::debug!(
                            target: "wxctl::substage::provider",
                            operation_id = %op_id,
                            rule_name = ?rule.get("name"),
                            guid = %guid,
                            "Updating existing rule"
                        );

                        let update_endpoint = format!("{}/{}", endpoint, guid);
                        let spec = RequestSpec::new(Method::PUT, &update_endpoint).body(BodyKind::Json(rule));
                        client.execute::<Value>(&op_id, spec).await
                    }
                });
                responses.extend(join_all_ok(update_futures).await?);
            }

            // Execute creates in parallel
            if !creates.is_empty() {
                let create_futures = creates.iter().map(|rule| {
                    let client = client.clone();
                    let endpoint = endpoint.to_string();
                    let op_id = operation_id.to_string();
                    let rule = rule.clone();

                    async move {
                        tracing::debug!(
                            target: "wxctl::substage::provider",
                            operation_id = %op_id,
                            rule_name = ?rule.get("name"),
                            "Creating new rule"
                        );

                        client.create::<Value>(&op_id, &endpoint, rule).await
                    }
                });
                responses.extend(join_all_ok(create_futures).await?);
            }

            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                created = create_count,
                updated = update_count,
                "Parallel rules operation complete"
            );

            Ok(HookOutcome::Handled(json!({
                "created": create_count,
                "updated": update_count,
                "responses": responses
            })))
        })
    }

    /// Delete all configured rules on destroy by guid.
    ///
    /// The bulk schema uses `discovery: skip`, so Destroy mode emits an optimistic
    /// Delete with `pre_delete` invoked. Resolve each configured rule's guid via
    /// `fetch_existing_rules` (name → guid) and `DELETE /v3/enforcement/rules/{guid}`
    /// — without this the bulk-created rules leak (the default delete targets the
    /// bulk resource's own id, not the per-rule guids).
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let wanted: Vec<String> = resource.get("rules").and_then(|r| r.as_array()).map(|rules| rules.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect()).unwrap_or_default();
            if wanted.is_empty() {
                return Ok(HookOutcome::Handled(json!({"deleted": 0})));
            }
            let existing = self.fetch_existing_rules(client, endpoint, operation_id).await?;
            let guids: Vec<String> = wanted.iter().filter_map(|name| existing.get(name).cloned()).collect();
            let deleted = guids.len();
            let base = rules_base_endpoint(endpoint);
            let delete_futures = guids.iter().map(|guid| {
                let client = client.clone();
                let op_id = operation_id.to_string();
                let del_endpoint = format!("{}/{}", base, guid);
                async move {
                    // `not_found_ok()` suppresses the spurious WXCTL-H001 event; the call
                    // still returns Err on 404, so map an already-absent rule to a no-op
                    // here so `join_all_ok` does not abort the whole destroy.
                    let spec = RequestSpec::new(Method::DELETE, &del_endpoint).body(BodyKind::None).not_found_ok();
                    match client.execute::<Value>(&op_id, spec).await {
                        Ok(v) => Ok(v),
                        Err(e) if error_has_status(&e, 404) => Ok(Value::Null),
                        Err(e) => Err(e),
                    }
                }
            });
            join_all_ok(delete_futures).await?;
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, deleted, "deleted bulk rules by guid");
            Ok(HookOutcome::Handled(json!({"deleted": deleted})))
        })
    }
}

impl RulesHandler {
    /// Fetch existing rules for reconciliation
    /// Returns a map of rule name -> guid
    async fn fetch_existing_rules<'a>(&'a self, client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Result<HashMap<String, String>> {
        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            "Fetching existing rules for reconciliation"
        );

        let base = rules_base_endpoint(endpoint);
        let all_resources = crate::util::fetch_all_pages(client, operation_id, base).await?;
        let mut existing_map: HashMap<String, String> = HashMap::new();

        for resource in &all_resources {
            let name = resource.get("entity").and_then(|e| e.get("name")).and_then(|n| n.as_str());
            let guid = resource.get("metadata").and_then(|m| m.get("guid")).and_then(|g| g.as_str());

            if let (Some(name), Some(guid)) = (name, guid) {
                existing_map.insert(name.to_string(), guid.to_string());
            }
        }

        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            existing_count = existing_map.len(),
            "Found existing rules"
        );

        Ok(existing_map)
    }

    /// Handle import file mode - upload to import endpoint
    /// API expects application/octet-stream with raw JSON content
    async fn handle_import<'a>(&'a self, client: &'a HttpClient, import_file: &'a str, operation_id: &'a str) -> Result<HookOutcome> {
        let file_path = Path::new(import_file);
        if !file_path.exists() {
            return Err(anyhow!("Import file not found: {}. Path should be relative to config file or absolute.", import_file));
        }

        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            import_file = %import_file,
            "Importing rules from file (skipping reconciliation)"
        );

        // Read file contents as raw bytes
        let file_contents = fs::read(file_path).await.map_err(|e| anyhow!("Failed to read import file '{}': {}", import_file, e))?;

        // Send as application/octet-stream (API requirement)
        let spec = RequestSpec::new(Method::POST, "/v3/enforcement/rules/import").body(BodyKind::OctetStream(file_contents));

        let response: Value = client.execute(operation_id, spec).await?;

        Ok(HookOutcome::Handled(response))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Gap A repro: the bulk `rules` kind's delete endpoint is the engine's fallback
    // `get_endpoint` template `/v3/enforcement/rules/{name}` (rules.yaml declares no
    // delete_endpoint). pre_delete must list + delete against the BASE path, never the
    // literal `{name}` template — otherwise the existence-GET 404s (WDPPS5009E) and the
    // destroy fails. Any final `{...}` placeholder (with or without a trailing slash) is
    // stripped; an already-base path (pre_create's create endpoint) is a no-op.
    #[test]
    fn rules_base_endpoint_cases() {
        let cases: &[(&str, &str)] = &[("/v3/enforcement/rules/{name}", "/v3/enforcement/rules"), ("/v3/enforcement/rules/{guid}", "/v3/enforcement/rules"), ("/v3/enforcement/rules/{name}/", "/v3/enforcement/rules"), ("/v3/enforcement/rules", "/v3/enforcement/rules")];
        for (input, expected) in cases {
            assert_eq!(rules_base_endpoint(input), *expected, "{input}");
        }
    }

    // The per-guid DELETE built from the base path must be the well-formed
    // `/v3/enforcement/rules/{guid}` (no encoded `%7Bname%7D` segment) and must
    // declare 404 as expected so an already-absent rule does not emit a spurious
    // WXCTL-H001 (delete 0, no error — Gap A's stated contract).
    #[test]
    fn per_guid_delete_spec_targets_base_path_and_tolerates_404() {
        let base = rules_base_endpoint("/v3/enforcement/rules/{name}");
        let del_endpoint = format!("{}/{}", base, "guid-123");
        assert_eq!(del_endpoint, "/v3/enforcement/rules/guid-123");
        assert!(!del_endpoint.contains('{'), "delete endpoint must not contain an unsubstituted template segment");
        let spec = RequestSpec::new(Method::DELETE, &del_endpoint).body(BodyKind::None).not_found_ok();
        assert!(spec.expected_statuses.contains(&404), "404 must be expected so an already-absent rule is a no-op");
    }

    // No configured rule matches any live rule → 0 guids resolved → 0 deletes,
    // 0 errors. This mirrors the name→guid match in pre_delete against an empty
    // existing-rules map.
    #[test]
    fn no_matching_rule_resolves_zero_guids() {
        let wanted = ["rule_pii_access"];
        let existing: std::collections::HashMap<String, String> = std::collections::HashMap::new();
        let guids: Vec<String> = wanted.iter().filter_map(|name| existing.get(*name).cloned()).collect();
        assert_eq!(guids.len(), 0, "no live match → 0 deletes / 0 errors");
    }

    // A bulk resource with no `rules` array deletes 0 (the wanted-is-empty
    // early-return).
    #[test]
    fn empty_rules_array_yields_no_wanted_names() {
        let resource = json!({"name": "rules_bulk"});
        let wanted: Vec<String> = resource.get("rules").and_then(|r| r.as_array()).map(|rules| rules.iter().filter_map(|r| r.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect()).unwrap_or_default();
        assert!(wanted.is_empty());
    }
}
