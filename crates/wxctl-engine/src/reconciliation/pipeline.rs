use super::references::check_dependencies;
use super::types::{DiscoveryStatus, Operation, OperationType, ReconciliationError, ReconciliationPlan, SkipReason};
use crate::context::RuntimeIdStore;
use crate::execution::resolution::{resolve_dependencies, resolve_dependencies_partial};
use crate::execution::{ExecutionObserver, NoOpObserver};
use anyhow::Result;
use std::sync::Arc;
use tracing::{Instrument, info_span};
use wxctl_core::logging::redact_for_log;
use wxctl_core::registry::ResourceDescriptor;
use wxctl_core::{ClientFactory, OnDestroyPolicy, Reconciler, RemoteResource, ResourceRegistry, StateComparison, ValidatedResource};
use wxctl_schema::schema::DiscoveryMethod;

/// Fires `on_reconcile_resource_complete` exactly once when dropped, regardless
/// of which `continue` / `?` path ends the loop iteration. `success` defaults to
/// `true` and is set `false` on a discovery-error branch. Display-only: the guard
/// never affects the reconcile outcome.
struct ReconcileResourceGuard<'a> {
    observer: &'a dyn ExecutionObserver,
    key: &'a wxctl_core::ResourceKey,
    success: bool,
}

impl Drop for ReconcileResourceGuard<'_> {
    fn drop(&mut self) {
        self.observer.on_reconcile_resource_complete(self.key, self.success);
    }
}

/// Selects the operation kind emitted when a remote is discovered during
/// reconciliation. `Apply` runs the normal state-comparison machinery (Create
/// / Update / Recreate / NoOp / Delete); `Destroy` short-circuits every
/// discovered remote to `Delete` and emits nothing when nothing is found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconcileMode {
    Apply,
    Destroy,
}

pub struct ReconciliationPipeline {
    registry: Arc<ResourceRegistry>,
    client_factory: Arc<ClientFactory>,
    observer: Arc<dyn ExecutionObserver>,
}

/// Log a structured `wxctl::error` event and append the same message to the
/// reconciliation plan's error list so the pipeline guard can render it.
fn record_reconciliation_error(operation_id: &str, code: &str, kind: &str, name: &str, msg: String, remediation: &str, errors: &mut Vec<ReconciliationError>) {
    wxctl_core::log_error_resource!(operation_id, "reconciliation", code, kind, name, &msg, remediation);
    errors.push(ReconciliationError { kind: kind.to_string(), name: name.to_string(), error: msg });
}

fn handle_discovery_error(operation_id: &str, kind: &str, name: &str, error: anyhow::Error, errors: &mut Vec<ReconciliationError>) {
    record_reconciliation_error(operation_id, wxctl_core::logging::error_codes::R001, kind, name, format!("{:#}", error), "Check network connectivity and API credentials, then retry", errors);
}

#[allow(clippy::too_many_arguments)]
async fn enrich_and_cache(remotes: &mut [wxctl_core::RemoteResource], key: &wxctl_core::ResourceKey, registry: &ResourceRegistry, client: &wxctl_core::client::HttpClient, runtime_store: &RuntimeIdStore, operation_id: &str, is_apply: bool) -> Result<()> {
    if let Some(first_remote) = remotes.first_mut() {
        if let Some(handler) = registry.get_handler(&key.kind) {
            let span = info_span!(target: "wxctl::substage::hook", "post_discover", operation_id = %operation_id, hook = "post_discover", handler_kind = %key.kind, resource_kind = %key.kind, resource_name = %key.name, is_apply);
            let before = first_remote.data.clone();
            handler.post_discover(&mut first_remote.data, client, operation_id, is_apply).instrument(span).await?;
            // Resource-level superset: discovered remote data is CAMS-shaped, so the
            // response-envelope spellings (`[results.]entity.<kind>.<path>`) are needed
            // to mask e.g. job_run's round-tripped `configuration.env_variables`.
            let sensitive = registry.get_descriptor(&key.kind).map(|d| d.schema.resource.sensitive_paths()).unwrap_or_default();
            tracing::debug!(target: "wxctl::substage::hook", operation_id = %operation_id, hook = "post_discover", handler_kind = %key.kind, before = %serde_json::to_string(&redact_for_log(&before, &sensitive)).unwrap_or_default(), after = %serde_json::to_string(&redact_for_log(&first_remote.data, &sensitive)).unwrap_or_default(), "hook payload diff");
        }
        runtime_store.insert(key.clone(), first_remote.data.clone());
    }
    Ok(())
}

/// Shared post-resolution comparison tail for a single discovered remote, used
/// by both the `Discovered` branch and the `Deferred`-but-found Apply branch.
/// Runs the state comparison, mirrors the R005 immutable-drift reject (when
/// `reject_on_immutable_drift` is set), then maps the comparison to an op and
/// pushes it with the matching decision log. The Deferred branch's
/// templated-field gate runs in the caller BEFORE this helper — this tail is
/// only reached once the comparison is known to be meaningful.
#[allow(clippy::too_many_arguments)]
fn apply_compare_to_op(reconciler: &dyn Reconciler, operation_id: &str, resource: &ValidatedResource, local_resource: &ValidatedResource, remote: RemoteResource, operations: &mut Vec<Operation>, errors: &mut Vec<ReconciliationError>) {
    let comparison = reconciler.compare(local_resource, &remote);

    // Per-kind opt-in: an immutable drift on a reject-on-drift kind is a hard
    // R005 error, not a Recreate op.
    if let StateComparison::Recreate { field, local_value, remote_value } = &comparison
        && resource.descriptor.schema.resource.reconciliation.reject_on_immutable_drift
    {
        let identity_hint = resource.descriptor.schema.resource.reconciliation.discovery.identity_match.as_ref().map(|im| im.local_path.as_str()).unwrap_or("name");
        let identity_value = super::schema_reconciler::render_value(super::schema_reconciler::get_nested_field(&local_resource.data, identity_hint));
        let kind = &resource.key.kind;
        let msg = format!(
            "{identity_hint} '{identity_value}' is already in use by another {kind} (immutable field '{field}' differs: local='{local_value}' vs remote='{remote_value}'). Choose a different `{identity_hint}` in your config, or destroy the existing {kind} first (via the watsonx.data UI or `wxctl destroy`)."
        );
        record_reconciliation_error(operation_id, wxctl_core::logging::error_codes::R005, kind, &resource.key.name, msg, "Rename the identity field or destroy the pre-existing resource.", errors);
        return;
    }

    let (op_type, decision, reason, changed_fields) = match comparison {
        StateComparison::Update { fields } => {
            let cf = fields.join(",");
            (OperationType::Update { fields: fields.clone() }, "Update", format!("{} fields differ", fields.len()), cf)
        }
        StateComparison::Delete => (OperationType::Delete, "Delete", "resource should be removed".to_string(), String::new()),
        StateComparison::Recreate { field, .. } => (OperationType::Recreate, "Recreate", format!("Immutable field '{}' changed", field), String::new()),
        StateComparison::NoChange => (OperationType::NoOp, "NoOp", "no differences".to_string(), String::new()),
        StateComparison::Create => unreachable!("StateComparison::Create impossible when a remote was discovered"),
    };

    wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, &reason, &changed_fields);
    operations.push(Operation { key: resource.key.clone(), op_type, local: Some(local_resource.clone()), remote: Some(remote) });
}

/// Map a resource's teardown policy to the `(op_type, decision_label)` pair
/// emitted in Destroy mode. `Retain` short-circuits the handler; `Delete`
/// runs the normal teardown path.
fn destroy_op(policy: OnDestroyPolicy) -> (OperationType, &'static str) {
    match policy {
        OnDestroyPolicy::Retain => (OperationType::Retain, "Retain"),
        OnDestroyPolicy::Delete => (OperationType::Delete, "Delete"),
    }
}

/// Map a skip reason to the `(op_type, decision_label)` pair emitted when
/// reconciliation drops a destroy candidate — either because the remote is
/// absent or because dependencies couldn't be resolved. Mirrors `destroy_op`
/// for the Skip short-circuit. Two distinct decision labels so the output
/// layer's per-string counters (`OperationSummary::add_decision`) can route
/// each to its own bucket.
fn skip_op(reason: SkipReason) -> (OperationType, &'static str) {
    let label = match reason {
        SkipReason::Absent => "SkipAbsent",
        SkipReason::Deferred => "SkipDeferred",
    };
    (OperationType::Skip { reason }, label)
}

fn get_or_create_client(clients: &mut std::collections::HashMap<String, wxctl_core::client::HttpClient>, client_factory: &ClientFactory, service: &str) -> Result<wxctl_core::client::HttpClient> {
    if let Some(existing) = clients.get(service) {
        return Ok(existing.clone());
    }
    let new_client = client_factory.create_client(service)?;
    clients.insert(service.to_string(), new_client.clone());
    Ok(new_client)
}

impl ReconciliationPipeline {
    pub fn new(registry: Arc<ResourceRegistry>, client_factory: Arc<ClientFactory>) -> Self {
        Self { registry, client_factory, observer: Arc::new(NoOpObserver) }
    }

    pub fn with_observer(registry: Arc<ResourceRegistry>, client_factory: Arc<ClientFactory>, observer: Arc<dyn ExecutionObserver>) -> Self {
        Self { registry, client_factory, observer }
    }

    pub async fn reconcile(&self, operation_id: &str, resources: Vec<ValidatedResource>, runtime_store: &RuntimeIdStore, mode: ReconcileMode, is_apply: bool) -> Result<ReconciliationPlan> {
        let span = info_span!(
            target: "wxctl::stage::reconciliation",
            "reconciliation",
            operation_id = %operation_id,
            resource_count = resources.len(),
            status = tracing::field::Empty
        );

        async {
            let mut operations = Vec::new();
            let mut errors = Vec::new();

            // Cache for service-specific HTTP clients
            let mut clients: std::collections::HashMap<String, wxctl_core::client::HttpClient> = std::collections::HashMap::new();

            let total = resources.len();
            self.observer.on_reconcile_start(total);

            for resource in resources {
                self.observer.on_reconcile_resource_start(&resource.key);
                let recon_key = resource.key.clone();
                let mut recon_guard = ReconcileResourceGuard { observer: self.observer.as_ref(), key: &recon_key, success: true };

                // Resolve the active deployment for this resource's service and apply
                // any matching overlay from the schema's `deployments:` map. The base
                // descriptor was compiled at registry-load time (deployment-agnostic);
                // here we produce an effective descriptor that reflects per-deployment
                // API/schema/reconciliation overrides before any reconciliation work.
                let deployment = self.client_factory.deployment_for_service(&resource.descriptor.service)?;
                let base_def = &resource.descriptor.schema.resource;

                // Check unsupported_on before doing any work.
                if wxctl_schema::schema::is_unsupported_on(base_def, &deployment) {
                    let constraint = base_def.unsupported_on.iter().find(|c| deployment.matches(c)).map(|c| c.to_string()).unwrap_or_default();
                    let msg = format!("[{}] kind '{}' is not supported on '{}' (matches unsupported_on constraint '{}')", wxctl_core::logging::error_codes::R004, resource.key.kind, deployment, constraint);
                    record_reconciliation_error(operation_id, wxctl_core::logging::error_codes::R004, &resource.key.kind, &resource.key.name, msg, "Remove this resource from your config or switch to a supported deployment.", &mut errors);
                    continue;
                }

                // Apply the deployment overlay to get the effective ResourceDefinition.
                // When the schema has no `deployments` map (the common case) this returns
                // the base unchanged (cloned). When an overlay is found, the returned
                // definition carries the merged values; we then rebuild the descriptor so
                // every downstream consumer (endpoints, field descriptors, schema ref)
                // sees the overlay-applied values.
                let resource = if base_def.deployments.is_some() {
                    let effective_def = wxctl_schema::schema::effective_definition(base_def, &deployment)?;
                    let mut merged_schema = resource.descriptor.schema.clone();
                    merged_schema.resource = effective_def;
                    let new_descriptor = Arc::new(ResourceDescriptor::from_schema(&merged_schema)?);
                    ValidatedResource { descriptor: new_descriptor, ..resource }
                } else {
                    resource
                };

                // Get reconciler for this resource type
                let reconciler = match self.registry.get_reconciler(&resource.key.kind) {
                    Some(r) => r,
                    None => {
                        // Skip if no reconciler registered
                        continue;
                    }
                };

                // Get or create client for this service
                let client = get_or_create_client(&mut clients, &self.client_factory, &resource.descriptor.service)?;

                // Check if all dependencies exist in the cache
                let missing_deps = check_dependencies(&resource, runtime_store);

                let resolved_resource = if missing_deps.is_empty() {
                    match resolve_dependencies(&resource.data, runtime_store, &resource.descriptor.schema) {
                        Ok(resolved_data) => {
                            let mut resolved = resource.clone();
                            resolved.data = resolved_data;
                            Some(resolved)
                        }
                        Err(e) => {
                            tracing::debug!(
                                target: "wxctl::dependency",
                                operation_id = %operation_id,
                                resource_type = %resource.key.kind,
                                resource_name = %resource.key.name,
                                status = "deferred",
                                reason = "template_resolution_failed",
                                error_code = wxctl_core::logging::error_codes::T001,
                                error = %e,
                                "Template resolution failed, deferring"
                            );
                            None
                        }
                    }
                } else {
                    None
                };

                // Deferred path: PARTIALLY resolve `${...}` refs from the state of
                // already-reconciled resources (topo order guarantees deps come
                // first); refs to deps with no discovered state stay templated. A
                // single undiscovered dep (e.g. an adopt-only kind whose decision is
                // Create) then no longer poisons every transitive downstream into
                // skipped discovery + CreateUnchecked — discovery runs whenever the
                // identity-relevant paths resolved, so an unchanged re-apply plans
                // NoChange instead of blind-POSTing a duplicate (live-proven: `job`
                // 400 "A job with the same name already exists"). On a from-scratch
                // first apply the store has no dep state, partial resolution is an
                // identity transform, and the CreateUnchecked behavior is unchanged.
                let partial_resource = if resolved_resource.is_none() {
                    let mut partial = resource.clone();
                    partial.data = resolve_dependencies_partial(&resource.data, runtime_store, &resource.descriptor.schema);
                    Some(partial)
                } else {
                    None
                };

                let discovery_status = if !missing_deps.is_empty() {
                    for dep_key in &missing_deps {
                        wxctl_core::log_dependency_deferred!(operation_id, &resource.key.kind, &resource.key.name, &dep_key.kind, &dep_key.name);
                    }
                    DiscoveryStatus::Deferred { missing_dependencies: missing_deps }
                } else if resolved_resource.is_none() {
                    DiscoveryStatus::Deferred { missing_dependencies: vec![] }
                } else {
                    // Discover ALL remote resources with matching name
                    let resolved = resolved_resource.as_ref().unwrap();
                    match reconciler.discover_all(operation_id, resolved, client.clone()).await {
                        Ok(mut remotes) => {
                            // Destroy mode seeds the cache with resolved-local data on a miss so
                            // dependents' template refs still resolve; without this the cascade
                            // would leave everything downstream Deferred with no Delete ops.
                            // `exists: false` keeps it from being treated as a genuine remote.
                            if remotes.is_empty() && mode == ReconcileMode::Destroy {
                                remotes.push(RemoteResource { key: resource.key.clone(), data: resolved.data.clone(), exists: false });
                            }
                            enrich_and_cache(&mut remotes, &resource.key, &self.registry, &client, runtime_store, operation_id, is_apply).await?;
                            if remotes.iter().any(|r| r.exists) { DiscoveryStatus::Discovered { remotes: remotes.into_iter().filter(|r| r.exists).collect() } } else { DiscoveryStatus::NotFound }
                        }
                        Err(e) => {
                            // Destroy tolerates list failures (e.g. a schema's catalog already
                            // deleted) rather than aborting the whole teardown on one 400.
                            if mode == ReconcileMode::Destroy {
                                tracing::warn!(target: "wxctl::substage::reconciliation", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, error = %e, "destroy: tolerating discovery error");
                                let mut synthetic = vec![RemoteResource { key: resource.key.clone(), data: resolved.data.clone(), exists: false }];
                                enrich_and_cache(&mut synthetic, &resource.key, &self.registry, &client, runtime_store, operation_id, is_apply).await?;
                                DiscoveryStatus::NotFound
                            } else {
                                // Apply/plan: discovery failed with a non-404 — we can't tell
                                // create-vs-update. Record the reconciliation error (plan still
                                // fails) AND emit an `Undetermined` decision so the Changes
                                // section shows a truthful red `!` row instead of dropping the
                                // resource (spec AC17). No operation is pushed: the plan fails
                                // before execution, so there is nothing to execute.
                                wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, "Undetermined", "discovery failed; create-vs-update could not be determined");
                                handle_discovery_error(operation_id, &resource.key.kind, &resource.key.name, e, &mut errors);
                                recon_guard.success = false;
                                continue;
                            }
                        }
                    }
                };

                // The comparison/create-body local: fully resolved when all deps were
                // cached, otherwise the partially-resolved variant (still-templated
                // fields are skipped by `compare` and re-resolved at execution time).
                let local_resource = resolved_resource.or(partial_resource).unwrap_or_else(|| resource.clone());

                // Generate operations based on discovery status
                match discovery_status {
                    DiscoveryStatus::Discovered { remotes } => {
                        // Generate an operation for EACH matching remote
                        for remote in remotes {
                            if mode == ReconcileMode::Destroy {
                                let (op_type, decision) = destroy_op(resource.on_destroy);
                                wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, "destroying resource");
                                // `local` stays the ORIGINAL (pre-resolution) resource so execution-time
                                // enrich_with_linked_refs can extract ref names from `${...}` templates.
                                operations.push(Operation { key: resource.key.clone(), op_type, local: Some(resource.clone()), remote: Some(remote) });
                                continue;
                            }

                            apply_compare_to_op(reconciler.as_ref(), operation_id, &resource, &local_resource, remote, &mut operations, &mut errors);
                        }
                    }
                    DiscoveryStatus::NotFound => {
                        if mode == ReconcileMode::Destroy {
                            // Skip-method kinds can't be listed, so discovery always says NotFound.
                            // Emit an optimistic Delete — the handler's pre_delete must tolerate a
                            // truly-absent resource (symmetric to apply's create-or-adopt path).
                            if matches!(resource.descriptor.schema.resource.reconciliation.discovery.method, DiscoveryMethod::Skip) {
                                let synthetic = RemoteResource { key: resource.key.clone(), data: local_resource.data.clone(), exists: true };
                                let (op_type, decision) = destroy_op(resource.on_destroy);
                                wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, "destroying resource (skip-discovery, optimistic)");
                                operations.push(Operation { key: resource.key.clone(), op_type, local: Some(resource.clone()), remote: Some(synthetic) });
                                continue;
                            }
                            // Non-skip-method kinds: the remote wasn't found during discovery. Emit a
                            // Skip op so the destroy summary surfaces it as `skipped (absent)` instead
                            // of silently dropping the resource from the plan.
                            let (op_type, decision) = skip_op(SkipReason::Absent);
                            wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, "absent");
                            let synthetic = RemoteResource { key: resource.key.clone(), data: local_resource.data.clone(), exists: false };
                            operations.push(Operation { key: resource.key.clone(), op_type, local: Some(resource.clone()), remote: Some(synthetic) });
                            continue;
                        }
                        wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, "Create", "resource does not exist");

                        operations.push(Operation { key: resource.key.clone(), op_type: OperationType::Create, local: Some(local_resource), remote: None });
                    }
                    DiscoveryStatus::Deferred { missing_dependencies } => {
                        let dep_names: Vec<String> = missing_dependencies.iter().map(|k| format!("{}.{}", k.kind, k.name)).collect();
                        let reason = format!("deferred: dependencies not discovered [{}]", dep_names.join(", "));

                        // Try to discover the remote resource by name to determine Create vs Update.
                        // `local_resource` carries the PARTIALLY-resolved data here (refs to
                        // already-reconciled deps are real values), so discovery runs whenever
                        // the identity-relevant paths resolved. If any scoping/identity param
                        // still has an unresolved template ref, discover_all skips the API
                        // call and returns empty — the resource is treated as Create.
                        let remotes = match reconciler.discover_all(operation_id, &local_resource, client.clone()).await {
                            Ok(remotes) => remotes,
                            // Destroy tolerates discovery failures on the Deferred path too —
                            // symmetric with the Discovered path above — so one 400/500 on a
                            // deferred resource can't abort the whole teardown. Empty remotes
                            // flow into the existing skip/optimistic-delete logic below.
                            Err(e) if mode == ReconcileMode::Destroy => {
                                tracing::warn!(target: "wxctl::substage::reconciliation", operation_id = %operation_id, resource_type = %resource.key.kind, resource_name = %resource.key.name, error = %e, "destroy: tolerating discovery error");
                                vec![]
                            }
                            Err(e) => {
                                wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, "Undetermined", "discovery failed; create-vs-update could not be determined");
                                handle_discovery_error(operation_id, &resource.key.kind, &resource.key.name, e, &mut errors);
                                recon_guard.success = false;
                                continue;
                            }
                        };

                        if remotes.is_empty() {
                            if mode == ReconcileMode::Destroy {
                                // A skip-discovery kind can never be listed, so its refs being
                                // unresolvable at destroy-plan time must NOT drop it from the plan:
                                // its `pre_delete` deletes configured items by name (e.g. each
                                // `rules[].name` / `terms[].name`), never the resolved trigger/parent.
                                // Emit the optimistic Delete here — mirroring the `NotFound` +
                                // skip-discovery path above — so the handler's teardown still runs.
                                // (Honors `on_destroy`: a `Retain` skip-kind still short-circuits.)
                                if matches!(resource.descriptor.schema.resource.reconciliation.discovery.method, DiscoveryMethod::Skip) {
                                    let synthetic = RemoteResource { key: resource.key.clone(), data: resource.data.clone(), exists: true };
                                    let (op_type, decision) = destroy_op(resource.on_destroy);
                                    wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, "destroying resource (skip-discovery, optimistic, unresolved refs)");
                                    operations.push(Operation { key: resource.key.clone(), op_type, local: Some(resource.clone()), remote: Some(synthetic) });
                                    continue;
                                }
                                // Non-skip kinds whose deps/templates couldn't resolve: surface as
                                // `skipped (deferred)` so the destroy summary distinguishes
                                // "couldn't determine" from "absent".
                                let reason_msg = if dep_names.is_empty() { "deferred: template resolution failed".to_string() } else { format!("deferred: missing deps [{}]", dep_names.join(", ")) };
                                let (op_type, decision) = skip_op(SkipReason::Deferred);
                                wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, &reason_msg);
                                let synthetic = RemoteResource { key: resource.key.clone(), data: resource.data.clone(), exists: false };
                                operations.push(Operation { key: resource.key.clone(), op_type, local: Some(resource.clone()), remote: Some(synthetic) });
                                continue;
                            }
                            // Empty remotes on the Deferred path can mean either "nothing
                            // there" or "couldn't check because identity paths are still
                            // templated". Surface the latter as `CreateUnchecked` so the
                            // user sees the uncertainty instead of a confident `+ create`.
                            // Checked against the PARTIALLY-resolved data: identity paths
                            // resolved from already-reconciled deps mean discovery genuinely
                            // ran and found nothing — a confident `Create`.
                            let (decision, reason_str) =
                                if let Some(tpl) = super::schema_reconciler::identity_paths_unresolved(&local_resource.data, &resource.descriptor.schema) { ("CreateUnchecked", format!("unchecked: identity path has unresolved template `{tpl}`")) } else { ("Create", reason.clone()) };
                            wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, &reason_str);

                            operations.push(Operation { key: resource.key.clone(), op_type: OperationType::Create, local: Some(local_resource), remote: None });
                        } else {
                            let mut remotes = remotes;
                            enrich_and_cache(&mut remotes, &resource.key, &self.registry, &client, runtime_store, operation_id, is_apply).await?;

                            if mode == ReconcileMode::Destroy {
                                // Destroy: behavior unchanged — short-circuit each found remote
                                // to the on_destroy policy op. The local stays the ORIGINAL
                                // (pre-resolution) resource so execution-time enrichment can
                                // still read `${...}` ref names from templates.
                                let (op_type, decision) = destroy_op(resource.on_destroy);
                                for remote in remotes {
                                    wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, decision, &reason);
                                    operations.push(Operation { key: resource.key.clone(), op_type: op_type.clone(), local: Some(resource.clone()), remote: Some(remote) });
                                }
                            } else {
                                // Apply: run the SAME state comparison as the Discovered branch
                                // against each found remote, so a re-apply of an unchanged resource
                                // reports NoChange instead of a phantom `~ update` with no fields.
                                //
                                // The deferred local data may still carry `${...}` templates for the
                                // undiscovered dependency; `compare` skips any compared field whose
                                // local value is still templated (no real value to diff), comparing
                                // only the fully-resolved fields. Fall back to the conservative blind
                                // `Update { fields: vec![] }` ONLY when the comparison would be wholly
                                // vacuous — every present compared field is still templated, so there
                                // is nothing literal to anchor a NoChange/Update decision on.
                                let (comparable_fields, templated_fields) = super::schema_reconciler::compared_field_resolution(&local_resource.data, &resource.descriptor.schema);
                                let comparison_vacuous = comparable_fields == 0 && templated_fields > 0;
                                for remote in remotes {
                                    if comparison_vacuous {
                                        // No fully-resolved compared field exists — comparison can't
                                        // run meaningfully. Keep the conservative blind Update.
                                        wxctl_core::log_decision!(operation_id, &resource.key.kind, &resource.key.name, "Update", &format!("{reason}; all compared fields have unresolved templates"));
                                        operations.push(Operation { key: resource.key.clone(), op_type: OperationType::Update { fields: vec![] }, local: Some(local_resource.clone()), remote: Some(remote) });
                                        continue;
                                    }

                                    apply_compare_to_op(reconciler.as_ref(), operation_id, &resource, &local_resource, remote, &mut operations, &mut errors);
                                }
                            }
                        }
                    }
                }
            }

            if errors.is_empty() {
                tracing::Span::current().record("status", "completed");
            } else {
                tracing::Span::current().record("status", "failed");
                tracing::debug!(target: "wxctl::substage::reconciliation", status = "failed", "reconciliation stage failed");
            }
            Ok(ReconciliationPlan { operations, errors })
        }
        .instrument(span)
        .await
    }
}
