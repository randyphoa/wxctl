use crate::cloud_object_storage::cos_client::{CosAuth, CosClient, CosRequest, parse_s3_error};
use anyhow::{Result, anyhow};
use reqwest::Method;
use serde_json::{Value, json};
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;
use wxctl_core::client::{BodyKind, HttpClient, RequestSpec};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

pub struct WmlModelHandler;

const TRAININGS: &str = "/ml/v4/trainings";
const MODELS: &str = "/ml/v4/models";
const ASSET_FILES: &str = "/v2/asset_files";
const VERSION: &str = "2024-01-01";
const MODEL_TYPE: &str = "wml-hybrid_0.1";
/// AutoAI writes the per-pipeline wml_model request.json under a `_<phase>` dir.
/// The winning metric's `context.phase` is tried first; this is the documented
/// fallback when that dir's request.json lacks a `content_location` (spec Q3/Q4).
const FALLBACK_PHASE: &str = "compose_model_type_output";
/// Platform host for the spaces-credentials GET on SaaS. The wml_model handler
/// runs on the ML-host watsonx_ai client, but `/v2/spaces` is platform-host —
/// reached exactly as the sibling wml_deployment handler reaches task-credentials
/// (`wml_deployment.rs`: same constant): `client.raw_client()` + this URL + the
/// account-scoped IAM bearer (`get_token`). Global/public IBM Cloud host (not a
/// private cluster/region host) — matches the hardcoded URLs in `cos_discovery`.
const PLATFORM_URL: &str = "https://api.dataplatform.cloud.ibm.com";
/// SaaS only: the space's bundled COS storage type returned by the spaces GET.
const STORAGE_TYPE_BMCOS: &str = "bmcos_object_storage";

/// Resolve the winning leaderboard metric for `pipeline_name`. `best`/empty →
/// metrics[0] (the trainings API orders the leaderboard best-first); an explicit
/// `P<N>` matches the metric whose context.intermediate_model.name == that value.
fn resolve_winner<'a>(pipeline_name: &str, metrics: &'a [Value]) -> Result<&'a Value> {
    let want = pipeline_name.trim();
    if want.is_empty() || want.eq_ignore_ascii_case("best") {
        return metrics.first().ok_or_else(|| anyhow!("leaderboard is empty — cannot resolve `best` pipeline"));
    }
    metrics.iter().find(|m| m.pointer("/context/intermediate_model/name").and_then(|v| v.as_str()) == Some(want)).ok_or_else(|| anyhow!("pipeline_name `{want}` not found in the leaderboard"))
}

/// Build the `/v2/asset_files` RELATIVE path for a winning pipeline's request.json.
/// `schema_location` is `metrics[0].context.intermediate_model.schema_location`; the
/// assets root is everything before the LAST `/data/` segment. The absolute path is
/// `<root>/assets/<training_id>_<winner_name>_<phase>/resources/wml_model/request.json`
/// (winner_name is `P<N>`); the asset-files-relative form is everything after the
/// FIRST `/assets/` (matches the live-proven GET in troubleshoot §3).
fn request_json_rel(schema_location: &str, training_id: &str, winner_name: &str, phase: &str) -> Result<String> {
    let root = schema_location.rsplit_once("/data/").map(|(head, _)| head).ok_or_else(|| anyhow!("schema_location has no `/data/` segment: {schema_location}"))?;
    let abs = format!("{root}/assets/{training_id}_{winner_name}_{phase}/resources/wml_model/request.json");
    abs.split_once("/assets/").map(|(_, tail)| tail.to_string()).ok_or_else(|| anyhow!("request.json path has no `/assets/` segment: {abs}"))
}

/// GET the winning pipeline's request.json (the complete model-create body) via
/// `/v2/asset_files`. Returns the raw file JSON (NOT a CAMS envelope).
async fn fetch_request_json(client: &HttpClient, schema_location: &str, training_id: &str, winner_name: &str, phase: &str, space_id: Option<&str>, operation_id: &str) -> Result<Value> {
    let rel = request_json_rel(schema_location, training_id, winner_name, phase)?;
    let mut path = format!("{ASSET_FILES}/{rel}?version={VERSION}");
    if let Some(s) = space_id {
        path.push_str(&format!("&space_id={s}"));
    }
    let spec = RequestSpec::new(Method::GET, &path).body(BodyKind::None);
    client.execute(operation_id, spec).await.map_err(|e| anyhow!("[{operation_id}] wml_model: GET request.json ({rel}) failed: {e}"))
}

/// Poll GET /ml/v4/models/{id} until `entity.content_import_state == completed`.
/// Import was instant in live testing; bounded to ~3.3 min (40 attempts @ 5 s).
async fn wait_for_model_import(client: &HttpClient, model_id: &str, space_id: Option<&str>, operation_id: &str) -> Result<()> {
    let space_id = space_id.map(|s| s.to_string());
    let model_id = model_id.to_string();
    let operation_id = operation_id.to_string();
    crate::util::poll_until(40, Duration::from_secs(5), crate::util::PollTimeout::Bail(format!("[{operation_id}] timed out (3.3 min) waiting for wml_model {model_id} content import to complete")), None::<String>, move |attempt, mut prev| {
        let space_id = space_id.clone();
        let model_id = model_id.clone();
        let operation_id = operation_id.clone();
        async move {
            let mut path = format!("{MODELS}/{model_id}?version={VERSION}");
            if let Some(s) = &space_id {
                path.push_str(&format!("&space_id={s}"));
            }
            let resp: Value = client.execute(&operation_id, RequestSpec::new(Method::GET, &path).body(BodyKind::None)).await?;
            let state = resp.pointer("/entity/content_import_state").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if prev.as_deref() != Some(state.as_str()) {
                tracing::debug!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "wml_model", model_id = %model_id, content_import_state = %state, attempt = attempt, "wml_model content import state observed");
                prev = Some(state.clone());
            }
            let outcome = match state.as_str() {
                "completed" => crate::util::PollOutcome::Done(Value::Null),
                "failed" => crate::util::PollOutcome::Failed(format!("[{operation_id}] wml_model {model_id} content import failed")),
                _ => crate::util::PollOutcome::Pending,
            };
            Ok((outcome, prev))
        }
    })
    .await
    .map(|_| ())
}

/// SaaS only: resolve the space's BUNDLED COS connection (bucket + endpoint +
/// HMAC admin keys) via the platform-host spaces-credentials GET. A `container`
/// results reference carries no creds, so this is the only way to S3-GET the
/// per-pipeline request.json. Returns `(bucket_name, endpoint_url, access_key,
/// secret_key)`. (Q1 resolution — live-confirmed shape, Phase 1 findings.)
async fn resolve_space_cos_connection(client: &HttpClient, space_id: &str, operation_id: &str) -> Result<(String, String, String, String)> {
    let token = client.get_token().await.map_err(|e| anyhow!("[{operation_id}] wml_model: failed to mint IAM token for the spaces-credentials GET: {e}"))?;
    let url = format!("{PLATFORM_URL}/v2/spaces/{space_id}?include=everything,credentials");
    let resp = client.raw_client().get(&url).header("Authorization", format!("Bearer {token}")).send().await.map_err(|e| anyhow!("[{operation_id}] wml_model: spaces-credentials GET for space {space_id} failed: {e}"))?;
    let status = resp.status();
    let body: Value = resp.json().await.map_err(|e| anyhow!("[{operation_id}] wml_model: spaces-credentials GET for space {space_id} returned a non-JSON body (HTTP {status}): {e}"))?;
    if !status.is_success() {
        return Err(anyhow!("[{operation_id}] wml_model: spaces-credentials GET for space {space_id} returned HTTP {status}: {}", serde_json::to_string(&body).unwrap_or_default()));
    }
    let storage = body.pointer("/entity/storage").ok_or_else(|| anyhow!("[{operation_id}] wml_model: space {space_id} has no entity.storage — the bundled COS was not provisioned"))?;
    let storage_type = storage.pointer("/type").and_then(|v| v.as_str()).unwrap_or_default();
    if storage_type != STORAGE_TYPE_BMCOS {
        return Err(anyhow!("[{operation_id}] wml_model: space {space_id} storage.type is `{storage_type}`, expected `{STORAGE_TYPE_BMCOS}` (SaaS bundled COS)"));
    }
    let props = storage.pointer("/properties").ok_or_else(|| anyhow!("[{operation_id}] wml_model: space {space_id} storage has no properties"))?;
    let bucket = props.pointer("/bucket_name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] wml_model: space {space_id} storage.properties has no bucket_name"))?.to_string();
    let endpoint = props.pointer("/endpoint_url").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] wml_model: space {space_id} storage.properties has no endpoint_url"))?.to_string();
    let access_key = props
        .pointer("/credentials/admin/access_key_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("[{operation_id}] wml_model: space {space_id} has no storage.properties.credentials.admin.access_key_id — the apikey may lack include=credentials scope or the bundled COS HMAC creds are absent"))?
        .to_string();
    let secret_key = props.pointer("/credentials/admin/secret_access_key").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] wml_model: space {space_id} has no storage.properties.credentials.admin.secret_access_key"))?.to_string();
    Ok((bucket, endpoint, access_key, secret_key))
}

/// Extract the COS region from a bundled-COS endpoint for the SigV4 signing scope:
/// `https://s3.<region>.cloud-object-storage.appdomain.cloud` → `<region>`. IBM COS
/// requires the scope region to match the endpoint's region. Defaults to `us-south`
/// when the host doesn't match the expected shape.
fn region_from_endpoint(endpoint: &str) -> String {
    endpoint.strip_prefix("https://s3.").or_else(|| endpoint.strip_prefix("http://s3.")).and_then(|rest| rest.split('.').next()).filter(|r| !r.is_empty()).unwrap_or("us-south").to_string()
}

/// SaaS only: the CONTAINER-relative COS object key for a winning pipeline's
/// request.json (vs the `/v2/asset_files`-relative form the Software branch builds).
/// On SaaS `schema_location` (`metrics[0].context.intermediate_model.schema_location`)
/// is container-relative; the assets root is everything before the LAST `/data/`.
/// Key = `<root>/assets/<training_id>_<winner>_<phase>/resources/wml_model/request.json`
/// — live-pinned by the Phase 1 bucket listing.
fn request_json_container_key(schema_location: &str, training_id: &str, winner_name: &str, phase: &str) -> Result<String> {
    let root = schema_location.rsplit_once("/data/").map(|(head, _)| head).ok_or_else(|| anyhow!("schema_location has no `/data/` segment: {schema_location}"))?;
    Ok(format!("{root}/assets/{training_id}_{winner_name}_{phase}/resources/wml_model/request.json"))
}

/// SaaS only: S3-GET a request.json object from the space's bundled COS bucket
/// (SigV4 HMAC, path-style `/{bucket}/{key}`). `Ok(None)` on a 404 so the caller
/// can retry the fallback-phase key; any other non-2xx → a named error citing the
/// bucket + key + S3 code/message (distinct from the CP4D `/v2/asset_files` 404).
#[allow(clippy::too_many_arguments)]
async fn cos_get_request_json(client: &HttpClient, bucket: &str, endpoint: &str, access_key: &str, secret_key: &str, region: &str, key: &str, operation_id: &str) -> Result<Option<Value>> {
    let cos = CosClient::new(client.clone(), client.capacity(), CosAuth::Hmac { access_key: access_key.to_string(), secret_key: secret_key.to_string() }, None, Some(endpoint.to_string()));
    let path = format!("/{bucket}/{key}");
    let resp = cos.send(CosRequest { region, method: Method::GET, path: &path, ..Default::default() }, operation_id).await?;
    if resp.status.as_u16() == 404 {
        return Ok(None);
    }
    if !resp.status.is_success() {
        let err = parse_s3_error(&resp.body_str());
        return Err(anyhow!("[{operation_id}] wml_model: COS GET s3://{bucket}/{key} failed: HTTP {} {} — {} (wrong container-relative key derivation, or stale/invalid bundled-COS HMAC)", resp.status.as_u16(), err.code, err.message));
    }
    let parsed: Value = serde_json::from_slice(&resp.body).map_err(|e| anyhow!("[{operation_id}] wml_model: COS object s3://{bucket}/{key} is not valid JSON: {e}"))?;
    Ok(Some(parsed))
}

/// SaaS only: a `container` results reference carries no connection, but the
/// model POST needs `content_location.connection` populated. Normalize a legacy `s3`
/// content_location to `container` (defensive — current SaaS runs already emit
/// `container`), then inject the connection from the training's
/// `results_reference.connection`, falling back to the WHOLE `results_reference`
/// object (the bare-container case live-confirmed on SaaS — the empty `{}`
/// connection is replaced).
fn inject_container_connection(body: &mut Value, results_reference: &Value) {
    let connection = results_reference.get("connection").cloned().unwrap_or_else(|| results_reference.clone());
    if let Some(cl) = body.get_mut("content_location").and_then(|v| v.as_object_mut()) {
        if cl.get("type").and_then(|v| v.as_str()) == Some("s3") {
            cl.insert("type".to_string(), json!("container"));
        }
        cl.insert("connection".to_string(), connection);
    }
}

impl ResourceHandler for WmlModelHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], client: &'a HttpClient, _endpoint: &'a str, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            let name = resource.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] wml_model requires name"))?.to_string();
            let training_id = resource.get("experiment").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] wml_model requires experiment (autoai_experiment training id)"))?.to_string();
            let pipeline_name = resource.get("pipeline_name").and_then(|v| v.as_str()).unwrap_or("best").to_string();
            let space_id = resource.get("space_id").and_then(|v| v.as_str()).map(|s| s.to_string());
            let is_saas = client.deployment().flavor() == wxctl_core::types::Flavor::Saas;

            // 1. GET the completed training for its leaderboard + assets root.
            let mut get_path = format!("{TRAININGS}/{training_id}?version={VERSION}");
            if let Some(s) = &space_id {
                get_path.push_str(&format!("&space_id={s}"));
            }
            let training: Value = client.execute(operation_id, RequestSpec::new(Method::GET, &get_path).body(BodyKind::None)).await.map_err(|e| anyhow!("[{operation_id}] wml_model: failed to read experiment training {training_id}: {e}"))?;
            let empty: Vec<Value> = Vec::new();
            let metrics = training.pointer("/entity/status/metrics").and_then(|v| v.as_array()).unwrap_or(&empty);

            // 2. Resolve the winning pipeline + its phase + the assets root.
            let winner = resolve_winner(&pipeline_name, metrics).map_err(|e| anyhow!("[{operation_id}] wml_model: {e} (training {training_id})"))?;
            let winner_name = winner.pointer("/context/intermediate_model/name").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] wml_model: winning metric has no context.intermediate_model.name (training {training_id})"))?.to_string();
            let phase = winner.pointer("/context/phase").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let schema_location = metrics.first().and_then(|m| m.pointer("/context/intermediate_model/schema_location")).and_then(|v| v.as_str()).ok_or_else(|| anyhow!("[{operation_id}] wml_model: metrics[0] has no context.intermediate_model.schema_location (training {training_id})"))?.to_string();

            // 3. Fetch the server-authored request.json (the complete model-create body).
            //    Software (CP4D): GET /v2/asset_files (cluster fs). SaaS (IBM Cloud): S3-GET
            //    from the space's BUNDLED COS bucket — no /v2/asset_files on SaaS — resolving
            //    the bucket + HMAC via the platform-host spaces-credentials GET, then SigV4
            //    GET the container-relative key. Both fall back to the FALLBACK_PHASE dir if
            //    the primary object lacks content_location (current SaaS runs hit the primary
            //    key; Phase 1 confirmed content_location is present — the fallback is defensive).
            let mut body = if is_saas {
                let space = space_id.as_deref().ok_or_else(|| anyhow!("[{operation_id}] wml_model on SaaS requires space_id to resolve the bundled COS connection"))?;
                let (bucket, endpoint, access_key, secret_key) = resolve_space_cos_connection(client, space, operation_id).await?;
                let region = region_from_endpoint(&endpoint);
                let key = request_json_container_key(&schema_location, &training_id, &winner_name, &phase)?;
                let mut fetched = cos_get_request_json(client, &bucket, &endpoint, &access_key, &secret_key, &region, &key, operation_id).await?;
                if fetched.as_ref().map(|b| b.get("content_location").is_none()).unwrap_or(true) && phase != FALLBACK_PHASE {
                    let fb_key = request_json_container_key(&schema_location, &training_id, &winner_name, FALLBACK_PHASE)?;
                    if let Some(b) = cos_get_request_json(client, &bucket, &endpoint, &access_key, &secret_key, &region, &fb_key, operation_id).await? {
                        fetched = Some(b);
                    }
                }
                fetched.ok_or_else(|| anyhow!("[{operation_id}] wml_model: request.json not found in bundled COS bucket {bucket} for {winner_name} (training {training_id}) — recheck the container-relative key derived from schema_location"))?
            } else {
                let mut b = fetch_request_json(client, &schema_location, &training_id, &winner_name, &phase, space_id.as_deref(), operation_id).await?;
                if b.get("content_location").is_none() && phase != FALLBACK_PHASE {
                    b = fetch_request_json(client, &schema_location, &training_id, &winner_name, FALLBACK_PHASE, space_id.as_deref(), operation_id).await?;
                }
                b
            };
            if body.get("content_location").is_none() {
                return Err(anyhow!("[{operation_id}] wml_model: fetched request.json for {winner_name} has no content_location (training {training_id})"));
            }

            // 4. Patch only name + space_id (drop the on-disk run's project_id to avoid a scope clash).
            if let Some(obj) = body.as_object_mut() {
                obj.insert("name".to_string(), json!(name));
                if let Some(s) = &space_id {
                    obj.insert("space_id".to_string(), json!(s));
                    obj.remove("project_id");
                }
            }

            // 4b. SaaS only: a `container` results reference carries no connection, but the
            //     model POST needs content_location.connection populated. Inject it (and
            //     normalize a legacy `s3` content_location to `container`). The training's
            //     results_reference is the connection source (bare-container fallback = the
            //     whole results_reference, the live-confirmed SaaS case).
            if is_saas {
                let results_reference = training.pointer("/entity/results_reference").cloned().unwrap_or(Value::Null);
                inject_container_connection(&mut body, &results_reference);
            }

            // 5. POST /ml/v4/models — the handler owns the POST so the replayed
            //    by-reference body (content_location, schemas, software_spec, …) is
            //    sent verbatim (the materializer would strip its undeclared keys).
            let post_spec = RequestSpec::new(Method::POST, MODELS).query_param("version", VERSION).body(BodyKind::Json(body));
            let mut response: Value = client.execute(operation_id, post_spec).await.map_err(|e| anyhow!("[{operation_id}] wml_model: POST /ml/v4/models failed: {e}"))?;
            let model_id = response.pointer("/metadata/id").or_else(|| response.get("id")).and_then(|v| v.as_str()).map(|s| s.to_string()).ok_or_else(|| anyhow!("[{operation_id}] wml_model: no model id in POST response: {}", serde_json::to_string_pretty(&response).unwrap_or_default()))?;

            // 6. Poll content import to completed.
            wait_for_model_import(client, &model_id, space_id.as_deref(), operation_id).await?;

            // 7. Fold computed fields top-level so ${wml_model.x.id} resolves and the
            //    re-plan/display is clean (Handled responses are stored verbatim).
            if let Some(obj) = response.as_object_mut() {
                obj.insert("id".to_string(), json!(model_id));
                obj.insert("model_type".to_string(), json!(MODEL_TYPE));
                obj.insert("pipeline_node".to_string(), json!(winner_name));
            }
            tracing::info!(target: "wxctl::substage::provider", operation_id = %operation_id, resource_type = "wml_model", model_id = %model_id, winner = %winner_name, "wml_model published from AutoAI request.json replay");

            Ok(HookOutcome::Handled(response))
        })
    }

    fn post_discover<'a>(&'a self, remote_data: &'a mut Value, _client: &'a HttpClient, _operation_id: &'a str, _is_apply: bool) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        // Fold the model id top-level so ${wml_model.x.id} resolves on the discovery
        // (re-plan) path too — the discovered model carries its id at metadata.id.
        Box::pin(async move {
            if let Some(id) = remote_data.pointer("/metadata/id").and_then(|v| v.as_str()).map(|s| s.to_string())
                && let Some(obj) = remote_data.as_object_mut()
            {
                obj.insert("id".to_string(), json!(id));
            }
            Ok(())
        })
    }

    fn recover_from_create_error<'a>(&'a self, _resource: &'a Value, _error: &'a anyhow::Error, _client: &'a HttpClient, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<Option<Value>>> + Send + 'a>> {
        // A fresh model POST mints a new id; there is nothing to adopt on error.
        // Idempotent re-apply is handled by name-based discovery (list_and_get).
        Box::pin(async move { Ok(None) })
    }
}
