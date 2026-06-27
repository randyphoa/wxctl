use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use wxctl_core::client::{BodyKind, HttpClient, Method, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use crate::util::join_all_ok;

pub struct BusinessTermsHandler;

/// Term info needed for reconciliation
struct ExistingTerm {
    artifact_id: String,
    /// Published version_id (if published)
    version_id: String,
    /// Published revision (if published)
    revision: String,
    /// Draft version info if a draft exists
    draft: Option<DraftInfo>,
}

impl ExistingTerm {
    /// Get version info for the published term
    fn published_version(&self) -> TermVersion<'_> {
        TermVersion { artifact_id: &self.artifact_id, version_id: &self.version_id, revision: &self.revision }
    }

    /// Get version info for the draft (if exists)
    fn draft_version(&self) -> Option<TermVersion<'_>> {
        self.draft.as_ref().map(|d| TermVersion { artifact_id: &self.artifact_id, version_id: &d.version_id, revision: &d.revision })
    }
}

/// Draft version info
#[derive(Clone)]
struct DraftInfo {
    version_id: String,
    revision: String,
}

/// Version identifier for PATCH operations
struct TermVersion<'a> {
    artifact_id: &'a str,
    version_id: &'a str,
    revision: &'a str,
}

impl ResourceHandler for BusinessTermsHandler {
    /// Handle bulk term creation by transforming the payload to match API expectations
    /// Supports two modes:
    /// 1. Inline terms: The API expects an array of terms directly
    /// 2. Import file: Upload a CSV file to the import endpoint
    ///
    /// For inline terms, this handler implements reconciliation:
    /// - If a term has an existing draft: PATCH the draft to update it
    /// - If a term is published with no draft: PATCH to create a linked draft
    /// - If a term is new: POST to create a new draft term
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            // Check if import_file mode is specified
            if let Some(import_file) = resource.get("import_file").and_then(|v| v.as_str()) {
                return self.handle_import(resource, client, import_file, operation_id).await;
            }

            // Inline terms mode - extract the terms array from the resource
            let terms = resource.get("terms").and_then(|t| t.as_array()).ok_or_else(|| anyhow!("business_terms requires either 'terms' array or 'import_file'"))?;

            // Transform each term to match API format
            let transformed_terms: Vec<Value> = terms.iter().map(|term| transform_term(term.clone())).collect();

            // Fetch existing terms (both published and drafts) for reconciliation
            let mut existing_terms = self.fetch_existing_terms(client, operation_id).await?;

            // Split terms into: new (POST), update draft (PATCH draft), create draft (PATCH published)
            // Use remove() to take ownership and avoid cloning
            let mut new_terms: Vec<Value> = Vec::new();
            let mut terms_to_update_draft: Vec<(Value, ExistingTerm)> = Vec::new();
            let mut terms_needing_draft: Vec<(Value, ExistingTerm)> = Vec::new();

            for term in transformed_terms {
                let term_name = term.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if let Some(existing) = existing_terms.remove(term_name) {
                    if existing.draft.is_some() {
                        // Draft exists - update it
                        tracing::debug!(
                            target: "wxctl::substage::provider",
                            operation_id = %operation_id,
                            term_name = %term_name,
                            "Will update existing draft"
                        );
                        terms_to_update_draft.push((term, existing));
                    } else {
                        // Published but no draft - needs PATCH to create linked draft
                        terms_needing_draft.push((term, existing));
                    }
                } else {
                    new_terms.push(term);
                }
            }

            let new_count = new_terms.len();
            let update_draft_count = terms_to_update_draft.len();
            let create_draft_count = terms_needing_draft.len();

            tracing::debug!(
                target: "wxctl::substage::provider",
                operation_id = %operation_id,
                new_term_count = new_count,
                update_draft_count = update_draft_count,
                create_draft_count = create_draft_count,
                "Processing business terms (new via POST, published via PATCH, drafts via PATCH)"
            );

            let mut responses: Vec<Value> = Vec::new();

            // PATCH existing drafts to update them (in parallel)
            if !terms_to_update_draft.is_empty() {
                let draft_futures = terms_to_update_draft.iter().map(|(term, existing)| self.patch_term(client, operation_id, term, existing.draft_version().unwrap(), "PATCHing existing draft term to update it"));
                responses.extend(join_all_ok(draft_futures).await?);
            }

            // PATCH published terms to create linked drafts (in parallel)
            if !terms_needing_draft.is_empty() {
                let published_futures = terms_needing_draft.iter().map(|(term, existing)| self.patch_term(client, operation_id, term, existing.published_version(), "PATCHing published term to create linked draft"));
                responses.extend(join_all_ok(published_futures).await?);
            }

            // POST new terms in bulk. skip_workflow_if_possible=true publishes them
            // immediately (live-proven 2026-06-15) so they land in the
            // `/v3/glossary_terms` + `governance_artifact_types?workflow_status=published`
            // lists — fetch_existing_terms and the version-path delete both depend on
            // that (a DRAFT-in-workflow term is invisible to those lists → re-create on
            // re-apply + a no-op delete on destroy).
            if !new_terms.is_empty() {
                let endpoint_with_workflow = format!("{}?skip_workflow_if_possible=true", endpoint);
                let post_response: Value = client.create(operation_id, &endpoint_with_workflow, json!(new_terms)).await?;
                responses.push(post_response);
            }

            // Return combined response
            Ok(HookOutcome::Handled(json!({
                "created": new_count,
                "drafts_updated": update_draft_count,
                "drafts_created": create_draft_count,
                "responses": responses
            })))
        })
    }

    /// Delete all configured terms on destroy via the version path.
    ///
    /// The bulk schema uses `discovery: skip`, so Destroy mode emits an optimistic
    /// Delete with `pre_delete` invoked. Terms are DRAFT-in-workflow (plain
    /// `DELETE /v3/glossary_terms/{id}` → 404); each version deletes via
    /// `DELETE /v3/glossary_terms/{id}/versions/{vid}`. Reuse `fetch_existing_terms`
    /// (name → published + draft versions) to resolve ids, then delete the draft
    /// and/or published version of every term named in the resource's `terms`.
    fn pre_delete<'a>(&'a self, resource: &'a Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let wanted: Vec<String> = resource.get("terms").and_then(|t| t.as_array()).map(|terms| terms.iter().filter_map(|t| t.get("name").and_then(|n| n.as_str()).map(str::to_string)).collect()).unwrap_or_default();
            if wanted.is_empty() {
                return Ok(HookOutcome::Handled(json!({"deleted": 0})));
            }
            let existing = self.fetch_existing_terms(client, operation_id).await?;
            let mut delete_endpoints: Vec<String> = Vec::new();
            for name in &wanted {
                if let Some(term) = existing.get(name) {
                    let draft_vid = term.draft_version().map(|d| d.version_id.to_string());
                    let published = term.published_version();
                    delete_endpoints.extend(version_delete_endpoints(published.artifact_id, draft_vid.as_deref(), Some(published.version_id)));
                }
            }
            let deleted = delete_endpoints.len();
            let delete_futures = delete_endpoints.iter().map(|ep| {
                let client = client.clone();
                let op_id = operation_id.to_string();
                let ep = ep.clone();
                async move {
                    let spec = RequestSpec::new(Method::DELETE, &ep).body(BodyKind::None);
                    client.execute::<Value>(&op_id, spec).await
                }
            });
            join_all_ok(delete_futures).await?;
            tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, deleted, "deleted bulk business_term versions");
            Ok(HookOutcome::Handled(json!({"deleted": deleted})))
        })
    }
}

impl BusinessTermsHandler {
    /// Fetch existing glossary terms (both published and drafts) for reconciliation
    ///
    /// Note: The bulk list endpoint `/v3/governance_artifact_types/glossary_term?workflow_status=DRAFT`
    /// does NOT return linked drafts (drafts created by PATCH on published terms).
    /// We must query `/v3/glossary_terms/{artifact_id}/versions?status=DRAFT` per artifact
    /// to detect linked drafts.
    async fn fetch_existing_terms<'a>(&'a self, client: &'a HttpClient, operation_id: &'a str) -> Result<HashMap<String, ExistingTerm>> {
        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            "Fetching existing glossary terms for reconciliation"
        );

        let mut existing_map: HashMap<String, ExistingTerm> = HashMap::new();

        // Fetch all published terms (paginated)
        let published_endpoint = "/v3/governance_artifact_types/glossary_term?workflow_status=published&limit=200";
        let all_published = crate::util::fetch_all_pages(client, operation_id, published_endpoint).await?;

        // Collect term info for parallel fetching
        let term_infos: Vec<_> = all_published
            .iter()
            .filter_map(|resource| {
                let name = resource.get("name").and_then(|n| n.as_str())?;
                let artifact_id = resource.get("artifact_id").and_then(|a| a.as_str())?;
                let version_id = resource.get("version_id").and_then(|v| v.as_str())?;
                Some((name.to_string(), artifact_id.to_string(), version_id.to_string()))
            })
            .collect();

        // Build endpoints for batch fetching term details
        let term_endpoints: Vec<String> = term_infos.iter().map(|(_, artifact_id, version_id)| format!("/v3/glossary_terms/{}/versions/{}", artifact_id, version_id)).collect();

        // Build endpoints for batch fetching draft info. Use the bare versions
        // endpoint (returns 200 with all versions) and filter for a DRAFT-state
        // version below — the `?status=DRAFT` query 404s on this API (proven live
        // 2026-06-15), which otherwise emits a spurious error event that fails the run.
        let draft_endpoints: Vec<String> = term_infos.iter().map(|(_, artifact_id, _)| format!("/v3/glossary_terms/{}/versions", artifact_id)).collect();

        // Execute both batches in parallel
        let term_endpoint_refs: Vec<&str> = term_endpoints.iter().map(|s| s.as_str()).collect();
        let draft_endpoint_refs: Vec<&str> = draft_endpoints.iter().map(|s| s.as_str()).collect();

        let (term_results, draft_results) = tokio::join!(client.get_many::<Value>(operation_id, &term_endpoint_refs), client.get_many::<Value>(operation_id, &draft_endpoint_refs));

        // Process results
        for (i, (name, artifact_id, version_id)) in term_infos.into_iter().enumerate() {
            match &term_results[i] {
                Ok(term_details) => {
                    let revision = term_details.get("metadata").and_then(|m| m.get("revision")).and_then(|r| r.as_str()).unwrap_or("0").to_string();

                    // Extract draft info from the batch result
                    let draft = match &draft_results[i] {
                        Ok(response) => {
                            let draft_info = (|| {
                                let resources = response.get("resources")?.as_array()?;
                                // The bare /versions endpoint returns all versions; a linked
                                // draft is the one whose state is DRAFT (published versions are
                                // PUBLISHED). No DRAFT version → no linked draft.
                                let draft_resource = resources.iter().find(|r| r.get("metadata").and_then(|m| m.get("state")).and_then(|s| s.as_str()) == Some("DRAFT"))?;
                                let metadata = draft_resource.get("metadata")?;
                                let draft_version_id = metadata.get("version_id")?.as_str()?;
                                let draft_revision = metadata.get("revision").and_then(|r| r.as_str()).unwrap_or("0");

                                Some(DraftInfo { version_id: draft_version_id.to_string(), revision: draft_revision.to_string() })
                            })();

                            if let Some(ref info) = draft_info {
                                tracing::debug!(
                                    target: "wxctl::substage::provider",
                                    operation_id = %operation_id,
                                    artifact_id = %artifact_id,
                                    draft_version_id = %info.version_id,
                                    draft_revision = %info.revision,
                                    "Found linked draft for published term"
                                );
                            }
                            draft_info
                        }
                        Err(e) => {
                            tracing::warn!(
                                target: "wxctl::substage::provider",
                                operation_id = %operation_id,
                                artifact_id = %artifact_id,
                                error = %e,
                                "Failed to check for draft - assuming no draft exists"
                            );
                            None
                        }
                    };

                    existing_map.insert(name, ExistingTerm { artifact_id, version_id, revision, draft });
                }
                Err(e) => {
                    tracing::warn!(
                        target: "wxctl::substage::provider",
                        operation_id = %operation_id,
                        term_name = %name,
                        artifact_id = %artifact_id,
                        error = %e,
                        "Failed to fetch term details - term will be treated as new"
                    );
                }
            }
        }

        // Also fetch standalone draft terms (never published) from bulk endpoint
        let draft_endpoint = "/v3/governance_artifact_types/glossary_term?workflow_status=DRAFT&limit=200";
        let all_drafts = crate::util::fetch_all_pages(client, operation_id, draft_endpoint).await?;

        // Filter to only standalone drafts not already in map
        let standalone_drafts: Vec<_> = all_drafts
            .iter()
            .filter_map(|resource| {
                let name = resource.get("name").and_then(|n| n.as_str())?;
                let artifact_id = resource.get("artifact_id").and_then(|a| a.as_str())?;
                let version_id = resource.get("version_id").and_then(|v| v.as_str())?;
                // Only include if not already in map (standalone draft, not linked to published)
                if existing_map.contains_key(name) { None } else { Some((name.to_string(), artifact_id.to_string(), version_id.to_string())) }
            })
            .collect();

        if !standalone_drafts.is_empty() {
            // Build endpoints for batch fetching draft revisions
            let revision_endpoints: Vec<String> = standalone_drafts.iter().map(|(_, artifact_id, version_id)| format!("/v3/glossary_terms/{}/versions/{}", artifact_id, version_id)).collect();

            let revision_endpoint_refs: Vec<&str> = revision_endpoints.iter().map(|s| s.as_str()).collect();
            let revision_results = client.get_many::<Value>(operation_id, &revision_endpoint_refs).await;

            // Process results
            for (i, (name, artifact_id, version_id)) in standalone_drafts.into_iter().enumerate() {
                let draft_revision = revision_results[i].as_ref().ok().and_then(|details| details.get("metadata").and_then(|m| m.get("revision")).and_then(|r| r.as_str()).map(|s| s.to_string())).unwrap_or_else(|| "0".to_string());

                existing_map.insert(name, ExistingTerm { artifact_id: artifact_id.clone(), version_id: version_id.clone(), revision: "0".to_string(), draft: Some(DraftInfo { version_id, revision: draft_revision }) });
            }
        }

        let published_only_count = existing_map.values().filter(|t| t.draft.is_none()).count();
        let draft_count = existing_map.values().filter(|t| t.draft.is_some()).count();

        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            published_only_count = published_only_count,
            draft_count = draft_count,
            "Found existing glossary terms"
        );

        Ok(existing_map)
    }

    /// PATCH a term (either published to create draft, or draft to update)
    async fn patch_term<'a>(&'a self, client: &'a HttpClient, operation_id: &'a str, term: &'a Value, version: TermVersion<'a>, log_message: &'a str) -> Result<Value> {
        // skip_workflow_if_possible=true publishes the modify immediately rather than
        // parking it as a MODIFY-workflow draft. On a governed cluster a plain PATCH
        // triggers a modify workflow whose draft is invisible to /versions discovery
        // (only /v3/workflows surfaces it), which then blocks the destroy's published-
        // version delete (404 WKCBG2116E). Mirrors the POST path's skip_workflow flag.
        let endpoint = format!("/v3/glossary_terms/{}/versions/{}?skip_workflow_if_possible=true", version.artifact_id, version.version_id);

        let mut patch_body = term.clone();
        patch_body["revision"] = json!(version.revision);

        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            artifact_id = %version.artifact_id,
            version_id = %version.version_id,
            revision = %version.revision,
            "{}", log_message
        );

        let spec = RequestSpec::new(Method::PATCH, &endpoint).body(BodyKind::Json(patch_body));

        client.execute(operation_id, spec).await
    }

    /// Handle CSV file import for business terms
    async fn handle_import<'a>(&'a self, resource: &'a Value, client: &'a HttpClient, import_file: &'a str, operation_id: &'a str) -> Result<HookOutcome> {
        // Resolve the file path
        let file_path = Path::new(import_file);
        if !file_path.exists() {
            return Err(anyhow!("Import file not found: {}. Path should be relative to config file or absolute.", import_file));
        }

        // Get optional parameters (only include if explicitly set)
        let merge_option = resource.get("merge_option").and_then(|v| v.as_str());

        let async_mode = resource.get("async_mode").and_then(|v| v.as_bool());

        tracing::debug!(
            target: "wxctl::substage::provider",
            operation_id = %operation_id,
            import_file = %import_file,
            merge_option = ?merge_option,
            async_mode = ?async_mode,
            "Importing business terms from CSV"
        );

        // Build endpoint with query parameters (only include if set)
        let mut query_params = Vec::new();
        if let Some(merge) = merge_option {
            query_params.push(format!("merge_option={}", merge));
        }
        if let Some(true) = async_mode {
            query_params.push("async_mode=true".to_string());
        }

        let import_endpoint = if query_params.is_empty() { "/v3/governance_artifact_types/glossary_term/import".to_string() } else { format!("/v3/governance_artifact_types/glossary_term/import?{}", query_params.join("&")) };

        // Upload the CSV file using multipart form
        let response: Value = client.upload_file(operation_id, &import_endpoint, file_path, "file").await?;

        Ok(HookOutcome::Handled(response))
    }
}

/// Transform a term object to match the API expected format
/// - Extracts artifact_id from resolved parent_category to create {"id": "..."}
fn transform_term(mut term: Value) -> Value {
    if let Some(parent_category) = term.get("parent_category")
        && let Some(id) = crate::util::extract_artifact_id(parent_category)
    {
        term["parent_category"] = json!({"id": id});
    }

    term
}

/// Build the version-path delete endpoints for a term given its resolved
/// `artifact_id`, an optional draft `version_id`, and an optional published
/// `version_id`. Both versions are deleted when present so destroy removes the
/// term in any workflow status. `skip_workflow_if_possible=true` makes the delete
/// immediate (204); without it the delete is parked as a DELETE-workflow draft
/// (201) and the term lingers in the list — proven live 2026-06-15.
fn version_delete_endpoints(artifact_id: &str, draft_version_id: Option<&str>, published_version_id: Option<&str>) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(vid) = draft_version_id {
        out.push(format!("/v3/glossary_terms/{}/versions/{}?skip_workflow_if_possible=true", artifact_id, vid));
    }
    if let Some(vid) = published_version_id.filter(|v| !v.is_empty()) {
        out.push(format!("/v3/glossary_terms/{}/versions/{}?skip_workflow_if_possible=true", artifact_id, vid));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transform_term_extracts_parent_category_id() {
        let term = json!({"name": "Email", "parent_category": {"resources": [{"artifact_id": "cat-9"}]}});
        let out = transform_term(term);
        assert_eq!(out.get("parent_category"), Some(&json!({"id": "cat-9"})));
    }

    // version_delete_endpoints builds the version-path deletes (skip_workflow_if_possible=true)
    // for whichever of draft/published version ids are present; an empty published id is skipped.
    #[test]
    fn version_delete_endpoints_cases() {
        type Case<'a> = (&'a str, Option<&'a str>, Option<&'a str>, Vec<&'a str>);
        let cases: &[Case] = &[
            ("draft + published", Some("draft-v"), Some("pub-v"), vec!["/v3/glossary_terms/art-1/versions/draft-v?skip_workflow_if_possible=true", "/v3/glossary_terms/art-1/versions/pub-v?skip_workflow_if_possible=true"]),
            ("skips empty published", None, Some(""), vec![]),
            ("draft only", Some("draft-v"), None, vec!["/v3/glossary_terms/art-1/versions/draft-v?skip_workflow_if_possible=true"]),
        ];
        for (msg, draft, published, expected) in cases {
            let eps = version_delete_endpoints("art-1", *draft, *published);
            assert_eq!(eps, expected.iter().map(|s| s.to_string()).collect::<Vec<_>>(), "{msg}");
        }
    }
}
