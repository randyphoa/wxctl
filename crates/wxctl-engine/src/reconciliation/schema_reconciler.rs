//! Schema-based reconciler implementation.

use crate::templates::is_template;
use anyhow::{Context, Result, bail};
use reqwest::Method;
use serde_json::Value;
use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, BodyKindSelector, HttpClient, RequestMaterializer, RequestSpec, error_has_status, lookup_nested};
use wxctl_core::traits::{AdvisorySink, NoOpAdvisorySink, Reconciler, StateComparison};
use wxctl_core::types::{RemoteResource, ValidatedResource};
use wxctl_schema::ir::{AbsentWhenIr, DiscoveryMethodIr, FieldLocationIr, HashStorageIr, IdentityMatchIr, ListFilterIr, SchemaBodyIr, SchemaIr};

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
fn build_scoping_params(data: &Value, schema: &SchemaIr) -> Result<Option<HashMap<String, String>>> {
    let mut params = HashMap::new();

    for field in schema.resource.schema.fields {
        if !matches!(field.location, FieldLocationIr::Query) && !field.also_query {
            continue;
        }
        // Resolve via get_nested_field so a dotted Query field name (e.g.
        // `target.target_id`) reads the nested value `data["target"]["target_id"]`
        // and is sent as `?target.target_id=<id>`. Single-segment names traverse
        // through the same `map.get`, so flat scoping fields are unchanged.
        match get_nested_field(data, field.name) {
            Value::String(val) => {
                if is_template(val) {
                    bail!("unresolved template reference in scoping parameter: {}", field.name);
                }
                params.insert(field.name.to_string(), val.to_string());
            }
            // A whole object/array here is the leftover of a reference that could
            // not be extracted to an id (e.g. destroy against an absent stack: the
            // bare `${project.x}` ref resolves to the cache-seeded local object,
            // which carries no guid). Sending the LIST without the param 400s on
            // scoped endpoints — treat it like an unresolved template so the
            // caller skips the call.
            v @ (Value::Object(_) | Value::Array(_)) => {
                let _ = v;
                bail!("unresolvable scoping parameter '{}': reference did not extract to an id", field.name);
            }
            // Null/absent (optional field never set) and non-string scalars keep
            // their existing behavior: no param emitted.
            _ => {}
        }
    }

    if params.is_empty() { Ok(None) } else { Ok(Some(params)) }
}

/// Substitute `{field}` path placeholders in a list/get endpoint using the
/// values of fields declared with `location: Path` in the schema. The
/// list-call machinery otherwise passes endpoints through verbatim — schemas
/// like `watsonx_data.schema` whose `list_endpoint` carries a `{catalog_id}`
/// segment would 400 against the literal template string.
fn substitute_path_placeholders(endpoint: &str, data: &Value, schema: &SchemaIr) -> Result<String> {
    let mut out = endpoint.to_string();
    for field in schema.resource.schema.fields {
        if !matches!(field.location, FieldLocationIr::Path) {
            continue;
        }
        let placeholder = format!("{{{}}}", field.name);
        if !out.contains(&placeholder) {
            continue;
        }
        let value = data.get(field.name).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("path placeholder `{placeholder}` has no resolved value in resource data"))?;
        out = out.replace(&placeholder, value);
    }
    Ok(out)
}

/// GET-based list call for a schema that declares `list_field` explicitly. An
/// undeclared `list_field` lets `list_with_params_absent_ok` auto-detect the
/// envelope via `ListEnvelope`, which recognizes a bare array or a present
/// (possibly empty) array under a guessed key — but it cannot represent "the key
/// is omitted entirely when the list is empty" (e.g. Pliant's folders GET, whose
/// `flows` key the server drops altogether for an empty folder, rather than
/// sending an empty array). A declared `list_field` sidesteps auto-detection
/// entirely: extract `resp[field]` directly, treating an absent key as an empty
/// list. Mirrors `list_with_params_absent_ok`'s own RequestSpec construction
/// (GET, `not_found_ok`, `stage("reconciliation")`, `sensitive_paths`) and the
/// `list_method: post` branch's query-param handling and field extraction.
async fn list_via_declared_field(client: &HttpClient, operation_id: &str, list_endpoint: &str, params: &Option<HashMap<String, String>>, sensitive_paths: Vec<String>, field: &str) -> Result<Vec<Value>> {
    let mut spec = RequestSpec::new(Method::GET, list_endpoint).body(BodyKind::None).not_found_ok().stage("reconciliation").sensitive_paths(sensitive_paths);
    if let Some(params) = params {
        for (k, v) in params {
            spec = spec.query_param(k, v);
        }
    }
    let resp = client.execute::<Value>(operation_id, spec).await?;
    Ok(extract_declared_field(resp, field))
}

/// Pull the item array out of a list response whose schema declares `list_field`.
/// Absent key = empty list (the whole point of declaring the field — see
/// `list_via_declared_field`). Bare-array fallback: some APIs return a bare array
/// from the same endpoint on other deployments/versions (live 2026-07-04: wxO SaaS
/// `GET /v1/orchestrate/models` is a bare array while the schema declares
/// `list_field: models`) — extracting `resp[field]` from an array yields nothing,
/// which reads as "resource gone" (phantom Create on re-plan, silent destroy skip,
/// duplicate POST on re-apply). A bare array can only be the list itself, so use it
/// directly.
fn extract_declared_field(resp: Value, field: &str) -> Vec<Value> {
    if let Value::Array(items) = resp {
        return items;
    }
    resp.get(field).and_then(|v| v.as_array()).cloned().unwrap_or_default()
}

/// Return the first unresolved `${...}` template found along paths the list
/// call actually needs to see literal values for: schema-declared Path/Query
/// fields (which feed the URL and query string) plus the identity field
/// compared against remote list items. Templates elsewhere in `data` (e.g. a
/// bucket ref on storage_registration whose identity path is a hardcoded
/// catalog_name) do NOT block discovery.
pub(super) fn identity_paths_unresolved(data: &Value, schema: &SchemaIr) -> Option<String> {
    for field in schema.resource.schema.fields {
        if !matches!(field.location, FieldLocationIr::Path | FieldLocationIr::Query) && !field.also_query {
            continue;
        }
        if let Some(s) = get_nested_field(data, field.name).as_str()
            && is_template(s)
        {
            return Some(s.to_string());
        }
    }
    let discovery = &schema.resource.reconciliation.discovery;
    if let Some(im) = &discovery.identity_match {
        if let Some(s) = get_nested_field(data, im.local_path).as_str()
            && is_template(s)
        {
            return Some(s.to_string());
        }
    } else {
        let name_field = discovery.name_field.unwrap_or("name");
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
pub(super) fn compared_field_resolution(data: &Value, schema: &SchemaIr) -> (usize, usize) {
    let reconciliation = &schema.resource.reconciliation;
    let state_fields = reconciliation.state_fields.unwrap_or(&[]);
    let (mut comparable, mut templated) = (0usize, 0usize);
    for field in state_fields.iter().copied().chain(reconciliation.immutable_fields.iter().copied()) {
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

/// True when `data` matches the schema's `absent_when` sentinel: the value at
/// `absent_when.field` (the `Null` sentinel if the field is missing — see
/// `get_nested_field`) equals `absent_when.equals`. `None` (no `absent_when`
/// declared) never matches. Shared by two discovery shapes: the `Singleton`
/// arm (a 200 body that is really "not enabled") and the `ListAndGet` arm (an
/// identity-matched item whose state field indicates the record is a leftover
/// husk, not a live resource — e.g. watsonx Orchestrate's undeployed
/// `Environment` record with `current_version: null`).
fn is_absent_sentinel(data: &Value, absent_when: Option<&AbsentWhenIr>) -> bool {
    absent_when.is_some_and(|aw| {
        let equals: Value = serde_json::from_str(aw.equals).expect("canonical json absent_when.equals");
        *get_nested_field(data, aw.field) == equals
    })
}

/// Filter `items` to those matching the schema-declared identity. The schema
/// declares exactly one of: `identity_match` (different local vs. remote paths) or
/// `name_field` (single same-path lookup, default `name`). For `storage: tag`
/// identity-hash kinds, `run_hash` further requires the item's `run-hash:` tag to
/// equal the desired hash (so a prior generation with the same base name is not
/// matched); for every other kind `run_hash` is `None` and imposes no filter.
/// For `storage: env_marker` kinds (`env_marker_hash` Some), the name is ignored
/// entirely — the server clobbers it (job_run → "Notebook Job" on both CPDaaS and
/// CP4D) — and items match solely on the WXCTL_IDENTITY entry round-tripped in
/// `configuration.env_variables`; matches are ordered Completed-first (then
/// in-flight, then failed) so `discover`'s first-match pick and post_discover's
/// enrichment adopt the stable run when the pre-fix create-loop left duplicates.
#[allow(clippy::too_many_arguments)]
fn match_remote_items<'a>(items: &'a [Value], resource_data: &Value, name_field: &str, resource_name: Option<&str>, identity: Option<&IdentityMatchIr>, run_hash: Option<&str>, env_marker_hash: Option<&str>, list_filter: Option<&ListFilterIr>) -> Vec<&'a Value> {
    if let Some(hash) = env_marker_hash {
        let mut matched: Vec<&Value> = items.iter().filter(|item| wxctl_providers::extract_identity_env_marker(item) == Some(hash)).collect();
        matched.sort_by_key(|item| wxctl_providers::job_run_state_rank(item));
        return matched;
    }
    if let Some(im) = identity {
        let Some(target) = get_nested_field(resource_data, im.local_path).as_str() else {
            return Vec::new();
        };
        return items.iter().filter(|item| get_nested_field(item, im.remote_path).as_str() == Some(target)).collect();
    }
    let Some(resource_name) = resource_name else {
        return Vec::new();
    };
    items.iter().filter(|item| name_matches(item, resource_name, name_field) && run_hash.is_none_or(|h| wxctl_providers::extract_run_hash(item) == Some(h)) && list_filter.is_none_or(|lf| get_nested_field(item, lf.field).as_str() == Some(lf.equals))).collect()
}

/// R501: within a `list_filter` kind's discovery, a same-named item whose filter
/// field holds a different value is a cross-type name collision. Emit the warn-level
/// tracing event per colliding item (telemetry, byte-identical to before) and push
/// one deduped advisory per (resource, conflicting value) onto `sink`.
#[allow(clippy::too_many_arguments)]
fn push_list_filter_advisories(items: &[Value], lf: &ListFilterIr, resource_name: &str, name_field: &str, run_hash: Option<&str>, key_kind: &str, key_name: &str, sink: &dyn AdvisorySink) {
    const R501_SUGGESTION: &str = "If the create is rejected, rename this resource so its name does not collide with the existing item of a different type.";
    let mut seen: HashSet<String> = HashSet::new();
    for item in items {
        if name_matches(item, resource_name, name_field) && run_hash.is_none_or(|h| wxctl_providers::extract_run_hash(item) == Some(h)) && get_nested_field(item, lf.field).as_str() != Some(lf.equals) {
            let found = render_value(get_nested_field(item, lf.field));
            let message = format!("a same-named item exists with {}='{}' (expected '{}'); '{}' is absent and will be created — a backend enforcing cross-type name uniqueness may then reject the create", lf.field, found, lf.equals, key_name);
            wxctl_core::log_warn_resource_field!(wxctl_core::logging::error_codes::R501, key_kind, key_name, lf.field, found, std::slice::from_ref(&lf.equals), message);
            if seen.insert(found.clone()) {
                sink.push(wxctl_core::logging::error_codes::R501, &format!("{}/{}", key_kind, key_name), &message, R501_SUGGESTION);
            }
        }
    }
}

/// For identity-hash kinds, stamp `identity_hash` on a matched+denormalized remote
/// so `compare` — which drops the name field and compares the synthetic
/// `identity_hash` state field — sees equal hashes → NoChange. Reads the hash from
/// the `run-hash:` tag (Tag), the name-field suffix after the final `-`
/// (NameSuffix), or the WXCTL_IDENTITY entry in the round-tripped
/// `configuration.env_variables` (EnvMarker). No-op for ServerSide and for
/// non-hash kinds.
fn normalize_identity_hash(remote_data: &mut Value, schema: &SchemaIr) {
    let def = &schema.resource;
    let Some(ih) = def.reconciliation.identity_hash.as_ref() else {
        return;
    };
    let name_field = def.reconciliation.discovery.name_field.unwrap_or("name");
    let hash = match ih.storage {
        HashStorageIr::Tag => wxctl_providers::extract_run_hash(remote_data).map(str::to_string),
        HashStorageIr::NameSuffix => get_nested_field(remote_data, name_field).as_str().and_then(|n| n.rsplit_once('-')).map(|(_, suffix)| suffix.to_string()),
        HashStorageIr::EnvMarker => wxctl_providers::extract_identity_env_marker(remote_data).map(str::to_string),
        HashStorageIr::ServerSide => None,
        // Local kinds never reach ListAndGet normalization (discovery: skip).
        HashStorageIr::Local => None,
    };
    if let Some(h) = hash
        && let Some(obj) = remote_data.as_object_mut()
    {
        obj.insert("identity_hash".to_string(), Value::String(h));
    }
}

/// Q2 local-hash fallback (identity_hash.storage: local — sal_*): a Skip-discovery
/// kind whose desired hash is already in the local record store is reported as
/// existing, with data = the desired data (explicit state_fields: [] ⇒ compare is
/// trivially NoChange). No record ⇒ None ⇒ the caller falls through to the normal
/// always-create Skip behavior (fresh machine: one re-run, then idempotent).
/// Read-only — safe during plan.
fn local_hash_skip_match(schema: &SchemaIr, resource: &ValidatedResource, store_root: &std::path::Path, env: &str) -> Option<RemoteResource> {
    let ih = schema.resource.reconciliation.identity_hash.as_ref()?;
    if !matches!(ih.storage, HashStorageIr::Local) {
        return None;
    }
    let hash = resource.data.get("identity_hash").and_then(|v| v.as_str())?;
    if wxctl_providers::local_hash::has_run_hash_at(store_root, env, &resource.key.kind, &resource.key.name, hash) { Some(RemoteResource { key: resource.key.clone(), data: resource.data.clone(), exists: true }) } else { None }
}

/// Build the discovery GET request for `DiscoveryMethod::GetById`. The id
/// placeholder is string-replaced first (`id_source` is typically a
/// Body-located field such as `name`, which the materializer would not place
/// in `path_vars`), then the request is materialized so any OTHER
/// `location: Path`/`Query` field interpolates too — mirroring the
/// `Singleton` arm below. Without this, a parent-scoped kind whose
/// `get_endpoint` carries more than the id placeholder (e.g. Planning
/// Analytics `/Dimensions('{dimension}')/Hierarchies('{name}')`) sends the
/// literal `{dimension}` text in the URL; the live TM1 server 404s
/// (`'{dimension}' can not be found`), discovery reports not-found, and every
/// re-plan shows a phantom `+ create` for the child kind (live-proven:
/// pa_hierarchy / pa_subset / pa_view).
///
/// If a Path field is itself still `${ref}`-templated (unresolved
/// cross-kind reference), the materializer inserts that literal `${...}`
/// string into `path_vars` — the resulting GET still 404s and discovery
/// still reports not-found (phantom Create), matching today's net behavior.
/// `identity_paths_unresolved` (used on the Deferred reconciliation path in
/// `pipeline.rs`, surfacing `CreateUnchecked`/"unchecked: ...") continues to
/// cover that case; this fix only removes the *additional* phantom-create
/// trigger for non-templated parent-scoped fields that were previously left
/// as literal `{placeholder}` text.
fn build_get_by_id_spec(schema: &SchemaIr, resource_data: &Value, id_source_field: &str, resource_id: &str) -> Result<RequestSpec> {
    let def = &schema.resource;
    let endpoint = def.api.get_endpoint.replace(&format!("{{{}}}", id_source_field), resource_id);

    // 404 = resource absent (plan Create) — not_found_ok() suppresses the
    // wxctl::error event so the output collector doesn't count it as a failure.
    // `materialize` sets `sensitive_paths` from the flat field slice only (no
    // variant-scoped fields); chain the resource-level `sensitive_paths()` after
    // it (which also walks `variants` and adds the CAMS response-envelope
    // spellings) so the GET response is redacted like the LIST path.
    Ok(RequestMaterializer::new(Method::GET, &endpoint).materialize(resource_data, schema.resource.schema.fields, BodyKindSelector::None)?.sensitive_paths(schema.resource.sensitive_paths()).not_found_ok().stage("reconciliation"))
}

impl Reconciler for SchemaBasedReconciler {
    fn discover<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient) -> Pin<Box<dyn Future<Output = Result<RemoteResource>> + Send + 'a>> {
        Box::pin(async move {
            let schema = resource.descriptor.schema;
            let def = &schema.resource;
            let discovery = &def.reconciliation.discovery;
            let _id_field = def.api.id_field;

            match discovery.method {
                DiscoveryMethodIr::ListAndGet => {
                    // Delegate to discover_all's ListAndGet arm — the single copy of the
                    // list/match/normalize path (list_field handling, POST-search, 404/403
                    // quirks) — and take the first match. Keeping one implementation means
                    // every list-path fix applies to both entry points.
                    let mut matches = self.discover_all(operation_id, resource, client, &NoOpAdvisorySink).await?;
                    if matches.is_empty() { Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false }) } else { Ok(matches.swap_remove(0)) }
                }
                DiscoveryMethodIr::GetById => {
                    // Try to get resource by ID directly using the id_source field
                    let id_source_field = discovery.id_source;
                    // A server-minted id is absent until the resource is created. With no id to
                    // GET, the resource cannot exist remotely under our reference, so treat it as
                    // not-found (plan Create) rather than erroring. (Kinds whose id is
                    // client-supplied, e.g. ingestion_job, always have it populated here.)
                    let Some(resource_id) = resource.data.get(id_source_field).and_then(|v| v.as_str()) else {
                        tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, id_source = %id_source_field, "get_by_id: id source absent — treating as not found (create)");
                        return Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false });
                    };

                    // build_get_by_id_spec materializes location: Path/Query fields into
                    // the URL (not just the id placeholder) — see its doc comment for why.
                    let spec = build_get_by_id_spec(schema, &resource.data, id_source_field, resource_id)?;
                    match client.execute::<Value>(operation_id, spec).await {
                        Ok(remote_data) => {
                            // absent_when: the GET can return 200 for a CONTAINER whose target
                            // sub-resource is absent — e.g. a Vault identity group
                            // (GET /identity/group/id/{canonical_id}) whose embedded `alias` is
                            // empty (`data.alias.id: null`) until a vault_group_alias is created.
                            // Treat the sentinel as not-found (plan Create), same as the Singleton arm.
                            if is_absent_sentinel(&remote_data, discovery.absent_when.as_ref()) {
                                return Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false });
                            }
                            // Denormalize API response to add user-facing fields from nested api_field paths
                            let mut data = remote_data;
                            denormalize_api_response(&mut data, &schema.resource.schema);
                            Ok(RemoteResource { key: resource.key.clone(), data, exists: true })
                        }
                        Err(e) => {
                            // Only treat 404 as "not found" — propagate network/server errors.
                            // The HTTP client converts errors to anyhow via retry.rs, losing the
                            // typed status code; error_has_status parses the "HTTP 404 ..." message
                            // produced by HttpError::with_status in http.rs.
                            let is_not_found = error_has_status(&e, 404);

                            if is_not_found { Ok(RemoteResource { key: resource.key.clone(), data: Value::Null, exists: false }) } else { Err(e).context(format!("Failed to discover {} '{}'", resource.key.kind, resource.key.name)) }
                        }
                    }
                }
                DiscoveryMethodIr::Skip => {
                    // Q2 local-hash fallback: consult the local record store first.
                    if let Some(found) = local_hash_skip_match(schema, resource, &wxctl_core::logging::run_record::runs_root(), &wxctl_providers::local_hash::env_key(client.base_url())) {
                        tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "local-hash record matched — treating as existing (NoChange)");
                        return Ok(found);
                    }
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
                DiscoveryMethodIr::Singleton => {
                    // Per-instance singleton (e.g. sal_integration, sal_global_settings):
                    // GET the id-less get_endpoint. Materialize the request so a singleton's
                    // location: Query/Path fields (e.g. sal_enrichment_settings.project_id)
                    // flow into the URL — a raw GET drops a required query param and 400s.
                    // A non-empty 200 is the one existing instance; an empty body or 404
                    // means absent (plan Create / "enable").
                    // not_found_ok() suppresses the wxctl::error event for 404 — the Err
                    // branch below still distinguishes HTML (bad route) vs clean 404 (absent).
                    let spec = RequestMaterializer::new(Method::GET, def.api.get_endpoint).materialize(&resource.data, schema.resource.schema.fields, BodyKindSelector::None)?.not_found_ok().stage("reconciliation");
                    match client.execute::<Value>(operation_id, spec).await {
                        Ok(remote_data) => {
                            // Some singletons return 200 with a sentinel body when absent (e.g. SAL's
                            // GET /v3/sal_integration → {"status":"missing"} until enabled). Honor the
                            // schema's `absent_when` so that reads as absent (plan Create) not Update.
                            let absent_sentinel = is_absent_sentinel(&remote_data, discovery.absent_when.as_ref());
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
                            let is_not_found = error_has_status(&e, 404);
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

    fn discover_all<'a>(&'a self, operation_id: &'a str, resource: &'a ValidatedResource, client: HttpClient, advisories: &'a dyn AdvisorySink) -> Pin<Box<dyn Future<Output = Result<Vec<RemoteResource>>> + Send + 'a>> {
        Box::pin(async move {
            let descriptor_schema = resource.descriptor.schema;
            let def = &descriptor_schema.resource;
            let discovery = &def.reconciliation.discovery;

            match discovery.method {
                DiscoveryMethodIr::ListAndGet => {
                    // List all resources and find ALL matching ones
                    let list_endpoint = def.api.list_endpoint.ok_or_else(|| anyhow::anyhow!("list_endpoint not configured"))?;

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
                    // Schema-driven sensitive paths thread through to the HTTP client so a LIST
                    // response echoing credential-shaped fields (e.g. an alerting channel's
                    // `webhookUrls`) gets redacted the same way create/update bodies do.
                    // The resource-level superset also carries the CAMS response-envelope
                    // spellings (`[results.]entity.<kind>.<path>`) — job/job_run LIST bodies
                    // echo `configuration.env_variables` there in plaintext (live-pinned
                    // 2026-07-05), unreachable from the bare field paths.
                    let sensitive_paths = descriptor_schema.resource.sensitive_paths();
                    // Treat 404 as "no resources found" — the parent container
                    // (space/project) may have been deleted, so no children can exist.
                    // list_with_params_absent_ok suppresses the wxctl::error event for 404.
                    let items: Vec<Value> = if discovery.list_map {
                        // Object-map list (HashiCorp Vault sys/mounts|sys/auth|sys/audit):
                        // the endpoint returns {"data": {"<path>/": {...config...}}}, an object
                        // keyed by the resource path rather than an array — and these kinds have
                        // no per-path GET usable for discovery (absent mount/auth → 400, audit has
                        // no GET → 405). Read the `data` object and yield one item per entry: the
                        // entry's config object with the key (trailing "/" stripped) injected under
                        // the match field, so name_matches finds it AND the entry's computed fields
                        // (e.g. an auth mount's `accessor`) survive rediscovery for downstream
                        // ${...accessor} references. An empty map or 404 means no resources.
                        let map_field = discovery.name_field.unwrap_or("name");
                        let spec = RequestSpec::new(Method::GET, &list_endpoint).body(BodyKind::None).not_found_ok().stage("reconciliation").sensitive_paths(sensitive_paths.clone());
                        match client.execute::<Value>(operation_id, spec).await {
                            Ok(resp) => resp
                                .get("data")
                                .and_then(|d| d.as_object())
                                .map(|m| {
                                    m.iter()
                                        .map(|(k, v)| {
                                            let key = k.trim_end_matches('/').to_string();
                                            match v {
                                                Value::Object(vo) => {
                                                    let mut obj = vo.clone();
                                                    obj.insert(map_field.to_string(), Value::String(key));
                                                    Value::Object(obj)
                                                }
                                                _ => Value::String(key),
                                            }
                                        })
                                        .collect()
                                })
                                .unwrap_or_default(),
                            Err(e) if error_has_status(&e, 404) => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "list-map returned 404 — treating as not found");
                                return Ok(vec![]);
                            }
                            Err(e) => return Err(e).context("Failed to list resources (object map)"),
                        }
                    } else if discovery.list_method == Some("post") {
                        // POST-search enumeration (see `discover`): APIs like CAMS
                        // `/v2/asset_types/<type>/search` have no GET list, only a search POST.
                        let body = discovery.list_body.map(|s| serde_json::from_str::<Value>(s).expect("canonical json list_body")).unwrap_or_else(|| serde_json::json!({}));
                        let mut spec = RequestSpec::new(Method::POST, &list_endpoint).body(BodyKind::Json(body)).not_found_ok().stage("reconciliation").sensitive_paths(sensitive_paths.clone());
                        if let Some(params) = &params {
                            for (k, v) in params {
                                spec = spec.query_param(k, v);
                            }
                        }
                        match client.execute::<Value>(operation_id, spec).await {
                            Ok(resp) => {
                                let field = discovery.list_field.ok_or_else(|| anyhow::anyhow!("list_method: post requires list_field"))?;
                                resp.get(field).and_then(|v| v.as_array()).cloned().unwrap_or_default()
                            }
                            Err(e) if error_has_status(&e, 404) => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "search returned 404 — treating as not found");
                                return Ok(vec![]);
                            }
                            Err(e) => return Err(e).context("Failed to search resources"),
                        }
                    } else if let Some(field) = discovery.list_field {
                        // Declared list_field on a GET list ⇒ explicit resp[field] extraction — see
                        // `discover`'s matching branch (and `list_via_declared_field`) for the rationale.
                        match list_via_declared_field(&client, operation_id, &list_endpoint, &params, sensitive_paths.clone(), field).await {
                            Ok(items) => items,
                            Err(e) if error_has_status(&e, 404) => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "list returned 404 — treating as not found");
                                return Ok(vec![]);
                            }
                            Err(e) => return Err(e).context("Failed to list resources"),
                        }
                    } else {
                        match client.list_with_params_absent_ok(operation_id, &list_endpoint, params, sensitive_paths).await {
                            Ok(items) => items,
                            Err(e) if error_has_status(&e, 404) => {
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "list returned 404 — treating as not found");
                                return Ok(vec![]);
                            }
                            Err(e) if e.to_string().contains("HTTP 403") && e.to_string().ends_with(": []") => {
                                // Instana 3.319 quirk — see `discover`: an empty synthetics list arrives as 403 + `[]`.
                                tracing::debug!(target: "wxctl::reconciliation::discovery", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, "list returned 403 with empty-array body — treating as empty list (Instana empty-synthetics quirk)");
                                return Ok(vec![]);
                            }
                            Err(e) => return Err(e).context("Failed to list resources"),
                        }
                    };

                    // Identity lookup: see `discover` for the name_field vs identity_match dispatch.
                    // EnvMarker kinds match on the identity marker alone (the server clobbers
                    // names), so no local name is required or consulted.
                    let name_field = discovery.name_field.unwrap_or("name");
                    let env_marker_hash = def.reconciliation.identity_hash.as_ref().filter(|ih| matches!(ih.storage, HashStorageIr::EnvMarker)).and_then(|_| resource.data.get("identity_hash").and_then(|v| v.as_str()));
                    let resource_name = if discovery.identity_match.is_some() || env_marker_hash.is_some() { None } else { Some(resource.data.get(name_field).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Resource has no '{}' field", name_field))?) };
                    let run_hash = def.reconciliation.identity_hash.as_ref().filter(|ih| matches!(ih.storage, HashStorageIr::Tag)).and_then(|_| resource.data.get("identity_hash").and_then(|v| v.as_str()));

                    // Find ALL resources matching the identity (not just first)
                    let field_schema = &descriptor_schema.resource.schema;
                    let matched_items = match_remote_items(&items, &resource.data, name_field, resource_name, discovery.identity_match.as_ref(), run_hash, env_marker_hash, discovery.list_filter.as_ref());

                    // list_filter cross-type collision: an item shares the name but is a
                    // different type. The resource is correctly absent (plan Create), but warn
                    // — a backend enforcing cross-type name uniqueness (paw_* has a
                    // parent-id+name unique index) may reject the create. Only the
                    // name-matching path carries list_filter; identity/env-marker kinds never
                    // declare it, so `resource_name` is Some here whenever `list_filter` is.
                    if let (Some(lf), Some(name)) = (discovery.list_filter.as_ref(), resource_name) {
                        push_list_filter_advisories(&items, lf, name, name_field, run_hash, resource.key.kind.as_ref(), resource.key.name.as_ref(), advisories);
                    }

                    // A matched item can still be a leftover husk rather than a live resource
                    // (e.g. an undeployed wxO Environment record with `current_version: null`
                    // left in place by undeploy). `absent_when` marks that shape; drop such
                    // items from the match set entirely — same as "no match" — so `discover`
                    // (which delegates here and takes the first match, else exists:false)
                    // reports the resource as absent (plan Create), not Update/NoChange.
                    let matches: Vec<RemoteResource> = matched_items
                        .into_iter()
                        .filter_map(|data| {
                            let mut denormalized_data = normalize_list_item(data.clone(), name_field);
                            denormalize_api_response(&mut denormalized_data, field_schema);
                            normalize_identity_hash(&mut denormalized_data, descriptor_schema);
                            if is_absent_sentinel(&denormalized_data, discovery.absent_when.as_ref()) {
                                return None;
                            }
                            Some(RemoteResource { key: resource.key.clone(), data: denormalized_data, exists: true })
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
                DiscoveryMethodIr::GetById | DiscoveryMethodIr::Skip | DiscoveryMethodIr::Singleton => {
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
        for immutable_field in reconciliation.immutable_fields.iter().copied() {
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
                return StateComparison::Recreate { field: immutable_field.to_string(), local_value: render_value(local_value), remote_value: render_value(remote_value) };
            }
        }

        // Compare state fields
        // Only compare fields that are explicitly set in the local config
        // This prevents spurious updates for fields with remote defaults that aren't specified locally
        let mut changed_fields = Vec::new();
        let state_fields = reconciliation.state_fields.unwrap_or(&[]);
        for state_field in state_fields.iter().copied() {
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
                changed_fields.push(state_field.to_string());
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
/// For CP4D/watsonx.data responses, a single-segment miss also checks under
/// `entity.{field}` / `metadata.{field}`. Thin wrapper over
/// [`wxctl_core::client::lookup_nested`] returning the `Null` sentinel on a miss.
pub(super) fn get_nested_field<'a>(value: &'a Value, path: &str) -> &'a Value {
    lookup_nested(value, path, true).unwrap_or(&Value::Null)
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
            // Arrays of scalars (id/tag/string/number lists) are compared as an
            // unordered multiset: many APIs return such arrays in a
            // server-determined order (e.g. Instana sorts an alerting
            // configuration's `ruleIds`/`integrationIds`), so positional
            // comparison would report a phantom Update on a pure reorder. Arrays
            // containing objects or nested arrays keep positional comparison —
            // nested order may be semantically meaningful, and structural
            // multiset matching is ambiguous.
            let all_scalar = local_arr.iter().chain(remote_arr.iter()).all(|v| !v.is_array() && !v.is_object());
            if all_scalar {
                let mut remaining: Vec<&Value> = remote_arr.iter().collect();
                for local_item in local_arr {
                    match remaining.iter().position(|remote_item| values_match(local_item, remote_item)) {
                        Some(pos) => {
                            remaining.swap_remove(pos);
                        }
                        None => return false,
                    }
                }
                true
            } else {
                // Positional comparison for object/nested arrays (existing behavior).
                for (local_item, remote_item) in local_arr.iter().zip(remote_arr.iter()) {
                    if !values_match(local_item, remote_item) {
                        return false;
                    }
                }
                true
            }
        }
        // Numbers compare by numeric value, not serde_json representation: a config
        // integer (`5`) an API echoes back as a float (`5.0`) is the same value, and
        // representation-sensitive `==` would report permanent phantom drift on it
        // (e.g. Instana echoes an integer threshold `conditionValue` as a float).
        // Integer/integer pairs compare exactly (no f64 round-trip precision loss).
        (Value::Number(local_num), Value::Number(remote_num)) => {
            if let (Some(l), Some(r)) = (local_num.as_i64(), remote_num.as_i64()) {
                l == r
            } else if let (Some(l), Some(r)) = (local_num.as_u64(), remote_num.as_u64()) {
                l == r
            } else if let (Some(l), Some(r)) = (local_num.as_f64(), remote_num.as_f64()) {
                l == r
            } else {
                local_num == remote_num
            }
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

/// Nested lookup with CP4D `entity.<field>` / `metadata.<field>` envelope
/// fallback for single-segment paths. Used when denormalizing a discovered
/// (list_and_get) response whose `api_field` value sits inside the CP4D envelope
/// rather than at the top level. Returns `None` when the field is absent
/// anywhere (no-op: never fabricates a value that would mask a real diff). Thin
/// wrapper over [`wxctl_core::client::lookup_nested`].
fn extract_nested_value_enveloped<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    lookup_nested(value, path, true)
}

/// Denormalize API response to user-facing format using api_field mappings
/// Extracts values from nested API paths and adds them as top-level fields
fn denormalize_api_response(response: &mut Value, schema: &SchemaBodyIr) {
    // Collect field values from an immutable borrow first, then apply as mutations
    let insertions: Vec<(String, Value)> = schema
        .fields
        .iter()
        .filter_map(|field| {
            let api_path = field.api_field?;
            let value = extract_nested_value_enveloped(&*response, api_path)?;
            if value.is_null() {
                return None;
            }
            Some((field.name.to_string(), value.clone()))
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
    fn extract_declared_field_shapes() {
        // Declared key present → its array.
        let enveloped = json!({"models": [{"name": "m1"}, {"name": "m2"}]});
        assert_eq!(extract_declared_field(enveloped, "models").len(), 2);
        // Absent key = empty list (the declared-field contract, e.g. Pliant folders).
        assert!(extract_declared_field(json!({"other": []}), "models").is_empty());
        // Bare-array response (wxO SaaS GET /v1/orchestrate/models) → the array itself,
        // even though the schema declares a field.
        let bare = json!([{"name": "m1"}, {"name": "m2"}, {"name": "m3"}]);
        assert_eq!(extract_declared_field(bare, "models").len(), 3);
        // Declared key present but non-array → empty, not a panic.
        assert!(extract_declared_field(json!({"models": "oops"}), "models").is_empty());
    }

    #[test]
    fn get_nested_field_indexes_into_arrays() {
        let v = json!({"associated_catalogs": [{"catalog_name": "cat_a"}, {"catalog_name": "cat_b"}]});
        assert_eq!(get_nested_field(&v, "associated_catalogs.0.catalog_name").as_str(), Some("cat_a"));
        assert_eq!(get_nested_field(&v, "associated_catalogs.1.catalog_name").as_str(), Some("cat_b"));
        assert!(get_nested_field(&v, "associated_catalogs.2.catalog_name").is_null());
        assert!(get_nested_field(&v, "associated_catalogs.nope.catalog_name").is_null());
    }

    #[test]
    fn get_nested_field_envelope_fallback() {
        // Single-segment miss falls through the CP4D entity/metadata envelope.
        let v = json!({"entity": {"description": "d"}, "metadata": {"id": "m-1"}});
        assert_eq!(get_nested_field(&v, "description").as_str(), Some("d"));
        assert_eq!(get_nested_field(&v, "id").as_str(), Some("m-1"));
        // Multi-segment paths never consult the envelope.
        assert!(get_nested_field(&v, "foo.description").is_null());
        // Absent everywhere → Null sentinel.
        assert!(get_nested_field(&v, "missing").is_null());
    }

    #[test]
    fn extract_nested_value_enveloped_arrays_and_envelope() {
        // Superset behavior: array indexing (formerly objects-only) plus the
        // single-segment envelope fallback.
        let v = json!({"catalogs": [{"name": "a"}], "entity": {"tag": "t"}});
        assert_eq!(extract_nested_value_enveloped(&v, "catalogs.0.name").and_then(|x| x.as_str()), Some("a"));
        assert_eq!(extract_nested_value_enveloped(&v, "tag").and_then(|x| x.as_str()), Some("t"));
        assert!(extract_nested_value_enveloped(&v, "absent").is_none());
    }

    #[test]
    fn match_remote_items_selection() {
        // Storage-registration shape: plural `associated_catalogs[0]` remote, singular `associated_catalog` local.
        let items = vec![json!({"display_name": "A", "associated_catalogs": [{"catalog_name": "cat_x"}]}), json!({"display_name": "B", "associated_catalogs": [{"catalog_name": "cat_y"}]})];
        let local = json!({"display_name": "ignored", "associated_catalog": {"catalog_name": "cat_y"}});
        let identity = IdentityMatchIr { local_path: "associated_catalog.catalog_name", remote_path: "associated_catalogs.0.catalog_name" };
        let hits = match_remote_items(&items, &local, "display_name", None, Some(&identity), None, None, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("display_name").and_then(|v| v.as_str()), Some("B"), "identity_match selects by catalog name, not display_name");

        // Even if `display_name` matches, identity_match is the only criterion when declared.
        let items = vec![json!({"display_name": "same", "associated_catalog": {"catalog_name": "cat_x"}}), json!({"display_name": "other", "associated_catalog": {"catalog_name": "cat_y"}})];
        let local = json!({"display_name": "same", "associated_catalog": {"catalog_name": "cat_y"}});
        let identity = IdentityMatchIr { local_path: "associated_catalog.catalog_name", remote_path: "associated_catalog.catalog_name" };
        let hits = match_remote_items(&items, &local, "display_name", None, Some(&identity), None, None, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("display_name").and_then(|v| v.as_str()), Some("other"), "identity_match ignores a matching display_name");

        // Falls back to name_field when identity_match is absent.
        let items = vec![json!({"name": "alpha"}), json!({"name": "beta"})];
        let local = json!({"name": "beta"});
        let hits = match_remote_items(&items, &local, "name", Some("beta"), None, None, None, None);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("name").and_then(|v| v.as_str()), Some("beta"));
    }

    #[test]
    fn match_remote_items_list_filter_selects_by_type() {
        // A folder and a book share the name; the paw_book kind filters on type=dashboard.
        let items = vec![json!({"name": "Reports", "type": "folder"}), json!({"name": "Reports", "type": "dashboard"})];
        let local = json!({"name": "Reports"});
        let lf = ListFilterIr { field: "type", equals: "dashboard" };
        // AC1: only the dashboard item matches — the folder is never adopted.
        let hits = match_remote_items(&items, &local, "name", Some("Reports"), None, None, None, Some(&lf));
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].get("type").and_then(|v| v.as_str()), Some("dashboard"));
        // A book named Reports that does NOT exist (only the folder does) → no match → absent → Create.
        let only_folder = vec![json!({"name": "Reports", "type": "folder"})];
        assert!(match_remote_items(&only_folder, &local, "name", Some("Reports"), None, None, None, Some(&lf)).is_empty());
        // AC4: no list_filter → name-only match, both items selected (unchanged behavior).
        let both = match_remote_items(&items, &local, "name", Some("Reports"), None, None, None, None);
        assert_eq!(both.len(), 2);
    }

    /// Records every advisory pushed, for assertion in tests below.
    struct RecordingSink(std::sync::Mutex<Vec<(String, String, String)>>);
    impl AdvisorySink for RecordingSink {
        fn push(&self, code: &str, resource: &str, message: &str, _suggestion: &str) {
            self.0.lock().unwrap().push((code.to_string(), resource.to_string(), message.to_string()));
        }
    }

    /// R501 dedup requirement: two colliding items sharing one conflicting `found`
    /// value push exactly one advisory (deduped by (resource, conflicting value)); a
    /// same-named item that matches the kind's own `list_filter` value is not a
    /// collision at all.
    #[test]
    fn push_list_filter_advisories_dedupes_by_conflicting_value() {
        let lf = ListFilterIr { field: "asset_type", equals: "dashboard" };
        let items = vec![
            json!({ "name": "Reports", "asset_type": "folder" }),    // collision
            json!({ "name": "Reports", "asset_type": "folder" }),    // same conflicting value → deduped
            json!({ "name": "Reports", "asset_type": "dashboard" }), // own type → not a collision
        ];
        let sink = RecordingSink(std::sync::Mutex::new(Vec::new()));
        push_list_filter_advisories(&items, &lf, "Reports", "name", None, "paw_book", "Reports", &sink);
        let got = sink.0.lock().unwrap();
        assert_eq!(got.len(), 1, "one advisory per (resource, conflicting value): {got:?}");
        assert_eq!(got[0].0, "WXCTL-R501");
        assert_eq!(got[0].1, "paw_book/Reports");
        assert!(got[0].2.contains("asset_type='folder'"), "names the conflicting value: {}", got[0].2);
    }

    #[test]
    fn is_absent_sentinel_null_equals_matches_missing_and_explicit_null() {
        // wxO agent_release shape: a matched `live` Environment record with a
        // deployed version is NOT the sentinel — exists true.
        let aw = AbsentWhenIr { field: "current_version", equals: "null" };
        assert!(!is_absent_sentinel(&json!({"name": "live", "current_version": 5}), Some(&aw)), "a real deployed version is not the absent sentinel");

        // Undeploy leaves the record with current_version explicitly null — absent.
        assert!(is_absent_sentinel(&json!({"name": "live", "current_version": null}), Some(&aw)), "explicit null current_version is the absent sentinel");

        // The field can also be omitted entirely (get_nested_field's Null miss
        // sentinel) — `equals: null` must match that shape too, or a response
        // shape that never echoes the field would falsely read as a release.
        assert!(is_absent_sentinel(&json!({"name": "live"}), Some(&aw)), "a missing field is treated the same as an explicit null");

        // No absent_when declared => never the sentinel (existing kinds unaffected).
        assert!(!is_absent_sentinel(&json!({"name": "live", "current_version": null}), None));
    }

    #[test]
    fn is_absent_sentinel_string_equals_still_works() {
        // SAL's existing `equals: missing` (a bare string, not null) must keep working.
        // `equals` is the canonical-JSON encoding of the sentinel value, so a string
        // sentinel carries its own quotes.
        let aw = AbsentWhenIr { field: "status", equals: "\"missing\"" };
        assert!(is_absent_sentinel(&json!({"status": "missing"}), Some(&aw)));
        assert!(!is_absent_sentinel(&json!({"status": "active"}), Some(&aw)));
        // A field merely absent does NOT match a non-null string sentinel — only
        // `equals: null` treats a missing field as a hit.
        assert!(!is_absent_sentinel(&json!({}), Some(&aw)));
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
            // scalar arrays compare as an unordered multiset: a server that
            // reorders an id/tag list (Instana sorts `ruleIds`) must not read as
            // drift. Object arrays stay positional (nested order may be meaningful).
            (json!(["a", "b"]), json!(["b", "a"]), true),
            (json!(["cpu", "mq"]), json!(["mq", "cpu"]), true),
            (json!(["a", "a", "b"]), json!(["a", "b", "a"]), true),
            (json!(["a", "a"]), json!(["a", "b"]), false),
            (json!([{"k": 1}, {"k": 2}]), json!([{"k": 2}, {"k": 1}]), false),
            // numbers: value equality across serde_json representations — a config
            // integer echoed back by the API as a float is the same value (Instana
            // returns `conditionValue: 5` as `5.0`); differing values still differ,
            // and large u64s beyond f64's 2^53 integer range compare exactly.
            (json!(5), json!(5.0), true),
            (json!(5.0), json!(5), true),
            (json!(0.05), json!(0.05), true),
            (json!(5), json!(6), false),
            (json!(5), json!(5.5), false),
            (json!(u64::MAX), json!(u64::MAX), true),
            (json!(u64::MAX), json!(u64::MAX - 1), false),
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

    /// Build a minimal schema declaring the named fields at the given locations
    /// and optional identity/name_field settings, by compiling a YAML literal
    /// through the production parse path (D10). Used to exercise the Path/Query
    /// -aware branches of identity_paths_unresolved, build_scoping_params, and
    /// substitute_path_placeholders.
    fn make_schema_with_fields(fields: &[(&str, FieldLocationIr)], identity: Option<IdentityMatchIr>, name_field: Option<&str>) -> &'static SchemaIr {
        fn loc_str(l: FieldLocationIr) -> &'static str {
            match l {
                FieldLocationIr::Body => "Body",
                FieldLocationIr::Query => "Query",
                FieldLocationIr::Header => "Header",
                FieldLocationIr::Path => "Path",
                FieldLocationIr::Computed => "Computed",
                FieldLocationIr::LocalOnly => "LocalOnly",
            }
        }
        let mut fields_yaml = String::new();
        if fields.is_empty() {
            fields_yaml.push_str("    fields: []\n");
        } else {
            fields_yaml.push_str("    fields:\n");
            for (name, loc) in fields {
                fields_yaml.push_str(&format!("      - name: \"{name}\"\n        type: string\n        location: {}\n", loc_str(*loc)));
            }
        }
        // list_endpoint carries a `{field}` placeholder for every declared Path field,
        // so substitute_path_placeholders tests have something meaningful to substitute.
        let path_placeholders: String = fields.iter().filter(|(_, loc)| matches!(loc, FieldLocationIr::Path)).map(|(name, _)| format!("/{{{name}}}")).collect();
        let list_endpoint = format!("/v1/parents{path_placeholders}/things");
        let mut discovery_extra = String::new();
        if let Some(nf) = name_field {
            discovery_extra.push_str(&format!("      name_field: \"{nf}\"\n"));
        }
        if let Some(im) = identity {
            discovery_extra.push_str(&format!("      identity_match:\n        local_path: \"{}\"\n        remote_path: \"{}\"\n", im.local_path, im.remote_path));
        }
        let yaml = format!(
            r#"
resource:
  name: thing
  service: svc
  kind: thing
  version: v1
  api:
    base_path: "{list_endpoint}"
    id_field: id
    list_endpoint: "{list_endpoint}"
    get_endpoint: "{list_endpoint}/{{id}}"
    create_method: POST
    delete_method: DELETE
  schema:
{fields_yaml}  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
{discovery_extra}    state_fields: []
    update_strategy: patch
    use_json_patch: false
"#
        );
        wxctl_schema::ir_support::compile_to_static_ir(&yaml).unwrap_or_else(|e| panic!("test schema yaml failed to parse: {e}\n{yaml}"))
    }

    #[test]
    fn build_get_by_id_spec_interpolates_parent_scoped_path_fields() {
        // Planning Analytics regression: get_endpoint carries a parent-scoped
        // `{dimension}` placeholder IN ADDITION to the id placeholder `{name}`
        // (e.g. `/Dimensions('{dimension}')/Hierarchies('{name}')`). Before this
        // fix, only the id_source placeholder was string-replaced — `{dimension}`
        // was left literal, the live TM1 server 404s on the literal text, and
        // discovery reported a phantom not-found (every re-plan showed a spurious
        // `+ create` for pa_hierarchy / pa_subset / pa_view).
        const PA_HIERARCHY_YAML: &str = r#"
resource:
  name: pa_hierarchy
  service: planning-analytics
  kind: pa_hierarchy
  version: v1
  api:
    base_path: /Dimensions
    id_field: name
    get_endpoint: "/Dimensions('{dimension}')/Hierarchies('{name}')"
    create_method: POST
    delete_method: DELETE
  schema:
    fields:
      - name: dimension
        type: string
        required: true
        immutable: true
        location: Path
      - name: name
        type: string
        required: true
        immutable: true
        location: Body
  reconciliation:
    discovery:
      method: get_by_id
      id_source: name
    state_fields: []
    update_strategy: patch
    use_json_patch: false
"#;
        let schema = wxctl_schema::ir_support::compile_to_static_ir(PA_HIERARCHY_YAML).expect("valid test schema yaml");
        let data = json!({"name": "H1", "dimension": "D1"});

        let spec = build_get_by_id_spec(schema, &data, "name", "H1").unwrap();

        // id_source placeholder resolved via string-replace...
        assert!(spec.path_template.contains("('H1')"), "id placeholder must be substituted into the endpoint: {}", spec.path_template);
        // ...and the OTHER Path field resolved via materialize, not left as literal `{dimension}`.
        assert!(spec.path_template.contains("{dimension}"), "path_template keeps the raw placeholder text; interpolation happens via path_vars");
        assert_eq!(spec.path_vars.get("dimension").map(String::as_str), Some("D1"), "parent-scoped Path field must materialize into path_vars");
        assert!(matches!(spec.body, BodyKind::None), "GetById discovery sends no body");
        assert!(spec.expected_statuses.contains(&404), "404 must be an expected (not-found) outcome");
        assert_eq!(spec.stage, "reconciliation");
    }

    #[test]
    fn identity_paths_unresolved_branches() {
        // (label, data, fields, identity, name_field, expected) — each row is a
        // distinct identity_paths_unresolved branch: None = discovery proceeds,
        // Some(s) = unresolved template `s` surfaced so discovery is skipped.
        #[allow(clippy::type_complexity)]
        let cases: Vec<(&str, Value, Vec<(&str, FieldLocationIr)>, Option<IdentityMatchIr>, Option<&str>, Option<&str>)> = vec![
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
                Some(IdentityMatchIr { local_path: "associated_catalog.catalog_name", remote_path: "associated_catalogs.0.catalog_name" }),
                None,
                None,
            ),
            // identity_match value itself templated → surfaced.
            (
                "templated identity_match value",
                json!({"associated_catalog": {"catalog_name": "${presto_engine.analytics.catalog}", "catalog_type": "iceberg"}}),
                vec![],
                Some(IdentityMatchIr { local_path: "associated_catalog.catalog_name", remote_path: "associated_catalogs.0.catalog_name" }),
                None,
                Some("${presto_engine.analytics.catalog}"),
            ),
            // Templated Query / Path scoping fields → surfaced.
            ("templated query field", json!({"catalog_id": "${catalog.primary.id}", "name": "thing"}), vec![("catalog_id", FieldLocationIr::Query)], None, None, Some("${catalog.primary.id}")),
            ("templated path field", json!({"catalog_id": "${catalog.primary.id}", "name": "thing"}), vec![("catalog_id", FieldLocationIr::Path)], None, None, Some("${catalog.primary.id}")),
            // Path/Query/name all literal (an unrelated `other` template is irrelevant) → None.
            ("path/query/name all literal", json!({"catalog_id": "lit-cat-id", "name": "thing", "other": "${foo.bar}"}), vec![("catalog_id", FieldLocationIr::Path)], None, None, None),
            // A Body field with a templated value is NOT identity-relevant — the
            // template goes into the POST body at execution time, not the list call → None.
            ("templated body field ignored", json!({"bucket": "${s3_bucket.x.name}", "name": "thing"}), vec![("bucket", FieldLocationIr::Body)], None, None, None),
            // Templated default name field when identity_match absent → surfaced.
            ("templated name field (no identity)", json!({"name": "${thing.x.name}"}), vec![], None, None, Some("${thing.x.name}")),
            // Dotted Query field reader must surface an unresolved nested template
            // so discovery is skipped (returns the template string to the caller).
            ("templated dotted query field", json!({"target": {"target_id": "${subscription.os_sub.id}"}, "monitor_definition_id": "quality"}), vec![("target.target_id", FieldLocationIr::Query)], None, None, Some("${subscription.os_sub.id}")),
            // Custom name_field honored when templated.
            ("templated custom name field", json!({"display_name": "${engine.x.display_name}"}), vec![], None, Some("display_name"), Some("${engine.x.display_name}")),
        ];
        for (label, data, fields, identity, name_field, expected) in cases {
            let schema = make_schema_with_fields(&fields, identity, name_field);
            assert_eq!(identity_paths_unresolved(&data, schema).as_deref(), expected, "case: {label}");
        }
    }

    #[test]
    fn build_scoping_params_query_field_branches() {
        // A `location: Query` field named `target.target_id` must read the nested
        // value data["target"]["target_id"] and emit it under that dotted key.
        let data = json!({"target": {"target_id": "sub-123", "target_type": "subscription"}, "monitor_definition_id": "quality"});
        let schema = make_schema_with_fields(&[("target.target_id", FieldLocationIr::Query)], None, None);
        let params = build_scoping_params(&data, schema).unwrap().expect("scoping params present");
        assert_eq!(params.get("target.target_id").map(String::as_str), Some("sub-123"));

        // Single-segment Query names traverse via map.get exactly as before — guards
        // against a regression in existing flat scoping fields (space_id, catalog_id).
        let data = json!({"catalog_id": "cat-9", "name": "thing"});
        let schema = make_schema_with_fields(&[("catalog_id", FieldLocationIr::Query)], None, None);
        let params = build_scoping_params(&data, schema).unwrap().expect("scoping params present");
        assert_eq!(params.get("catalog_id").map(String::as_str), Some("cat-9"));

        // An unresolved ${...} template in a dotted query field must Err so the
        // caller skips the list call rather than sending the literal template string.
        let data = json!({"target": {"target_id": "${subscription.os_sub.id}"}});
        let schema = make_schema_with_fields(&[("target.target_id", FieldLocationIr::Query)], None, None);
        assert!(build_scoping_params(&data, schema).is_err());
    }

    #[test]
    fn build_scoping_params_bails_on_object_scoping_value() {
        // Destroy against an absent stack: a bare `${project.x}` ref resolves to the
        // WHOLE cache-seeded object, and `extract_reference_field` finds no id to
        // extract (the project was never created), leaving the object in the query
        // field. That value must Err — exactly like an unresolved template — so the
        // caller skips the list call instead of sending it paramless (live 400:
        // "Exactly one of the query parameters in [project_id, space_id] is
        // required"; run 20260708-002457-destroy-483a7e).
        let data = json!({"project_id": {"name": "churn-project", "description": "seeded local data, no guid"}, "display_name": "Runtime"});
        let schema = make_schema_with_fields(&[("project_id", FieldLocationIr::Query)], None, None);
        assert!(build_scoping_params(&data, schema).is_err(), "object left by failed ref extraction must not be silently dropped");

        // A genuinely absent optional scoping field still yields params-less Ok —
        // the config never set it, so an unscoped LIST is the author's intent.
        let data = json!({"display_name": "Runtime"});
        let schema = make_schema_with_fields(&[("project_id", FieldLocationIr::Query)], None, None);
        assert!(build_scoping_params(&data, schema).unwrap().is_none());
    }

    #[test]
    fn substitute_path_placeholders_branches() {
        let endpoint = "/v1/parents/{catalog_id}/things";
        // Path-located field substituted into its placeholder.
        let schema = make_schema_with_fields(&[("catalog_id", FieldLocationIr::Path)], None, None);
        assert_eq!(substitute_path_placeholders(endpoint, &json!({"catalog_id": "cat-123"}), schema).unwrap(), "/v1/parents/cat-123/things");
        // No matching placeholder in the endpoint → passes through unchanged.
        assert_eq!(substitute_path_placeholders("/v1/things", &json!({"catalog_id": "cat-123"}), schema).unwrap(), "/v1/things");
        // Missing value for a declared Path placeholder → error.
        assert!(substitute_path_placeholders(endpoint, &json!({}), schema).is_err());
        // A Body-located field `catalog_id` with the same name still shouldn't get
        // substituted into paths — only Path-located fields are candidates.
        let body_schema = make_schema_with_fields(&[("catalog_id", FieldLocationIr::Body)], None, None);
        assert_eq!(substitute_path_placeholders(endpoint, &json!({"catalog_id": "cat-123"}), body_schema).unwrap(), "/v1/parents/{catalog_id}/things");
    }

    /// Build a storage_registration-shaped schema whose reconciliation declares
    /// the given `immutable_fields` and `reject_on_immutable_drift`, by compiling
    /// a YAML literal (D10).
    fn make_schema_with_immutable(immutable_fields: &[&str], reject: bool) -> &'static SchemaIr {
        let immutable_yaml = immutable_fields.iter().map(|f| format!("\"{f}\"")).collect::<Vec<_>>().join(", ");
        let yaml = format!(
            r#"
resource:
  name: reg
  service: svc
  kind: reg
  version: v1
  api:
    base_path: /v3/regs
    id_field: id
    list_endpoint: /v3/regs
    get_endpoint: "/v3/regs/{{id}}"
    create_method: POST
    update_method: PATCH
    delete_method: DELETE
  schema:
    fields:
      - name: type
        type: string
        required: true
        immutable: true
        location: Body
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    state_fields: []
    update_strategy: patch
    immutable_fields: [{immutable_yaml}]
    reject_on_immutable_drift: {reject}
    use_json_patch: false
"#
        );
        wxctl_schema::ir_support::compile_to_static_ir(&yaml).unwrap_or_else(|e| panic!("test schema yaml failed to parse: {e}\n{yaml}"))
    }

    /// Build the orchestrate_connection schema (state_fields: [configured_environments],
    /// update_strategy: recreate, immutable_fields: [app_id]) by compiling a YAML
    /// literal (D10).
    fn make_connection_schema() -> &'static SchemaIr {
        const YAML: &str = r#"
resource:
  name: orchestrate_connection
  service: watsonx_orchestrate
  kind: orchestrate_connection
  version: v1
  api:
    base_path: /v1/orchestrate/connections/applications
    id_field: app_id
    list_endpoint: /v1/orchestrate/connections/applications
    get_endpoint: "/v1/orchestrate/connections/applications/{app_id}"
    create_method: POST
    update_method: PATCH
    delete_method: DELETE
  schema:
    fields:
      - name: app_id
        type: string
        immutable: true
        location: Body
      - name: configured_environments
        type: array
        item_type: string
        location: Body
  reconciliation:
    discovery:
      method: get_by_id
      id_source: app_id
    state_fields: [configured_environments]
    update_strategy: recreate
    immutable_fields: [app_id]
    use_json_patch: false
"#;
        wxctl_schema::ir_support::compile_to_static_ir(YAML).expect("valid test schema yaml")
    }

    #[test]
    fn connection_missing_env_diff_is_update_not_recreate() {
        // Draft-only connection now declaring [draft, live]: the configured_environments
        // state diff must be an Update (add live), never a Recreate (app_id is unchanged).
        let schema = make_connection_schema();
        let reconciler = SchemaBasedReconciler::new();
        let local = json!({"app_id": "churn-scoring", "configured_environments": ["draft", "live"]});
        let remote = json!({"app_id": "churn-scoring", "configured_environments": ["draft"]});
        let (lv, rv) = make_test_resources(local, remote, schema);
        match reconciler.compare(&lv, &rv) {
            StateComparison::Update { fields } => assert_eq!(fields, vec!["configured_environments".to_string()]),
            other => panic!("expected Update on configured_environments, got {:?}", other),
        }
    }

    #[test]
    fn connection_fully_configured_is_no_change() {
        // Both envs already configured -> steady state -> NoChange (idempotent re-apply, I1).
        let schema = make_connection_schema();
        let reconciler = SchemaBasedReconciler::new();
        let both = json!({"app_id": "churn-scoring", "configured_environments": ["draft", "live"]});
        let (lv, rv) = make_test_resources(both.clone(), both, schema);
        assert!(matches!(reconciler.compare(&lv, &rv), StateComparison::NoChange));
    }

    #[test]
    fn connection_without_declared_environment_is_inert() {
        // A connection that sets no configured_environments locally must not diff on it even
        // when the remote reports a configured set -- compare() skips state fields absent from
        // local config. Guards the architecture's inertness claim.
        let schema = make_connection_schema();
        let reconciler = SchemaBasedReconciler::new();
        let local = json!({"app_id": "plain-conn"});
        let remote = json!({"app_id": "plain-conn", "configured_environments": ["draft"]});
        let (lv, rv) = make_test_resources(local, remote, schema);
        assert!(matches!(reconciler.compare(&lv, &rv), StateComparison::NoChange));
    }

    fn make_test_resources(local_data: Value, remote_data: Value, schema: &'static SchemaIr) -> (ValidatedResource, RemoteResource) {
        use wxctl_core::registry::ResourceDescriptor;
        use wxctl_core::types::ResourceKey;
        let descriptor = std::sync::Arc::new(ResourceDescriptor::from_ir(schema));
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
            let schema = make_schema_with_immutable(&["type"], reject);
            let (lv, rv) = make_test_resources(json!({"type": "ibm_cos"}), json!({"type": "amazon_s3"}), schema);
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
        let schema = make_schema_with_immutable(&["type"], true);
        let (lv, rv) = make_test_resources(json!({"type": "ibm_cos"}), json!({"type": "ibm_cos"}), schema);
        assert!(matches!(reconciler.compare(&lv, &rv), StateComparison::NoChange));
    }

    /// Build a storage_registration-shaped schema: state_fields
    /// [display_name, description, tags] + immutable_fields [bucket,
    /// associated_catalog.catalog_name, associated_catalog.catalog_type] +
    /// reject_on_immutable_drift, with an identity_match on the catalog name, by
    /// compiling a YAML literal (D10). Used to reproduce the Deferred-but-found
    /// re-plan regression where the templated `bucket` immutable ref must not
    /// trigger a phantom Recreate.
    fn make_storage_registration_schema() -> &'static SchemaIr {
        const YAML: &str = r#"
resource:
  name: storage_registration
  service: watsonx_data
  kind: storage_registration
  version: v1
  api:
    base_path: /v3/storage_registrations
    id_field: id
    list_endpoint: /v3/storage_registrations
    get_endpoint: "/v3/storage_registrations/{id}"
    create_method: POST
    update_method: PATCH
    delete_method: DELETE
  schema:
    fields: []
  reconciliation:
    discovery:
      method: list_and_get
      list_field: storage_registrations
      id_source: id
      identity_match:
        local_path: associated_catalog.catalog_name
        remote_path: associated_catalogs.0.catalog_name
    state_fields:
      - display_name
      - description
      - tags
    update_strategy: patch
    use_json_patch: false
    immutable_fields:
      - bucket
      - associated_catalog.catalog_name
      - associated_catalog.catalog_type
    reject_on_immutable_drift: true
"#;
        wxctl_schema::ir_support::compile_to_static_ir(YAML).expect("valid test schema yaml")
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
        let (lv, rv) = make_test_resources(local, remote, schema);
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
        let (lv, rv) = make_test_resources(local, remote, schema);
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
        let (lv, rv) = make_test_resources(local, remote, schema);
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
        assert_eq!(compared_field_resolution(&data, schema), (4, 1));

        // When every present compared field is templated, comparable == 0 — the
        // Deferred-Apply caller keeps the conservative blind Update.
        let schema = make_schema_with_immutable(&["type"], false);
        let data = json!({"type": "${storage_connection.c.type}"});
        assert_eq!(compared_field_resolution(&data, schema), (0, 1));
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
        let overlay_schema = make_schema_with_immutable(&["job_id"], false);
        let local = json!({"job_id": "ingest-001"});
        let remote = json!({"job_id": "ingest-002"});
        let (lv, rv) = make_test_resources(local, remote, overlay_schema);
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
        use wxctl_schema::ir::{FieldIr, FieldTypeIr};
        // A category schema mapping user field `parent_category` ← api_field `parent_category_id`.
        let schema = SchemaBodyIr {
            fields: &[FieldIr {
                name: "parent_category",
                field_type: FieldTypeIr::String,
                required: false,
                immutable: false,
                location: FieldLocationIr::Body,
                description: None,
                validation: None,
                schema: None,
                item_type: None,
                default: None,
                allowed_values: None,
                references: None,
                api_field: Some("parent_category_id"),
                sensitive: false,
                also_query: false,
                is_path: false,
                synthesize: None,
                synth_shape: None,
            }],
            discriminator: None,
            variants: None,
        };

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

    /// Build a name_suffix identity-hash schema with the synthetic `identity_hash`
    /// as the only state field (as the parser produces after dropping name + hashed
    /// fields). Mirrors the autoai_experiment shape without wiring the real kind, by
    /// compiling a YAML literal (D10).
    fn make_identity_hash_schema() -> &'static SchemaIr {
        const YAML: &str = r#"
resource:
  name: job
  service: svc
  kind: job
  version: v1
  api:
    base_path: /v4/trainings
    id_field: id
    list_endpoint: /v4/trainings
    get_endpoint: "/v4/trainings/{id}"
    create_method: POST
    delete_method: DELETE
  schema:
    fields: []
  reconciliation:
    discovery:
      method: list_and_get
      id_source: id
    identity_hash:
      fields: [training_data, scoring]
      nonce_field: generation
      storage: name_suffix
      length: 8
    state_fields: [identity_hash]
    update_strategy: recreate
    use_json_patch: false
"#;
        wxctl_schema::ir_support::compile_to_static_ir(YAML).expect("valid test schema yaml")
    }

    #[test]
    fn identity_hash_name_suffix_nochange_when_match_create_when_differ() {
        let schema = make_identity_hash_schema();
        let reconciler = SchemaBasedReconciler::new();

        // Remote list holds two prior generations of the same base name.
        let items = vec![json!({"name": "exp-abc12345", "id": "t1"}), json!({"name": "exp-99999999", "id": "t2"})];

        // Same hash → discovery matches the suffixed name (run_hash None for name_suffix).
        let matched = match_remote_items(&items, &json!({"name": "exp-abc12345"}), "name", Some("exp-abc12345"), None, None, None, None);
        assert_eq!(matched.len(), 1, "suffixed name matches exactly one remote");
        let mut remote_data = matched[0].clone();
        normalize_identity_hash(&mut remote_data, schema);
        assert_eq!(remote_data.get("identity_hash").and_then(|v| v.as_str()), Some("abc12345"), "hash parsed from name suffix");

        // compare with equal identity_hash → NoChange (name is not a state field).
        let (lv, rv) = make_test_resources(json!({"name": "exp-abc12345", "identity_hash": "abc12345"}), remote_data, schema);
        assert!(matches!(reconciler.compare(&lv, &rv), StateComparison::NoChange), "matching hash → NoChange");

        // Different hash → no discovery match → RemoteResource exists:false → Create (NOT Recreate).
        let no_match = match_remote_items(&items, &json!({"name": "exp-def67890"}), "name", Some("exp-def67890"), None, None, None, None);
        assert!(no_match.is_empty(), "a differing hash yields a different suffixed name → no match");
        let (lv2, rv2) = make_test_resources(json!({"name": "exp-def67890", "identity_hash": "def67890"}), Value::Null, schema);
        let absent = RemoteResource { exists: false, ..rv2 };
        assert!(matches!(reconciler.compare(&lv2, &absent), StateComparison::Create), "differing hash → Create, never Recreate");
    }

    /// Build a job_run-shaped env_marker schema: the server clobbers submitted run
    /// names to "Notebook Job" (both CPDaaS and CP4D, live-pinned 2026-07-05), so
    /// identity rides a WXCTL_IDENTITY entry in the round-tripped
    /// configuration.env_variables. Mirrors the wired kind without a registry, by
    /// compiling a YAML literal (D10).
    fn make_env_marker_schema() -> &'static SchemaIr {
        const YAML: &str = r#"
resource:
  name: job_run
  service: common_core
  kind: job_run
  version: v1
  api:
    base_path: "/v2/jobs/{job}/runs"
    id_field: id
    list_endpoint: "/v2/jobs/{job}/runs"
    get_endpoint: "/v2/jobs/{job}/runs/{id}"
    create_method: POST
    delete_method: DELETE
  schema:
    fields: []
  reconciliation:
    discovery:
      method: list_and_get
      list_field: results
      id_source: id
    identity_hash:
      fields: [job, env_variables, project_id]
      nonce_field: generation
      storage: env_marker
      length: 8
    state_fields: [identity_hash]
    update_strategy: recreate
    use_json_patch: false
"#;
        wxctl_schema::ir_support::compile_to_static_ir(YAML).expect("valid test schema yaml")
    }

    /// A runs-LIST item in the live CAMS shape: name always the clobbered
    /// "Notebook Job", identity marker inside the round-tripped configuration.
    fn clobbered_run(hash: &str, id: &str, state: &str) -> Value {
        json!({"metadata": {"name": "Notebook Job", "asset_id": id}, "entity": {"job_run": {"name": "Notebook Job", "state": state, "configuration": {"env_variables": [format!("WXCTL_IDENTITY={hash}")]}}}, "id": id})
    }

    #[test]
    fn identity_hash_env_marker_matches_marker_ignores_clobbered_name() {
        let schema = make_env_marker_schema();
        let reconciler = SchemaBasedReconciler::new();

        // Remote list: a prior generation (different marker), a failed + completed
        // duplicate of the CURRENT marker (pre-fix create-loop residue), and a
        // legacy run with no marker at all. Every name is "Notebook Job".
        let items = vec![clobbered_run("99999999", "r-0", "Completed"), clobbered_run("abc12345", "r-1", "Failed"), clobbered_run("abc12345", "r-2", "Completed"), json!({"metadata": {"name": "Notebook Job", "asset_id": "r-3"}})];

        // Marker-only matching: the local name plays no role (it can't — the server
        // clobbers it), exact current hash only, Completed ordered first.
        let matched = match_remote_items(&items, &json!({"name": "train-run"}), "name", None, None, None, Some("abc12345"), None);
        assert_eq!(matched.len(), 2, "both duplicates of the current marker match; other generations and unmarked runs do not");
        assert_eq!(matched[0].pointer("/metadata/asset_id").and_then(|v| v.as_str()), Some("r-2"), "Completed run ordered first (stable adopt for discover/post_discover)");

        // normalize stamps identity_hash from the marker; compare over the synthetic
        // hash → NoChange even though local name ("train-run") differs from the
        // remote's clobbered "Notebook Job" — name is neither a state field nor
        // immutable, so no Update/Recreate loop.
        let mut remote_data = matched[0].clone();
        normalize_identity_hash(&mut remote_data, schema);
        assert_eq!(remote_data.get("identity_hash").and_then(|v| v.as_str()), Some("abc12345"), "hash parsed from the WXCTL_IDENTITY marker");
        let (lv, rv) = make_test_resources(json!({"name": "train-run", "identity_hash": "abc12345"}), remote_data, schema);
        assert!(matches!(reconciler.compare(&lv, &rv), StateComparison::NoChange), "matching marker → NoChange despite the clobbered name");

        // Changed input / bumped generation → different hash → no marker match →
        // exists:false → Create (a new run), never Recreate.
        let no_match = match_remote_items(&items, &json!({"name": "train-run"}), "name", None, None, None, Some("def67890"), None);
        assert!(no_match.is_empty(), "a differing hash matches no remote marker");
        let (lv2, rv2) = make_test_resources(json!({"name": "train-run", "identity_hash": "def67890"}), Value::Null, schema);
        let absent = RemoteResource { exists: false, ..rv2 };
        assert!(matches!(reconciler.compare(&lv2, &absent), StateComparison::Create), "differing hash → Create, never Recreate");
    }

    /// Build a sal_*-shaped schema: Skip discovery (non-discoverable API), no
    /// name_field, and `identity_hash.storage: Local` with an explicit empty
    /// `state_fields` (compare over the hash-identity is trivially NoChange once
    /// discovered as existing). Mirrors the wired Q2 local-hash kinds without a
    /// registry, by compiling a YAML literal (D10).
    fn make_local_hash_schema() -> &'static SchemaIr {
        const YAML: &str = r#"
resource:
  name: sal_like
  service: svc
  kind: sal_like
  version: v1
  api:
    base_path: /v3/sal_like
    id_field: id
    get_endpoint: "/v3/sal_like/{id}"
    create_method: POST
    delete_method: DELETE
  schema:
    fields: []
  reconciliation:
    discovery:
      method: skip
      id_source: id
    identity_hash:
      fields: [changes]
      nonce_field: generation
      storage: local
      length: 8
    state_fields: []
    update_strategy: recreate
    use_json_patch: false
"#;
        wxctl_schema::ir_support::compile_to_static_ir(YAML).expect("valid test schema yaml")
    }

    /// Build a bare `ValidatedResource` with a caller-chosen key (kind/name), unlike
    /// `make_test_resources` which hardcodes `reg/test` and always pairs it with a
    /// `RemoteResource`. `local_hash_skip_match` only needs the local side, and the
    /// local-hash record store is keyed by kind/name, so the test needs distinct keys.
    fn make_validated(schema: &'static SchemaIr, kind: &str, name: &str, data: Value) -> ValidatedResource {
        use wxctl_core::registry::ResourceDescriptor;
        use wxctl_core::types::ResourceKey;
        let descriptor = std::sync::Arc::new(ResourceDescriptor::from_ir(schema));
        ValidatedResource { key: ResourceKey::new(kind, name), data, descriptor, dependencies: vec![], on_destroy: Default::default() }
    }

    /// Q2 fallback: Skip + storage: local. No record → None (Create). Recorded →
    /// exists:true whose compare is NoChange. Changed input / bumped generation →
    /// new hash → no record → None (Create). Prior hashes stay recorded (accumulate).
    #[test]
    fn local_hash_skip_match_gates_create_vs_nochange() {
        let schema = make_local_hash_schema();
        let dir = tempfile::tempdir().unwrap();
        let (root, env) = (dir.path(), "env1");
        let fields = vec!["changes".to_string()];

        let base = json!({"ref_name": "enrich", "changes": [{"catalog": "c", "operation": "create", "schema": "s"}]});
        let h1 = wxctl_providers::identity_hash(&base, &fields, Some("generation"), 8);
        let mut data = base.clone();
        data["identity_hash"] = json!(h1);
        let resource = make_validated(schema, "sal_like", "enrich", data.clone());

        // Fresh store → no match → Create path.
        assert!(local_hash_skip_match(schema, &resource, root, env).is_none());

        // Recorded → exists:true, and compare over state_fields [] is NoChange.
        wxctl_providers::local_hash::record_run_hash_at(root, env, "sal_like", "enrich", &h1).unwrap();
        let found = local_hash_skip_match(schema, &resource, root, env).expect("recorded hash matches");
        assert!(found.exists);
        let (lv, rv) = make_test_resources(data.clone(), found.data.clone(), schema);
        assert!(matches!(SchemaBasedReconciler::new().compare(&lv, &rv), StateComparison::NoChange));

        // Changed hashed input → new hash → no record → Create (prior record intact).
        let mut changed = base.clone();
        changed["changes"][0]["schema"] = json!("s2");
        let h2 = wxctl_providers::identity_hash(&changed, &fields, Some("generation"), 8);
        assert_ne!(h1, h2);
        changed["identity_hash"] = json!(h2);
        assert!(local_hash_skip_match(schema, &make_validated(schema, "sal_like", "enrich", changed), root, env).is_none());

        // Bumped generation → new hash → Create.
        let mut bumped = base.clone();
        bumped["generation"] = json!("1");
        let h3 = wxctl_providers::identity_hash(&bumped, &fields, Some("generation"), 8);
        assert_ne!(h1, h3);
        bumped["identity_hash"] = json!(h3);
        assert!(local_hash_skip_match(schema, &make_validated(schema, "sal_like", "enrich", bumped), root, env).is_none());
    }

    /// Build the ingestion_job identity shape: get_by_id discovery with a
    /// client-settable `id`, `name_field: id`, `storage: name_suffix`, and an
    /// explicit `state_fields: [id]` (no synthetic identity_hash — get_by_id
    /// never runs normalize_identity_hash). Mirrors the wired kind without a
    /// registry, by compiling a YAML literal (D10).
    fn make_ingestion_job_schema() -> &'static SchemaIr {
        const YAML: &str = r#"
resource:
  name: ingestion_job
  service: watsonx_data
  kind: ingestion_job
  version: v1
  api:
    base_path: /v3/lhingestion/api/v1/ingestion/jobs
    id_field: id
    list_endpoint: "/v3/lhingestion/api/v1/ingestion/jobs?limit=100"
    get_endpoint: "/v3/lhingestion/api/v1/ingestion/jobs/{id}"
    create_method: POST
    delete_method: DELETE
  schema:
    fields: []
  reconciliation:
    discovery:
      method: get_by_id
      id_source: id
      name_field: id
    identity_hash:
      fields: [engine_id, source, target, partition_by, execute_config]
      nonce_field: generation
      storage: name_suffix
      length: 8
    state_fields: [id]
    update_strategy: recreate
    use_json_patch: false
"#;
        wxctl_schema::ir_support::compile_to_static_ir(YAML).expect("valid test schema yaml")
    }

    #[test]
    fn ingestion_job_hash_in_id_nochange_when_same_create_when_changed() {
        let schema = make_ingestion_job_schema();
        let reconciler = SchemaBasedReconciler::new();
        let fields: Vec<String> = vec!["engine_id".into(), "source".into(), "target".into(), "partition_by".into(), "execute_config".into()];

        // Baseline authored inputs (no generation) → hash → suffixed id, exactly
        // as the generic name_suffix validation step produces at apply time.
        let base = json!({"engine_id": "spark-1", "source": {"file_type": "csv"}, "target": {"table": "t"}});
        let h1 = wxctl_providers::identity_hash(&base, &fields, Some("generation"), 8);
        let id1 = format!("job1-{h1}");

        // Unchanged re-apply: get_by_id returns the same suffixed id (job_id → id).
        // compare over state_fields=[id] → NoChange; immutable=[] adds no drift.
        let (lv, rv) = make_test_resources(json!({"id": id1}), json!({"id": id1}), schema);
        assert!(matches!(reconciler.compare(&lv, &rv), StateComparison::NoChange), "same suffixed id → NoChange (AC1/AC8 offline shape)");

        // Changed hashed input → different hash → different id → GET-by-id 404 →
        // RemoteResource exists:false → Create (a new run), never Recreate (AC2).
        let changed = json!({"engine_id": "spark-1", "source": {"file_type": "parquet"}, "target": {"table": "t"}});
        let h2 = wxctl_providers::identity_hash(&changed, &fields, Some("generation"), 8);
        assert_ne!(h2, h1, "changed source → new hash → new id");
        let (lv2, rv2) = make_test_resources(json!({"id": format!("job1-{h2}")}), Value::Null, schema);
        let absent2 = RemoteResource { exists: false, ..rv2 };
        assert!(matches!(reconciler.compare(&lv2, &absent2), StateComparison::Create), "changed input → Create, never Recreate");

        // Bumped generation, all inputs unchanged → different hash → different id →
        // Create (AC3).
        let bumped = json!({"engine_id": "spark-1", "source": {"file_type": "csv"}, "target": {"table": "t"}, "generation": "2"});
        let h3 = wxctl_providers::identity_hash(&bumped, &fields, Some("generation"), 8);
        assert_ne!(h3, h1, "bumped generation → new hash → new id");
        let (lv3, rv3) = make_test_resources(json!({"id": format!("job1-{h3}")}), Value::Null, schema);
        let absent3 = RemoteResource { exists: false, ..rv3 };
        assert!(matches!(reconciler.compare(&lv3, &absent3), StateComparison::Create), "bumped generation → Create");
    }
}
