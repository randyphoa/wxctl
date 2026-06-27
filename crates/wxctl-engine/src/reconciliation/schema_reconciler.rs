//! Schema-based reconciler implementation.

use crate::templates::is_template;
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, BodyKindSelector, HttpClient, RequestMaterializer, RequestSpec, extract_nested};
use wxctl_core::schema::{DiscoveryMethod, FieldLocation, IdentityMatch, ResourceSchema, SchemaDefinition};
use wxctl_core::traits::{Reconciler, StateComparison};
use wxctl_core::types::{RemoteResource, ValidatedResource};

/// Stateless schema-driven reconciler. All schema reads come from
/// `ValidatedResource.descriptor.schema`, which the validation pipeline
/// rebuilds per resource with deployment overlays merged in. Holding a
/// captured base schema here would silently discard those overlays for
/// Variant B parallel-schema kinds (e.g. `ingestion_job` whose Software
/// schema renames the SaaS `id` field to `job_id`).
#[derive(Default)]
pub struct SchemaBasedReconciler;

impl SchemaBasedReconciler {
    pub fn new() -> Self {
        Self
    }
}

/// Build query parameters for discovery list calls from the schema's
/// `location: Query` fields, plus any field marked `also_query: true` (used
/// where a Body field on POST also needs to appear as a query param on
/// list/get/delete — e.g. WML `space_id`/`project_id`). Returns Err if any
/// scoping value is still a template reference — the endpoint usually requires
/// the param, so callers must skip the API call rather than send the literal
/// `${...}` string.
fn build_scoping_params(data: &Value, schema: &ResourceSchema) -> Result<Option<HashMap<String, String>>> {
    let mut params = HashMap::new();

    for field in &schema.resource.schema.fields {
        if field.location != FieldLocation::Query && !field.also_query {
            continue;
        }
        // Resolve via get_nested_field so a dotted Query field name (e.g.
        // `target.target_id`) reads the nested value `data["target"]["target_id"]`
        // and is sent as `?target.target_id=<id>`. Single-segment names traverse
        // through the same `map.get`, so flat scoping fields are unchanged.
        if let Some(val) = get_nested_field(data, &field.name).as_str() {
            if is_template(val) {
                bail!("unresolved template reference in scoping parameter: {}", field.name);
            }
            params.insert(field.name.clone(), val.to_string());
        }
    }

    if params.is_empty() { Ok(None) } else { Ok(Some(params)) }
}

/// Substitute `{field}` path placeholders in a list/get endpoint using the
/// values of fields declared with `location: Path` in the schema. The
/// list-call machinery otherwise passes endpoints through verbatim — schemas
/// like `watsonx_data.schema` whose `list_endpoint` carries a `{catalog_id}`
/// segment would 400 against the literal template string.
fn substitute_path_placeholders(endpoint: &str, data: &Value, schema: &ResourceSchema) -> Result<String> {
    let mut out = endpoint.to_string();
    for field in &schema.resource.schema.fields {
        if field.location != FieldLocation::Path {
            continue;
        }
        let placeholder = format!("{{{}}}", field.name);
        if !out.contains(&placeholder) {
            continue;
        }
        let value = data.get(&field.name).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("path placeholder `{placeholder}` has no resolved value in resource data"))?;
        out = out.replace(&placeholder, value);
    }
    Ok(out)
}

/// Return the first unresolved `${...}` template found along paths the list
/// call actually needs to see literal values for: schema-declared Path/Query
/// fields (which feed the URL and query string) plus the identity field
/// compared against remote list items. Templates elsewhere in `data` (e.g. a
/// bucket ref on storage_registration whose identity path is a hardcoded
/// catalog_name) do NOT block discovery.
pub(super) fn identity_paths_unresolved(data: &Value, schema: &ResourceSchema) -> Option<String> {
    for field in &schema.resource.schema.fields {
        if field.location != FieldLocation::Path && field.location != FieldLocation::Query && !field.also_query {
            continue;
        }
        if let Some(s) = get_nested_field(data, &field.name).as_str()
            && is_template(s)
        {
            return Some(s.to_string());
        }
    }
    let discovery = &schema.resource.reconciliation.discovery;
    if let Some(im) = &discovery.identity_match {
        if let Some(s) = get_nested_field(data, &im.local_path).as_str()
            && is_template(s)
        {
            return Some(s.to_string());
        }
    } else {
        let name_field = discovery.name_field.as_deref().unwrap_or("name");
        if let Some(s) = data.get(name_field).and_then(|v| v.as_str())
            && is_template(s)
        {
            return Some(s.to_string());
        }
    }
    None
}

/// Return `true` when a JSON value still carries an unresolved `${...}` template
/// reference anywhere within it (scalar, or nested in an object/array). Used by
/// `compare` to skip a field whose local value couldn't be resolved because an
/// upstream dependency wasn't discovered — comparing the literal `${...}` string
/// against the real remote value would otherwise yield a phantom diff.
fn value_has_unresolved_template(value: &Value) -> bool {
    let mut found = false;
    wxctl_core::extract_references(value, &mut |_| found = true);
    found
}

/// Classify the LOCAL values of the fields `compare` inspects (the schema's
/// `state_fields` plus `immutable_fields`) into `(comparable, templated)` counts:
/// how many are present-and-fully-resolved vs. present-but-still-templated.
///
/// The Deferred-but-found Apply path uses this to decide whether the comparison
/// can be anchored: if there is at least one fully-resolved compared field, the
/// comparison runs (skipping the templated ones via `compare`). Only when EVERY
/// present compared field is still templated — so the comparison would be wholly
/// vacuous — does the caller fall back to the conservative blind
/// `Update { fields: vec![] }`. Fields absent from local data are ignored
/// (`compare` skips them too).
pub(super) fn compared_field_resolution(data: &Value, schema: &ResourceSchema) -> (usize, usize) {
    let reconciliation = &schema.resource.reconciliation;
    let state_fields = reconciliation.state_fields.as_deref().unwrap_or(&[]);
    let (mut comparable, mut templated) = (0usize, 0usize);
    for field in state_fields.iter().chain(reconciliation.immutable_fields.iter()) {
        if !field_exists(data, field) {
            continue;
        }
        if value_has_unresolved_template(get_nested_field(data, field)) {
            templated += 1;
        } else {
            comparable += 1;
        }
    }
    (comparable, templated)
}

/// Check if a JSON item's name matches the target, using the CP4D-aware field lookup.
/// When the item is a bare string (some list endpoints return `["name1", "name2"]`
/// instead of objects), compare the string directly.
fn name_matches(item: &Value, target: &str, field: &str) -> bool {
    if let Value::String(s) = item {
        return s == target;
    }
    get_nested_field(item, field).as_str() == Some(target)
}

/// Normalize a matched list item so downstream consumers (compare, delete,
/// denormalize) see a uniform object shape. Some watsonx.data LIST endpoints
/// (notably `/v3/catalogs/{catalog_id}/schemas`) return the list as bare
/// strings — without this, `extract_resource_id` has no field to pull from
/// and delete fails with "Missing ID field for delete".
fn normalize_list_item(item: Value, name_field: &str) -> Value {
    if let Value::String(s) = item {
        let mut obj = serde_json::Map::new();
        obj.insert(name_field.to_string(), Value::String(s));
        return Value::Object(obj);
    }
    item
}

/// Filter `items` to those matching the schema-declared identity. The schema
/// declares exactly one of: `identity_match` (for resources whose stable
/// identity sits at different local vs. remote paths — e.g. `catalog_name`
/// nested in a plural array remotely but a singular object locally) or
/// `name_field` (single same-path lookup used by most kinds, defaulting to
/// `name`). Only one branch runs — there is no secondary attempt.
fn match_remote_items<'a>(items: &'a [Value], resource_data: &Value, name_field: &str, resource_name: Option<&str>, identity: Option<&IdentityMatch>) -> Vec<&'a Value> {
    if let Some(im) = identity {
        let Some(target) = get_nested_field(resource_data, &im.local_path).as_str() else {
            return Vec::new();
        };
        return items.iter().filter(|item| get_nested_field(item, &im.remote_path).as_str() == Some(target)).collect();
    }
    let Some(resource_name) = resource_name else {
        return Vec::new();
    };
    items.iter().filter(|item| name_matches(item, resource_name, name_field)).collect()
}

impl Reconciler for SchemaBasedReconciler {
    fn discover<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<RemoteResource>> + Send + 'a>> {
        Box::pin(async move {
            let schema = &resource.descriptor.schema;
            let def = &schema.resource;
            let discovery = &def.reconciliation.discovery;
            let _id_field = &def.api.id_field;

            match discovery.method {
                DiscoveryMethod::ListAndGet => {
                    // List all resources and find matching one
                    let list_endpoint = def.api.list_endpoint.as_ref().ok_or_else(|| anyhow::anyhow!("list_endpoint not configured"))?;

                    // Note: list_field is optional - ListEnvelope::from_value() handles envelope detection automatically

                    // Only bail when an identity-relevant path still carries a template —
                    // the scoping params and identity field feed the list call itself.
                    if let Some(tpl) = identity_paths_unresolved(&resource.data, schema) {
                        tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, template = %tpl, "skipping discovery: identity-relevant path has unresolved template reference");
                        return Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false });
                    }

                    let params = match build_scoping_params(&resource.data, schema) {
                        Ok(p) => p,
                        Err(_) => {
                            tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "skipping discovery: scoping parameter has unresolved template reference");
                            return Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false });
                        }
                    };
                    let list_endpoint = substitute_path_placeholders(list_endpoint, &resource.data, schema)?;
                    // Treat 404 as "no resources found" — the parent container
                    // (space/project) may have been deleted, so no children can exist.
                    // list_with_params_absent_ok suppresses the wxctl::error event for 404
                    // so the output collector doesn't count discovery probes as failures.
                    let items: Vec<Value> = if discovery.list_method.as_deref() == Some("post") {
                        // POST-search enumeration: some APIs (CAMS `/v2/asset_types/<type>/search`)
                        // have no GET list, only a search POST. Send `list_body` with the scoping
                        // params as query, then pull the array at `list_field`.
                        let body = discovery.list_body.clone().unwrap_or_else(|| serde_json::json!({}));
                        let mut spec = RequestSpec::new(Method::POST, &list_endpoint).body(BodyKind::Json(body)).not_found_ok().stage("reconciliation");
                        if let Some(params) = &params {
                            for (k, v) in params {
                                spec = spec.query_param(k, v);
                            }
                        }
                        match client.execute::<Value>(operation_id, spec).await {
                            Ok(resp) => {
                                let field = discovery.list_field.as_deref().ok_or_else(|| anyhow::anyhow!("list_method: post requires list_field"))?;
                                resp.get(field).and_then(|v| v.as_array()).cloned().unwrap_or_default()
                            }
                            Err(e) if e.to_string().contains("HTTP 404") => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "search returned 404 — treating as not found");
                                return Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false });
                            }
                            Err(e) => return Err(e).context("Failed to search resources"),
                        }
                    } else {
                        match client.list_with_params_absent_ok(operation_id, &list_endpoint, params).await {
                            Ok(items) => items,
                            Err(e) if e.to_string().contains("HTTP 404") => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "list returned 404 — treating as not found");
                                return Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false });
                            }
                            Err(e) => return Err(e).context("Failed to list resources"),
                        }
                    };

                    // Identity lookup dispatches on which identity mechanism the schema declares:
                    // `identity_match` (local/remote paths) or the legacy single-path `name_field`.
                    let name_field = discovery.name_field.as_deref().unwrap_or("name");
                    let resource_name = if discovery.identity_match.is_some() { None } else { Some(resource.data.get(name_field).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Resource has no '{}' field", name_field))?) };

                    let matches = match_remote_items(&items, &resource.data, name_field, resource_name, discovery.identity_match.as_ref());
                    if let Some(remote_data) = matches.first() {
                        let mut data = normalize_list_item((*remote_data).clone(), name_field);
                        // Denormalize API response to add user-facing fields from nested api_field paths
                        denormalize_api_response(&mut data, &schema.resource.schema);
                        Ok(RemoteResource { key: resource.key.clone(), data, exists: true })
                    } else {
                        Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false })
                    }
                }
                DiscoveryMethod::GetById => {
                    // Try to get resource by ID directly using the id_source field
                    let id_source_field = &discovery.id_source;
                    // A server-minted id is absent until the resource is created. With no id to
                    // GET, the resource cannot exist remotely under our reference, so treat it as
                    // not-found (plan Create) rather than erroring. (Kinds whose id is
                    // client-supplied, e.g. ingestion_job, always have it populated here.)
                    let Some(resource_id) = resource.data.get(id_source_field).and_then(|v| v.as_str()) else {
                        tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, id_source = %id_source_field, "get_by_id: id source absent — treating as not found (create)");
                        return Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false });
                    };

                    let endpoint = def.api.get_endpoint.replace(&format!("{{{}}}", id_source_field), resource_id);

                    // 404 = resource absent (plan Create) — not_found_ok() suppresses the
                    // wxctl::error event so the output collector doesn't count it as a failure.
                    let spec = RequestSpec::new(Method::GET, &endpoint).body(BodyKind::None).not_found_ok().stage("reconciliation");
                    match client.execute::<Value>(operation_id, spec).await {
                        Ok(remote_data) => {
                            // Denormalize API response to add user-facing fields from nested api_field paths
                            let mut data = remote_data;
                            denormalize_api_response(&mut data, &schema.resource.schema);
                            Ok(RemoteResource { key: resource.key.clone(), data, exists: true })
                        }
                        Err(e) => {
                            // Only treat 404 as "not found" — propagate network/server errors.
                            // The HTTP client converts errors to anyhow via retry.rs, losing the
                            // typed status code. The error message format "HTTP 404 ..." is produced
                            // by HttpError::with_status in http.rs and is the only reliable signal.
                            let is_not_found = e.to_string().contains("HTTP 404");

                            if is_not_found { Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false }) } else { Err(e).context(format!("Failed to discover {} '{}'", resource.key.kind, resource.key.name)) }
                        }
                    }
                }
                DiscoveryMethod::Skip => {
                    // Skip discovery - always return as not existing to force create
                    // Used for bulk/batch operations where individual resource discovery isn't possible
                    tracing::debug!(
                        target: "wxctl::reconciliation::discovery",
                        operation_id = %operation_id,
                        resource_type = %resource.key.kind,
                        resource_name = %resource.key.name,
                        "skipping discovery for bulk operation resource"
                    );
                    Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false })
                }
                DiscoveryMethod::Singleton => {
                    // Per-instance singleton (e.g. sal_integration, sal_global_settings):
                    // GET the id-less get_endpoint. Materialize the request so a singleton's
                    // location: Query/Path fields (e.g. sal_enrichment_settings.project_id)
                    // flow into the URL — a raw GET drops a required query param and 400s.
                    // A non-empty 200 is the one existing instance; an empty body or 404
                    // means absent (plan Create / "enable").
                    // not_found_ok() suppresses the wxctl::error event for 404 — the Err
                    // branch below still distinguishes HTML (bad route) vs clean 404 (absent).
                    let spec = RequestMaterializer::new(Method::GET, &def.api.get_endpoint).materialize(&resource.data, &schema.resource.schema.fields, BodyKindSelector::None)?.not_found_ok().stage("reconciliation");
                    match client.execute::<Value>(operation_id, spec).await {
                        Ok(remote_data) => {
                            // Some singletons return 200 with a sentinel body when absent (e.g. SAL's
                            // GET /v3/sal_integration → {"status":"missing"} until enabled). Honor the
                            // schema's `absent_when` so that reads as absent (plan Create) not Update.
                            let absent_sentinel = discovery.absent_when.as_ref().is_some_and(|aw| get_nested_field(&remote_data, &aw.field).as_str() == Some(aw.equals.as_str()));
                            let is_empty = absent_sentinel || remote_data.is_null() || remote_data.as_object().map(|m| m.is_empty()).unwrap_or(false);
                            if is_empty {
                                Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false })
                            } else {
                                let mut data = remote_data;
                                denormalize_api_response(&mut data, &schema.resource.schema);
                                Ok(RemoteResource { key: resource.key.clone(), data, exists: true })
                            }
                        }
                        Err(e) => {
                            // A 404 normally means the singleton is absent (plan Create / "enable").
                            // But an nginx/gateway *HTML* 404 means the ROUTE does not exist — a wrong
                            // endpoint path — not an absent resource. Don't mask a path bug as "absent"
                            // (that yields a false-green plan, then a 404 on the create POST). The
                            // discovery GET URL + status are in the structured HTTP log for diagnosis.
                            let es = e.to_string();
                            let is_not_found = es.contains("HTTP 404");
                            let route_missing = is_not_found && (es.contains("<html") || es.to_ascii_lowercase().contains("nginx") || es.contains("404 Not Found"));
                            if is_not_found && !route_missing {
                                Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false })
                            } else {
                                Err(e).context(format!("Failed to discover singleton {} '{}' (a 404 carrying an HTML/nginx body indicates a wrong endpoint path, not an absent resource)", resource.key.kind, resource.key.name))
                            }
                        }
                    }
                }
            }
        })
    }

    fn discover_all<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<Vec<RemoteResource>>> + Send + 'a>> {
        Box::pin(async move {
            let descriptor_schema = &resource.descriptor.schema;
            let def = &descriptor_schema.resource;
            let discovery = &def.reconciliation.discovery;

            match discovery.method {
                DiscoveryMethod::ListAndGet => {
                    // List all resources and find ALL matching ones
                    let list_endpoint = def.api.list_endpoint.as_ref().ok_or_else(|| anyhow::anyhow!("list_endpoint not configured"))?;

                    if let Some(tpl) = identity_paths_unresolved(&resource.data, descriptor_schema) {
                        tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, template = %tpl, "skipping discovery_all: identity-relevant path has unresolved template reference");
                        return Ok(vec![]);
                    }

                    let params = match build_scoping_params(&resource.data, descriptor_schema) {
                        Ok(p) => p,
                        Err(_) => {
                            tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "skipping discovery_all: scoping parameter has unresolved template reference");
                            return Ok(vec![]);
                        }
                    };
                    let list_endpoint = substitute_path_placeholders(list_endpoint, &resource.data, descriptor_schema)?;
                    // Treat 404 as "no resources found" — the parent container
                    // (space/project) may have been deleted, so no children can exist.
                    // list_with_params_absent_ok suppresses the wxctl::error event for 404.
                    let items: Vec<Value> = if discovery.list_method.as_deref() == Some("post") {
                        // POST-search enumeration (see `discover`): APIs like CAMS
                        // `/v2/asset_types/<type>/search` have no GET list, only a search POST.
                        let body = discovery.list_body.clone().unwrap_or_else(|| serde_json::json!({}));
                        let mut spec = RequestSpec::new(Method::POST, &list_endpoint).body(BodyKind::Json(body)).not_found_ok().stage("reconciliation");
                        if let Some(params) = &params {
                            for (k, v) in params {
                                spec = spec.query_param(k, v);
                            }
                        }
                        match client.execute::<Value>(operation_id, spec).await {
                            Ok(resp) => {
                                let field = discovery.list_field.as_deref().ok_or_else(|| anyhow::anyhow!("list_method: post requires list_field"))?;
                                resp.get(field).and_then(|v| v.as_array()).cloned().unwrap_or_default()
                            }
                            Err(e) if e.to_string().contains("HTTP 404") => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "search returned 404 — treating as not found");
                                return Ok(vec![]);
                            }
                            Err(e) => return Err(e).context("Failed to search resources"),
                        }
                    } else {
                        match client.list_with_params_absent_ok(operation_id, &list_endpoint, params).await {
                            Ok(items) => items,
                            Err(e) if e.to_string().contains("HTTP 404") => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "list returned 404 — treating as not found");
                                return Ok(vec![]);
                            }
                            Err(e) => return Err(e).context("Failed to list resources"),
                        }
                    };

                    // Identity lookup: see `discover` for the name_field vs identity_match dispatch.
                    let name_field = discovery.name_field.as_deref().unwrap_or("name");
                    let resource_name = if discovery.identity_match.is_some() { None } else { Some(resource.data.get(name_field).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Resource has no '{}' field", name_field))?) };

                    // Find ALL resources matching the identity (not just first)
                    let field_schema = &descriptor_schema.resource.schema;
                    let matched_items = match_remote_items(&items, &resource.data, name_field, resource_name, discovery.identity_match.as_ref());
                    let matches: Vec<RemoteResource> = matched_items
                        .into_iter()
                        .map(|data| {
                            let mut denormalized_data = normalize_list_item(data.clone(), name_field);
                            denormalize_api_response(&mut denormalized_data, field_schema);
                            RemoteResource { key: resource.key.clone(), data: denormalized_data, exists: true }
                        })
                        .collect();

                    if matches.len() > 1 {
                        tracing::warn!(
                            target: "wxctl::reconciliation::discovery",
                            operation_id = %operation_id,
                            resource_type = %resource.key.kind,
                            resource_name = %resource.key.name,
                            count = matches.len(),
                            "found multiple remote resources with the same name"
                        );
                    }

                    Ok(matches)
                }
                // For GetById, Skip, and Singleton, delegate to discover() (single result)
                DiscoveryMethod::GetById | DiscoveryMethod::Skip | DiscoveryMethod::Singleton => {
                    let remote = self.discover(operation_id, resource, client).await?;
                    if remote.exists { Ok(vec![remote]) } else { Ok(vec![]) }
                }
            }
        })
    }

    fn compare(&self, local: &ValidatedResource, remote: &RemoteResource) -> StateComparison {
        if !remote.exists {
            return StateComparison::Create;
        }

        let def = &local.descriptor.schema.resource;
        let reconciliation = &def.reconciliation;

        // Check immutable fields for recreate condition
        for immutable_field in &reconciliation.immutable_fields {
            let local_value = get_nested_field(&local.data, immutable_field);
            // Skip a field whose local value still carries an unresolved `${...}`
            // template — its upstream dependency wasn't discovered, so the literal
            // template string is not a real value to diff against the remote.
            // (On the normal Discovered path every local ref is already resolved,
            // so this skip is a no-op there; it only relaxes the Deferred-but-found
            // Apply path, which would otherwise see a phantom immutable drift.)
            if value_has_unresolved_template(local_value) {
                continue;
            }
            let remote_value = get_nested_field(&remote.data, immutable_field);

            if !values_match(local_value, remote_value) {
                return StateComparison::Recreate { field: immutable_field.clone(), local_value: render_value(local_value), remote_value: render_value(remote_value) };
            }
        }

        // Compare state fields
        // Only compare fields that are explicitly set in the local config
        // This prevents spurious updates for fields with remote defaults that aren't specified locally
        let mut changed_fields = Vec::new();
        let state_fields = reconciliation.state_fields.as_deref().unwrap_or(&[]);
        for state_field in state_fields {
            // Skip comparison if field doesn't exist in local config
            if !field_exists(&local.data, state_field) {
                continue;
            }

            let local_value = get_nested_field(&local.data, state_field);
            // Skip a state field still carrying an unresolved `${...}` template, for
            // the same reason as the immutable check above — no real value to diff.
            // No-op on the Discovered path (locals fully resolved).
            if value_has_unresolved_template(local_value) {
                continue;
            }
            let remote_value = get_nested_field(&remote.data, state_field);

            if !values_match(local_value, remote_value) {
                tracing::debug!(
                    target: "wxctl::reconciliation::diff",
                    resource_type = %local.key.kind,
                    resource_name = %local.key.name,
                    field = %state_field,
                    local_value = %local_value,
                    remote_value = %remote_value,
                    "Field difference detected"
                );
                changed_fields.push(state_field.clone());
            }
        }

        if changed_fields.is_empty() { StateComparison::NoChange } else { StateComparison::Update { fields: changed_fields } }
    }
}

/// Check if a nested field exists in JSON object using dot notation (e.g., "storage.bucket")
fn field_exists(value: &Value, path: &str) -> bool {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = value;

    for part in parts {
        match current {
            Value::Object(map) => match map.get(part) {
                Some(v) => current = v,
                None => return false,
            },
            _ => return false,
        }
    }

    true
}

/// Get nested field value from JSON object using dot notation (e.g., "storage.bucket").
/// Numeric segments index into arrays (e.g. `associated_catalogs.0.catalog_name`).
/// For CP4D/watsonx.data responses, also checks under entity.{field}.
pub(super) fn get_nested_field<'a>(value: &'a Value, path: &str) -> &'a Value {
    let parts: Vec<&str> = path.split('.').collect();
    let mut current = value;

    for part in &parts {
        match current {
            Value::Object(map) => {
                current = map.get(*part).unwrap_or(&Value::Null);
            }
            Value::Array(arr) => match part.parse::<usize>() {
                Ok(idx) => current = arr.get(idx).unwrap_or(&Value::Null),
                Err(_) => return &Value::Null,
            },
            _ => return &Value::Null,
        }
    }

    // If value is null and we only have a single part, try alternative locations (CP4D structure)
    if current.is_null()
        && parts.len() == 1
        && let Value::Object(map) = value
    {
        // Try entity.<field> (most CP4D responses)
        if let Some(Value::Object(entity_map)) = map.get("entity")
            && let Some(field_value) = entity_map.get(parts[0])
        {
            return field_value;
        }
        // Try metadata.<field> (categories, business terms, etc.)
        if let Some(Value::Object(metadata_map)) = map.get("metadata")
            && let Some(field_value) = metadata_map.get(parts[0])
        {
            return field_value;
        }
    }

    current
}

/// Render a JSON value as a plain display string: strings unquoted, other scalars
/// via their native formatting, structures via `serde_json::to_string`. Used for
/// building user-facing error messages about drifted immutable fields.
pub(super) fn render_value(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| "<unrenderable>".to_string()),
    }
}

/// Check if a JSON value is semantically empty (null-equivalent).
/// Many APIs return null for fields that are logically empty arrays, empty objects,
/// empty strings, or false booleans. This helper normalizes that behavior.
fn is_null_equivalent(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Array(arr) => arr.is_empty(),
        Value::Object(map) => map.is_empty(),
        Value::String(s) => s.is_empty(),
        Value::Bool(false) => true,
        _ => false,
    }
}

/// Compare values with selective matching for objects and arrays.
/// For objects: only compare fields present in local value (ignore extra remote fields)
/// For arrays: compare element-wise with selective matching for object elements
/// For null-equivalent pairs: treat null, [], {}, "", and false as equal
/// For other types: direct equality
fn values_match(local: &Value, remote: &Value) -> bool {
    // Handle null-equivalence: null ≈ [] ≈ {} ≈ "" ≈ false
    // Many APIs return null for empty arrays, default booleans, etc.
    if is_null_equivalent(local) && is_null_equivalent(remote) {
        return true;
    }

    match (local, remote) {
        (Value::Object(local_map), Value::Object(remote_map)) => {
            // Only compare fields that exist in the local config
            // This allows the remote to have additional server-managed fields
            for (key, local_value) in local_map {
                match remote_map.get(key) {
                    Some(remote_value) => {
                        // Recursively compare nested values
                        if !values_match(local_value, remote_value) {
                            return false;
                        }
                    }
                    None => {
                        // Local has a field that remote doesn't — but if the local
                        // value is null-equivalent, treat the missing remote field as matching
                        if !is_null_equivalent(local_value) {
                            return false;
                        }
                    }
                }
            }
            true
        }
        (Value::Array(local_arr), Value::Array(remote_arr)) => {
            // Arrays must have the same length
            if local_arr.len() != remote_arr.len() {
                return false;
            }
            // Compare each element with selective matching
            for (local_item, remote_item) in local_arr.iter().zip(remote_arr.iter()) {
                if !values_match(local_item, remote_item) {
                    return false;
                }
            }
            true
        }
        // Resolved reference objects vs extracted ID strings: template resolution
        // produces full resource objects, but the remote stores just the extracted
        // field (e.g., connection_id). Treat as matching if any field in the local
        // object equals the remote string.
        (Value::Object(local_map), Value::String(_)) => local_map.values().any(|v| v == remote),
        // Inverse of the arm above: APIs like IBM ML wrap referenced ids as
        // `{"id": "..."}` on the remote, while local has already been extracted
        // to a bare id string by `apply_field_references`. Match symmetrically.
        (Value::String(_), Value::Object(remote_map)) => remote_map.values().any(|v| v == local),
        // For other types, use direct equality
        _ => local == remote,
    }
}

/// Like `extract_nested`, but for a single-segment path also falls back
/// through the CP4D `entity.<field>` / `metadata.<field>` envelope — mirroring
/// `get_nested_field`. Used when denormalizing a discovered (list_and_get)
/// response whose `api_field` value sits inside the CP4D envelope rather than at
/// the top level. Returns `None` when the field is absent anywhere (no-op:
/// never fabricates a value that would mask a real diff).
fn extract_nested_value_enveloped<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    if let Some(found) = extract_nested(value, path) {
        return Some(found);
    }
    if !path.contains('.')
        && let Value::Object(map) = value
    {
        for envelope in ["entity", "metadata"] {
            if let Some(Value::Object(inner)) = map.get(envelope)
                && let Some(found) = inner.get(path)
            {
                return Some(found);
            }
        }
    }
    None
}

/// Denormalize API response to user-facing format using api_field mappings
/// Extracts values from nested API paths and adds them as top-level fields
fn denormalize_api_response(response: &mut Value, schema: &SchemaDefinition) {
    // Collect field values from an immutable borrow first, then apply as mutations
    let insertions: Vec<(String, Value)> = schema
        .fields
        .iter()
        .filter_map(|field| {
            let api_path = field.api_field.as_ref()?;
            let value = extract_nested_value_enveloped(&*response, api_path)?;
            if value.is_null() {
                return None;
            }
            Some((field.name.clone(), value.clone()))
        })
        .collect();

    // Apply collected insertions
    if let Some(obj) = response.as_object_mut() {
        for (name, value) in insertions {
            obj.insert(name, value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn get_nested_field_indexes_into_arrays() {
        let v = json!({"associated_catalogs": [{"catalog_name": "cat_a"}, {"catalog_name": "cat_b"}]});
        assert_eq!(get_nested_field(&v, "associated_catalogs.0.catalog_name").as_str(), Some("cat_a"));
        assert_eq!(get_nested_field(&v, "associated_catalogs.1.catalog_name").as_str(), Some("cat_b"));
        assert!(get_nested_field(&v, "associated_catalogs.2.catalog_name").is_null());
        assert!(get_nested_field(&v, "associated_catalogs.nope.catalog_name").is_null());
    }

    #[test]
    fn match_remote_items_selection() {
        // Storage-registration shape: plural `associated_catalogs[0]` remote, singular `associated_catalog` local.
        let items = vec![json!({"display_name": "A", "associated_catalogs": [{"catalog_name": "cat_x"}]}), json!({"display_name": "B", "associated_catalogs": [{"catalog_name": "cat_y"}]})];
        let local = json!({"display_name": "ignored", "associated_catalog": {"catalog_name": "cat_y"}});
        let identity = IdentityMatch { local_path: "associated_catalog.catalog_name".into(), remote_path: "associated_catalogs.0.catalog_name".into() };
        let hits = match_remote_items(&items, &local, "display_name", None, Some(&identity));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("display_name").and_then(|v| v.as_str()), Some("B"), "identity_match selects by catalog name, not display_name");

        // Even if `display_name` matches, identity_match is the only criterion when declared.
        let items = vec![json!({"display_name": "same", "associated_catalog": {"catalog_name": "cat_x"}}), json!({"display_name": "other", "associated_catalog": {"catalog_name": "cat_y"}})];
        let local = json!({"display_name": "same", "associated_catalog": {"catalog_name": "cat_y"}});
        let identity = IdentityMatch { local_path: "associated_catalog.catalog_name".into(), remote_path: "associated_catalog.catalog_name".into() };
        let hits = match_remote_items(&items, &local, "display_name", None, Some(&identity));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("display_name").and_then(|v| v.as_str()), Some("other"), "identity_match ignores a matching display_name");

        // Falls back to name_field when identity_match is absent.
        let items = vec![json!({"name": "alpha"}), json!({"name": "beta"})];
        let local = json!({"name": "beta"});
        let hits = match_remote_items(&items, &local, "name", Some("beta"), None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("name").and_then(|v| v.as_str()), Some("beta"));
    }

    #[test]
    fn values_match_equivalence_branches() {
        // (local, remote, expected) — each row is a distinct values_match branch.
        let cases: &[(Value, Value, bool)] = &[
            // identical / differing scalar strings
            (json!("hello"), json!("hello"), true),
            (json!("hello"), json!("world"), false),
            // null-equivalents: null↔null, null↔[], null↔{}, ""↔null, false↔null
            (json!(null), json!(null), true),
            (json!(null), json!([]), true),
            (json!(null), json!({}), true),
            (json!(""), json!(null), true),
            (json!(false), json!(null), true),
            // objects: extra remote fields ignored; changed compared field detected
            (json!({"name": "foo"}), json!({"name": "foo", "id": "123", "tenant_id": "abc"}), true),
            (json!({"name": "foo"}), json!({"name": "bar", "id": "123"}), false),
            // arrays: equal/differing same-length, and differing length
            (json!(["a", "b"]), json!(["a", "b"]), true),
            (json!(["a", "b"]), json!(["a", "c"]), false),
            (json!(["a"]), json!(["a", "b"]), false),
            // Template resolution produces full resource objects, but the remote
            // stores just the extracted field (e.g., connection_id).
            (json!({"app_id": "mcp-auth", "connection_id": "uuid-123", "tenant_id": "t1"}), json!("uuid-123"), true),
            (json!({"app_id": "mcp-auth", "connection_id": "uuid-123"}), json!("uuid-999"), false),
            // wml_deployment.asset shape: local extracted to a bare id string (by
            // apply_field_references), remote returns `{"id": "..."}`. Inverse of the
            // object-vs-extracted-id case above.
            (json!("019e3667-6624-7695-bee2-62609ef21f9b"), json!({"id": "019e3667-6624-7695-bee2-62609ef21f9b"}), true),
            (json!("uuid-999"), json!({"id": "uuid-123"}), false),
        ];
        for (local, remote, expected) in cases {
            assert_eq!(values_match(local, remote), *expected, "values_match({local}, {remote}) expected {expected}");
        }
    }

    #[test]
    fn name_matches_scalar_and_object_items() {
        // Scalar string items compare directly.
        assert!(name_matches(&json!("my_schema"), "my_schema", "name"));
        assert!(!name_matches(&json!("other"), "my_schema", "name"));
        // Object items compare on the lookup field.
        let item = json!({"name": "engine-1", "id": "presto-1"});
        assert!(name_matches(&item, "engine-1", "name"));
        assert!(!name_matches(&item, "engine-2", "name"));
        // The lookup field is honored, not hardcoded to "name".
        let custom = json!({"display_name": "My Engine", "id": "abc"});
        assert!(name_matches(&custom, "My Engine", "display_name"));
    }

    #[test]
    fn normalize_list_item_wraps_bare_strings_only() {
        // Bare string → wrapped as {name_field: value}; objects pass through unchanged.
        assert_eq!(normalize_list_item(json!("sample"), "name"), json!({"name": "sample"}));
        let orig = json!({"name": "x", "id": "y"});
        assert_eq!(normalize_list_item(orig.clone(), "name"), orig);
    }

    /// Build a minimal ResourceSchema with the named fields declared at the
    /// given locations and optional identity/name_field settings. Used to
    /// exercise the Path/Query-aware branches of identity_paths_unresolved,
    /// build_scoping_params, and substitute_path_placeholders.
    fn make_schema_with_fields(fields: &[(&str, FieldLocation)], identity: Option<IdentityMatch>, name_field: Option<&str>) -> ResourceSchema {
        use wxctl_core::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldType, HookDefinition, HttpMethod, ReconciliationDefinition, ResourceDefinition, ResourceSchema, SchemaDefinition, UpdateStrategy};
        let field_defs = fields
            .iter()
            .map(|(name, location)| FieldDefinition {
                name: (*name).into(),
                field_type: FieldType::String,
                required: false,
                immutable: false,
                location: location.clone(),
                description: None,
                validation: None,
                schema: None,
                item_type: None,
                default: None,
                allowed_values: None,
                references: None,
                api_field: None,
                sensitive: false,
                also_query: false,
                is_path: false,
                properties: None,
            })
            .collect();
        // Build list_endpoint with placeholders so substitute_path_placeholders
        // tests have something meaningful to substitute.
        let path_placeholders: String = fields.iter().filter(|(_, loc)| loc == &FieldLocation::Path).map(|(name, _)| format!("/{{{name}}}")).collect();
        let list_endpoint = format!("/v1/parents{path_placeholders}/things");
        ResourceSchema {
            resource: ResourceDefinition {
                name: "thing".into(),
                service: "svc".into(),
                kind: "thing".into(),
                version: "v1".into(),
                api: ApiDefinition {
                    base_path: list_endpoint.clone(),
                    id_field: "id".into(),
                    list_endpoint: Some(list_endpoint.clone()),
                    get_endpoint: format!("{list_endpoint}/{{id}}"),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: None,
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                },
                schema: SchemaDefinition { fields: field_defs, ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, name_field: name_field.map(str::to_string), identity_match: identity, absent_when: None, list_method: None, list_body: None, id_source: "id".into() },
                    state_fields: Some(vec![]),
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec![],
                    reject_on_immutable_drift: false,
                    use_json_patch: false,
                    json_patch_path_prefix: None,
                },
                hooks: HookDefinition::default(),
                deployments: None,
                unsupported_on: vec![],
                description: None,
                prompt: None,
            },
        }
    }

    #[test]
    fn identity_paths_unresolved_branches() {
        // (label, data, fields, identity, name_field, expected) — each row is a
        // distinct identity_paths_unresolved branch: None = discovery proceeds,
        // Some(s) = unresolved template `s` surfaced so discovery is skipped.
        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, Value, Vec<(&str, FieldLocation)>, Option<IdentityMatch>, Option<&str>, Option<&str>)> = vec![
            // storage_registration re-apply regression: bucket ref is templated but
            // identity_match.local_path (associated_catalog.catalog_name) is a literal → None.
            (
                "identity literal despite other templates",
                json!({
                    "bucket": "${s3_bucket.wxctl_iceberg_bucket.name}",
                    "associated_catalog": {"catalog_name": "wxctl_iceberg", "catalog_type": "iceberg"},
                    "display_name": "wxctl-iceberg"
                }),
                vec![],
                Some(IdentityMatch { local_path: "associated_catalog.catalog_name".into(), remote_path: "associated_catalogs.0.catalog_name".into() }),
                None,
                None,
            ),
            // identity_match value itself templated → surfaced.
            (
                "templated identity_match value",
                json!({"associated_catalog": {"catalog_name": "${presto_engine.analytics.catalog}", "catalog_type": "iceberg"}}),
                vec![],
                Some(IdentityMatch { local_path: "associated_catalog.catalog_name".into(), remote_path: "associated_catalogs.0.catalog_name".into() }),
                None,
                Some("${presto_engine.analytics.catalog}"),
            ),
            // Templated Query / Path scoping fields → surfaced.
            ("templated query field", json!({"catalog_id": "${catalog.primary.id}", "name": "thing"}), vec![("catalog_id", FieldLocation::Query)], None, None, Some("${catalog.primary.id}")),
            ("templated path field", json!({"catalog_id": "${catalog.primary.id}", "name": "thing"}), vec![("catalog_id", FieldLocation::Path)], None, None, Some("${catalog.primary.id}")),
            // Path/Query/name all literal (an unrelated `other` template is irrelevant) → None.
            ("path/query/name all literal", json!({"catalog_id": "lit-cat-id", "name": "thing", "other": "${foo.bar}"}), vec![("catalog_id", FieldLocation::Path)], None, None, None),
            // A Body field with a templated value is NOT identity-relevant — the
            // template goes into the POST body at execution time, not the list call → None.
            ("templated body field ignored", json!({"bucket": "${s3_bucket.x.name}", "name": "thing"}), vec![("bucket", FieldLocation::Body)], None, None, None),
            // Templated default name field when identity_match absent → surfaced.
            ("templated name field (no identity)", json!({"name": "${thing.x.name}"}), vec![], None, None, Some("${thing.x.name}")),
            // Dotted Query field reader must surface an unresolved nested template
            // so discovery is skipped (returns the template string to the caller).
            ("templated dotted query field", json!({"target": {"target_id": "${subscription.os_sub.id}"}, "monitor_definition_id": "quality"}), vec![("target.target_id", FieldLocation::Query)], None, None, Some("${subscription.os_sub.id}")),
            // Custom name_field honored when templated.
            ("templated custom name field", json!({"display_name": "${engine.x.display_name}"}), vec![], None, Some("display_name"), Some("${engine.x.display_name}")),
        ];
        for (label, data, fields, identity, name_field, expected) in cases {
            let schema = make_schema_with_fields(&fields, identity, name_field);
            assert_eq!(identity_paths_unresolved(&data, &schema).as_deref(), expected, "case: {label}");
        }
    }

    #[test]
    fn build_scoping_params_query_field_branches() {
        // A `location: Query` field named `target.target_id` must read the nested
        // value data["target"]["target_id"] and emit it under that dotted key.
        let data = json!({"target": {"target_id": "sub-123", "target_type": "subscription"}, "monitor_definition_id": "quality"});
        let schema = make_schema_with_fields(&[("target.target_id", FieldLocation::Query)], None, None);
        let params = build_scoping_params(&data, &schema).unwrap().expect("scoping params present");
        assert_eq!(params.get("target.target_id").map(String::as_str), Some("sub-123"));

        // Single-segment Query names traverse via map.get exactly as before — guards
        // against a regression in existing flat scoping fields (space_id, catalog_id).
        let data = json!({"catalog_id": "cat-9", "name": "thing"});
        let schema = make_schema_with_fields(&[("catalog_id", FieldLocation::Query)], None, None);
        let params = build_scoping_params(&data, &schema).unwrap().expect("scoping params present");
        assert_eq!(params.get("catalog_id").map(String::as_str), Some("cat-9"));

        // An unresolved ${...} template in a dotted query field must Err so the
        // caller skips the list call rather than sending the literal template string.
        let data = json!({"target": {"target_id": "${subscription.os_sub.id}"}});
        let schema = make_schema_with_fields(&[("target.target_id", FieldLocation::Query)], None, None);
        assert!(build_scoping_params(&data, &schema).is_err());
    }

    #[test]
    fn substitute_path_placeholders_branches() {
        let endpoint = "/v1/parents/{catalog_id}/things";
        // Path-located field substituted into its placeholder.
        let schema = make_schema_with_fields(&[("catalog_id", FieldLocation::Path)], None, None);
        assert_eq!(substitute_path_placeholders(endpoint, &json!({"catalog_id": "cat-123"}), &schema).unwrap(), "/v1/parents/cat-123/things");
        // No matching placeholder in the endpoint → passes through unchanged.
        assert_eq!(substitute_path_placeholders("/v1/things", &json!({"catalog_id": "cat-123"}), &schema).unwrap(), "/v1/things");
        // Missing value for a declared Path placeholder → error.
        assert!(substitute_path_placeholders(endpoint, &json!({}), &schema).is_err());
        // A Body-located field `catalog_id` with the same name still shouldn't get
        // substituted into paths — only Path-located fields are candidates.
        let body_schema = make_schema_with_fields(&[("catalog_id", FieldLocation::Body)], None, None);
        assert_eq!(substitute_path_placeholders(endpoint, &json!({"catalog_id": "cat-123"}), &body_schema).unwrap(), "/v1/parents/{catalog_id}/things");
    }

    fn make_schema_with_immutable(immutable_fields: Vec<String>, reject: bool) -> ResourceSchema {
        use wxctl_core::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, FieldDefinition, FieldLocation, FieldType, HookDefinition, HttpMethod, ReconciliationDefinition, ResourceDefinition, ResourceSchema, SchemaDefinition, UpdateStrategy};
        ResourceSchema {
            resource: ResourceDefinition {
                name: "reg".into(),
                service: "svc".into(),
                kind: "reg".into(),
                version: "v1".into(),
                api: ApiDefinition {
                    base_path: "/v3/regs".into(),
                    id_field: "id".into(),
                    list_endpoint: Some("/v3/regs".into()),
                    get_endpoint: "/v3/regs/{id}".into(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: Some(HttpMethod::Patch),
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                },
                schema: SchemaDefinition {
                    fields: vec![FieldDefinition {
                        name: "type".into(),
                        field_type: FieldType::String,
                        required: true,
                        immutable: true,
                        location: FieldLocation::Body,
                        description: None,
                        validation: None,
                        schema: None,
                        item_type: None,
                        default: None,
                        allowed_values: None,
                        references: None,
                        api_field: None,
                        sensitive: false,
                        also_query: false,
                        is_path: false,
                        properties: None,
                    }],
                    ..Default::default()
                },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition { method: DiscoveryMethod::ListAndGet, list_field: None, name_field: None, identity_match: None, absent_when: None, list_method: None, list_body: None, id_source: "id".into() },
                    state_fields: Some(vec![]),
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields,
                    reject_on_immutable_drift: reject,
                    use_json_patch: false,
                    json_patch_path_prefix: None,
                },
                hooks: HookDefinition::default(),
                deployments: None,
                unsupported_on: vec![],
                description: None,
                prompt: None,
            },
        }
    }

    fn make_test_resources(local_data: Value, remote_data: Value, schema: &ResourceSchema) -> (ValidatedResource, RemoteResource) {
        use wxctl_core::registry::ResourceDescriptor;
        use wxctl_core::types::ResourceKey;
        let descriptor = std::sync::Arc::new(ResourceDescriptor::from_schema(schema).unwrap());
        let key = ResourceKey::new("reg", "test");
        let local = ValidatedResource { key: key.clone(), data: local_data, descriptor, dependencies: vec![], on_destroy: Default::default() };
        let remote = RemoteResource { key, data: remote_data, exists: true };
        (local, remote)
    }

    #[test]
    fn compare_immutable_drift_recreate_vs_no_change() {
        let reconciler = SchemaBasedReconciler::new();
        // Drift on an immutable field → Recreate carrying the drifted field info,
        // regardless of reject_on_immutable_drift: the flag does not affect the
        // reconciler return shape — the pipeline reads the flag and converts the
        // Recreate into a ReconciliationError. The reconciler itself still reports
        // the drifted field so callers can build an error message.
        for reject in [true, false] {
            let schema = make_schema_with_immutable(vec!["type".into()], reject);
            let (lv, rv) = make_test_resources(json!({"type": "ibm_cos"}), json!({"type": "amazon_s3"}), &schema);
            match reconciler.compare(&lv, &rv) {
                StateComparison::Recreate { field, local_value, remote_value } => {
                    assert_eq!(field, "type");
                    assert_eq!(local_value, "ibm_cos");
                    assert_eq!(remote_value, "amazon_s3");
                }
                other => panic!("expected Recreate (reject={reject}), got {:?}", other),
            }
        }
        // No drift → NoChange.
        let schema = make_schema_with_immutable(vec!["type".into()], true);
        let (lv, rv) = make_test_resources(json!({"type": "ibm_cos"}), json!({"type": "ibm_cos"}), &schema);
        assert!(matches!(reconciler.compare(&lv, &rv), StateComparison::NoChange));
    }

    /// Build a storage_registration-shaped schema: state_fields
    /// [display_name, description, tags] + immutable_fields [bucket,
    /// associated_catalog.catalog_name, associated_catalog.catalog_type] +
    /// reject_on_immutable_drift, with an identity_match on the catalog name.
    /// Used to reproduce the Deferred-but-found re-plan regression where the
    /// templated `bucket` immutable ref must not trigger a phantom Recreate.
    fn make_storage_registration_schema() -> ResourceSchema {
        use wxctl_core::schema::{ApiDefinition, DiscoveryDefinition, DiscoveryMethod, HookDefinition, HttpMethod, IdentityMatch, ReconciliationDefinition, ResourceDefinition, ResourceSchema, SchemaDefinition, UpdateStrategy};
        ResourceSchema {
            resource: ResourceDefinition {
                name: "storage_registration".into(),
                service: "watsonx_data".into(),
                kind: "storage_registration".into(),
                version: "v1".into(),
                api: ApiDefinition {
                    base_path: "/v3/storage_registrations".into(),
                    id_field: "id".into(),
                    list_endpoint: Some("/v3/storage_registrations".into()),
                    get_endpoint: "/v3/storage_registrations/{id}".into(),
                    create_endpoint: None,
                    create_method: HttpMethod::Post,
                    update_endpoint: None,
                    update_method: Some(HttpMethod::Patch),
                    delete_endpoint: None,
                    delete_method: HttpMethod::Delete,
                },
                schema: SchemaDefinition { fields: vec![], ..Default::default() },
                reconciliation: ReconciliationDefinition {
                    discovery: DiscoveryDefinition {
                        method: DiscoveryMethod::ListAndGet,
                        list_field: Some("storage_registrations".into()),
                        name_field: None,
                        identity_match: Some(IdentityMatch { local_path: "associated_catalog.catalog_name".into(), remote_path: "associated_catalogs.0.catalog_name".into() }),
                        absent_when: None,
                        list_method: None,
                        list_body: None,
                        id_source: "id".into(),
                    },
                    state_fields: Some(vec!["display_name".into(), "description".into(), "tags".into()]),
                    update_strategy: UpdateStrategy::Patch,
                    immutable_fields: vec!["bucket".into(), "associated_catalog.catalog_name".into(), "associated_catalog.catalog_type".into()],
                    reject_on_immutable_drift: true,
                    use_json_patch: false,
                    json_patch_path_prefix: None,
                },
                hooks: HookDefinition::default(),
                deployments: None,
                unsupported_on: vec![],
                description: None,
                prompt: None,
            },
        }
    }

    /// Deferred-but-found storage_registration re-plan regression. On this path the
    /// local `bucket` immutable field is still `${s3_bucket....name}` (its
    /// skip-discovery dependency is never cached). The remote is the post-discover
    /// backfilled shape (singular `associated_catalog`, plural `associated_catalogs`,
    /// and the state fields); the user-set `bucket` ref name is never echoed by the API.
    /// `compare` must skip the templated `bucket` immutable PER-FIELD (not all-or-nothing)
    /// and still surface real drift. The three rows cover: unchanged → NoChange (NOT a
    /// phantom Recreate, which with reject_on_immutable_drift would become a hard
    /// reconciliation error); a changed state field (display_name) → Update (the
    /// templated-field skip must not suppress real drift on resolved fields); and drift
    /// on a RESOLVED immutable field (catalog_type), sibling to the templated/skipped
    /// `bucket` → Recreate.
    #[test]
    fn compare_deferred_storage_registration_diff_branches() {
        let schema = make_storage_registration_schema();
        let reconciler = SchemaBasedReconciler::new();

        // unchanged → NoChange
        let local = json!({
            "display_name": "e2e-sw-iceberg",
            "description": "e2e cos-lakehouse-sw COS registration",
            "bucket": "${s3_bucket.sw_bucket.name}",
            "associated_catalog": {"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"}
        });
        let remote = json!({
            "id": "reg-123",
            "display_name": "e2e-sw-iceberg",
            "description": "e2e cos-lakehouse-sw COS registration",
            "catalog_name": "e2e_sw_iceberg",
            "associated_catalog": {"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"},
            "associated_catalogs": [{"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"}]
        });
        let (lv, rv) = make_test_resources(local, remote, &schema);
        match reconciler.compare(&lv, &rv) {
            StateComparison::NoChange => {}
            other => panic!("expected NoChange (templated immutable `bucket` must be skipped), got {:?}", other),
        }

        // changed state field (display_name) → Update
        let local = json!({
            "display_name": "renamed-display",
            "description": "e2e cos-lakehouse-sw COS registration",
            "bucket": "${s3_bucket.sw_bucket.name}",
            "associated_catalog": {"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"}
        });
        let remote = json!({
            "id": "reg-123",
            "display_name": "e2e-sw-iceberg",
            "description": "e2e cos-lakehouse-sw COS registration",
            "associated_catalog": {"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"},
            "associated_catalogs": [{"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"}]
        });
        let (lv, rv) = make_test_resources(local, remote, &schema);
        match reconciler.compare(&lv, &rv) {
            StateComparison::Update { fields } => assert_eq!(fields, vec!["display_name".to_string()]),
            other => panic!("expected Update on display_name, got {:?}", other),
        }

        // drift on a resolved immutable field (catalog_type) → Recreate
        let local = json!({
            "display_name": "e2e-sw-iceberg",
            "bucket": "${s3_bucket.sw_bucket.name}",
            "associated_catalog": {"catalog_name": "e2e_sw_iceberg", "catalog_type": "hive"}
        });
        let remote = json!({
            "id": "reg-123",
            "display_name": "e2e-sw-iceberg",
            "associated_catalog": {"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"},
            "associated_catalogs": [{"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"}]
        });
        let (lv, rv) = make_test_resources(local, remote, &schema);
        match reconciler.compare(&lv, &rv) {
            StateComparison::Recreate { field, .. } => assert_eq!(field, "associated_catalog.catalog_type"),
            other => panic!("expected Recreate on catalog_type, got {:?}", other),
        }
    }

    #[test]
    fn compared_field_resolution_counts() {
        // Mixed: display_name + description + catalog_name + catalog_type resolved (4),
        // bucket templated (1), tags absent (skipped).
        let schema = make_storage_registration_schema();
        let data = json!({
            "display_name": "e2e-sw-iceberg",
            "description": "desc",
            "bucket": "${s3_bucket.sw_bucket.name}",
            "associated_catalog": {"catalog_name": "e2e_sw_iceberg", "catalog_type": "iceberg"}
        });
        assert_eq!(compared_field_resolution(&data, &schema), (4, 1));

        // When every present compared field is templated, comparable == 0 — the
        // Deferred-Apply caller keeps the conservative blind Update.
        let schema = make_schema_with_immutable(vec!["type".into()], false);
        let data = json!({"type": "${storage_connection.c.type}"});
        assert_eq!(compared_field_resolution(&data, &schema), (0, 1));
    }

    #[test]
    fn value_has_unresolved_template_detects_nested_and_scalar() {
        assert!(value_has_unresolved_template(&json!("${s3_bucket.x.name}")));
        assert!(value_has_unresolved_template(&json!({"a": {"b": "${conn.c}"}})));
        assert!(value_has_unresolved_template(&json!(["lit", "${conn.c}"])));
        assert!(!value_has_unresolved_template(&json!("literal")));
        assert!(!value_has_unresolved_template(&json!({"a": "literal"})));
        assert!(!value_has_unresolved_template(&json!(null)));
    }

    /// `compare` must read immutable_fields from the per-resource
    /// descriptor (which the validation pipeline rebuilds with overlay
    /// merges applied), not from any captured base. Using a Software-style
    /// `job_id` immutable field — which the SaaS base wouldn't carry —
    /// catches any regression that reintroduces a captured-base read path.
    #[test]
    fn compare_reads_immutable_fields_from_per_resource_descriptor() {
        let overlay_schema = make_schema_with_immutable(vec!["job_id".into()], false);
        let local = json!({"job_id": "ingest-001"});
        let remote = json!({"job_id": "ingest-002"});
        let (lv, rv) = make_test_resources(local, remote, &overlay_schema);
        let reconciler = SchemaBasedReconciler::new();
        match reconciler.compare(&lv, &rv) {
            StateComparison::Recreate { field, .. } => assert_eq!(field, "job_id"),
            other => panic!("expected Recreate on job_id, got {:?}", other),
        }
    }

    #[test]
    fn render_value_handles_common_json_shapes() {
        assert_eq!(render_value(&json!("hello")), "hello");
        assert_eq!(render_value(&json!(null)), "null");
        assert_eq!(render_value(&json!(42)), "42");
        assert_eq!(render_value(&json!(true)), "true");
        assert_eq!(render_value(&json!({"a": 1})), "{\"a\":1}");
    }

    #[test]
    fn denormalize_api_field_to_user_field_branches() {
        use wxctl_core::schema::{FieldDefinition, FieldLocation, FieldType, SchemaDefinition};
        // A category schema mapping user field `parent_category` ← api_field `parent_category_id`.
        let mut schema = SchemaDefinition::default();
        schema.fields = vec![FieldDefinition {
            name: "parent_category".into(),
            field_type: FieldType::String,
            required: false,
            immutable: false,
            location: FieldLocation::Body,
            description: None,
            validation: None,
            schema: None,
            item_type: None,
            default: None,
            allowed_values: None,
            references: None,
            api_field: Some("parent_category_id".into()),
            sensitive: false,
            also_query: false,
            properties: None,
            is_path: false,
        }];

        // Top-level api_field → mapped onto the user field.
        let mut resp = json!({"name": "e2e PII", "parent_category_id": "cat-root-1"});
        denormalize_api_response(&mut resp, &schema);
        assert_eq!(resp.get("parent_category").and_then(|v| v.as_str()), Some("cat-root-1"));

        // Discovered CP4D category shape: parent id under `entity` envelope → mapped.
        let mut resp = json!({"metadata": {"name": "e2e PII"}, "entity": {"parent_category_id": "cat-root-1"}});
        denormalize_api_response(&mut resp, &schema);
        assert_eq!(resp.get("parent_category").and_then(|v| v.as_str()), Some("cat-root-1"));

        // A root category (no parent) leaves `parent_category` unset → a real diff still surfaces.
        let mut resp = json!({"metadata": {"name": "e2e Glossary Domain"}, "entity": {}});
        denormalize_api_response(&mut resp, &schema);
        assert!(resp.get("parent_category").is_none());
    }

    #[test]
    fn business_term_state_fields_round_trip_to_nochange_against_cp4d_list_envelope() {
        // Gap B (2026-06-16): once parent_category is no longer immutable-compared,
        // compare() falls through to the state_fields loop. This proves the term's
        // compared state_fields (name, short_description, long_description,
        // abbreviations, tags — the fields term_email sets in the cell config)
        // round-trip discovered↔local → NoChange, NOT a residual Update. The live
        // CP4D LIST nests name+short_description+tags under `metadata` and
        // abbreviations+long_description under `entity`; get_nested_field's envelope
        // fallback hoists single-segment paths from entity/metadata.
        let local = json!({
            "name": "e2e Customer Email",
            "short_description": "A customer's email address.",
            "long_description": "The email address used to contact a customer; treated as PII.",
            "abbreviations": ["EMAIL"],
            "tags": ["e2e", "pii"]
        });
        let remote = json!({
            "metadata": {
                "name": "e2e Customer Email",
                "short_description": "A customer's email address.",
                "tags": ["e2e", "pii"],
                "artifact_id": "term-1"
            },
            "entity": {
                "abbreviations": ["EMAIL"],
                "long_description": "The email address used to contact a customer; treated as PII."
            }
        });
        for field in ["name", "short_description", "long_description", "abbreviations", "tags"] {
            let lv = get_nested_field(&local, field);
            let rv = get_nested_field(&remote, field);
            assert!(values_match(lv, rv), "state field `{field}` must round-trip discovered↔local (local={lv}, remote={rv})");
        }
    }
}
