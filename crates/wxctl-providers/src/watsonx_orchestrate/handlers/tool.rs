use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;
use tracing::Instrument;
use wxctl_core::client::{HttpClient, error_matches};
use wxctl_core::registry::FieldDescriptor;
use wxctl_core::traits::{HookOutcome, ResourceHandler};

use super::super::flow::load_flow_model;
use super::super::python::{ArtifactBuilder, ToolSchemas, load_schemas, parse_requirements_file};
use crate::util::{extract_artifact_path, set_source_hash_tag, validate_path};

pub struct ToolHandler;

enum BindingType {
    Python,
    Flow,
    OpenApi,
}

fn get_binding_type(resource: &Value) -> Option<BindingType> {
    if resource.pointer("/binding/python").is_some() {
        Some(BindingType::Python)
    } else if resource.pointer("/binding/flow").is_some() {
        Some(BindingType::Flow)
    } else if resource.pointer("/binding/openapi").is_some() {
        Some(BindingType::OpenApi)
    } else {
        None
    }
}

impl ResourceHandler for ToolHandler {
    fn pre_create<'a>(&'a self, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            match get_binding_type(resource) {
                Some(BindingType::Flow) => self.pre_create_flow(resource).await,
                Some(BindingType::Python) => self.pre_create_python(resource).await,
                Some(BindingType::OpenApi) => self.pre_create_openapi(resource).await,
                None => Ok(HookOutcome::Continue),
            }
        })
    }

    fn pre_update<'a>(&'a self, current: &'a Value, resource: &'a mut Value, _fields: &'a [FieldDescriptor], _client: &'a HttpClient, _endpoint: &'a str, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<HookOutcome>> + Send + 'a>> {
        Box::pin(async move {
            match get_binding_type(resource) {
                Some(BindingType::Flow) => self.pre_update_flow(current, resource).await,
                Some(BindingType::Python) => self.pre_update_python(current, resource).await,
                Some(BindingType::OpenApi) => self.pre_update_openapi(current, resource).await,
                None => Ok(HookOutcome::Continue),
            }
        })
    }

    fn post_create<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(artifact_path) = extract_artifact_path(resource) {
                let tool_id = response.get("id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No tool ID in response"))?;

                upload_and_cleanup_artifact(client, tool_id, &artifact_path, operation_id).await?;
            }

            Ok(())
        })
    }

    fn post_update<'a>(&'a self, resource: &'a Value, response: &'a mut Value, client: &'a HttpClient, operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            if let Some(artifact_path) = extract_artifact_path(resource) {
                let tool_id = response.get("id").or_else(|| resource.get("id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("No tool ID in response or resource"))?;

                upload_and_cleanup_artifact(client, tool_id, &artifact_path, operation_id).await?;
            }

            Ok(())
        })
    }

    fn post_validate<'a>(&'a self, resource: &'a mut Value, _operation_id: &'a str) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move {
            match get_binding_type(resource) {
                Some(BindingType::Flow) => {
                    // Flow tools are always async on the server - override any default
                    resource["is_async"] = json!(true);

                    if let Some(source_path_str) = resource.get("source_path").and_then(|v| v.as_str()) {
                        let source_path = PathBuf::from(source_path_str);
                        if !source_path.exists() {
                            bail!("Flow source path '{}' does not exist", source_path.display());
                        }
                        let source_path = validate_path(&source_path)?;
                        // Propagate parse errors — a malformed flow JSON must fail validate,
                        // not silently report green.
                        let flow_model = load_flow_model(&source_path)?;
                        if resource.get("description").is_none() || resource["description"].as_str().map(|s| s.is_empty()).unwrap_or(true) {
                            resource["description"] = json!(flow_model.description);
                        }
                    }
                }
                Some(BindingType::OpenApi) => {
                    let spec_path_str = match resource.get("spec_path").and_then(|v| v.as_str()) {
                        Some(path) => path,
                        None => return Ok(()),
                    };
                    let builder = super::super::openapi::OpenApiArtifactBuilder::new(std::path::PathBuf::from(spec_path_str))?;
                    let source_hash = builder.compute_source_hash()?;
                    set_source_hash_tag(resource, &source_hash);
                }
                Some(BindingType::Python) => {
                    // Sanitize connection app-id keys before reconciliation so the desired
                    // state matches what the platform stores (and what it wires credential
                    // injection from) — see sanitize_connection_app_ids.
                    sanitize_connection_app_ids(resource);

                    // Only process if source_path exists (tool resource)
                    let source_path_str = match resource.get("source_path").and_then(|v| v.as_str()) {
                        Some(path) => path,
                        None => return Ok(()), // Not a tool with source_path, skip
                    };

                    let source_path = PathBuf::from(source_path_str);

                    if !source_path.exists() {
                        bail!("Source path '{}' does not exist", source_path.display());
                    }

                    // Validate path to prevent traversal attacks
                    let source_path = validate_path(&source_path)?;

                    // Load schemas from schema.yaml and inject for reconciliation comparison
                    let schemas = load_schemas(&source_path)?;
                    resource["input_schema"] = schemas.input_schema;
                    resource["output_schema"] = schemas.output_schema;

                    // Strip agent_run_parameter from the binding for reconciliation — the API
                    // does not return it in GET, so keeping it would cause a perpetual diff.
                    // Save the value in a non-schema internal field so pre_create/pre_update
                    // can still inject the context schema and API binding field.
                    if let Some(param_name) = resource.pointer("/binding/python/agent_run_parameter").and_then(|v| v.as_str()).map(|s| s.to_string()) {
                        resource["_agent_run_parameter"] = json!(param_name);

                        if let Some(python_obj) = resource.pointer_mut("/binding/python").and_then(|v| v.as_object_mut()) {
                            python_obj.remove("agent_run_parameter");
                        }
                    }

                    let builder = ArtifactBuilder::new(source_path)?;
                    let source_hash = builder.compute_source_hash()?;
                    set_source_hash_tag(resource, &source_hash);
                }
                None => {}
            }

            Ok(())
        })
    }
}

// Flow binding specific implementations
impl ToolHandler {
    async fn pre_create_flow<'a>(&'a self, resource: &'a mut Value) -> Result<HookOutcome> {
        // Flow source file: source_path (primary) or flow_path (additive alias).
        // Both are resolved against the config dir by resolve_file_paths. If neither
        // is set, passthrough (the model may be inlined under binding.flow.model).
        let source_path_str = match resource.get("source_path").and_then(|v| v.as_str()).or_else(|| resource.get("flow_path").and_then(|v| v.as_str())) {
            Some(path) => path.to_string(),
            None => return Ok(HookOutcome::Continue),
        };

        let source_path = PathBuf::from(&source_path_str);

        // Validate source_path exists
        if !source_path.exists() {
            bail!("Flow source path '{}' does not exist", source_path.display());
        }

        // Validate path to prevent traversal attacks
        let source_path = validate_path(&source_path)?;

        // Load flow model from file
        let flow_model = load_flow_model(&source_path)?;

        // Inject flow_id and model into binding
        resource["binding"]["flow"]["flow_id"] = json!(flow_model.name);
        resource["binding"]["flow"]["model"] = flow_model.model;

        // Pin the flow's auto-data-mapping LLM, if the tool declares one. `flow_llm_model`
        // is a LocalOnly field resolved from `${model.<ref>}` to the model name; injecting it
        // into the flow model's `metadata.llm_model` makes the flow runtime use that model for
        // the script/decisions nodes instead of the instance `DEFAULT_FLOW_LLM_MODEL` (which a
        // CP4D AI-Gateway instance lacks). Existing metadata keys are preserved.
        inject_flow_llm_model(resource);

        // Inject description from flow model if not already set
        if resource.get("description").is_none() || resource["description"].as_str().map(|s| s.is_empty()).unwrap_or(true) {
            resource["description"] = json!(flow_model.description);
        }

        // Inject schemas if present in the flow model
        if let Some(input_schema) = flow_model.input_schema {
            resource["input_schema"] = input_schema;
        }
        if let Some(output_schema) = flow_model.output_schema {
            resource["output_schema"] = output_schema;
        }

        Ok(HookOutcome::Continue)
    }

    async fn pre_update_flow<'a>(&'a self, _current: &'a Value, resource: &'a mut Value) -> Result<HookOutcome> {
        // For flow bindings, update is the same as create - reload from source each time
        // No artifact upload means no hash-based optimization needed
        self.pre_create_flow(resource).await
    }
}

/// Inject the resolved `flow_llm_model` into the registered flow's `metadata.llm_model`.
///
/// No-op when the tool doesn't declare `flow_llm_model`, so the flow runtime falls back to
/// the instance `DEFAULT_FLOW_LLM_MODEL`. Requires `binding.flow.model` to already be set
/// (call after the flow model is loaded). Existing `metadata` keys are preserved.
fn inject_flow_llm_model(resource: &mut Value) {
    let Some(llm_model) = resource.get("flow_llm_model").and_then(|v| v.as_str()).filter(|s| !s.is_empty()).map(|s| s.to_string()) else {
        return;
    };
    let Some(model) = resource.pointer_mut("/binding/flow/model") else {
        return;
    };
    if !model.get("metadata").map(Value::is_object).unwrap_or(false) {
        model["metadata"] = json!({});
    }
    model["metadata"]["llm_model"] = json!(llm_model);
}

// Python binding specific implementations
impl ToolHandler {
    async fn pre_create_python<'a>(&'a self, resource: &'a mut Value) -> Result<HookOutcome> {
        let (source_path, schemas, requirements) = validate_python_source(resource)?;

        // Build ZIP artifact and compute hash (path already validated by validate_python_source)
        let builder = ArtifactBuilder::new(source_path)?;
        let (artifact_path, source_hash) = builder.build()?;

        // Inject fields
        resource["input_schema"] = schemas.input_schema;
        resource["output_schema"] = schemas.output_schema;
        resource["binding"]["python"]["requirements"] = serde_json::to_value(requirements)?;
        sanitize_connection_app_ids(resource);
        inject_agent_run_schema(resource);
        translate_agent_run_parameter(resource);
        resource["artifact"] = json!({
            "path": artifact_path.to_string_lossy().to_string()
        });

        set_source_hash_tag(resource, &source_hash);

        Ok(HookOutcome::Continue)
    }

    async fn pre_update_python<'a>(&'a self, current: &'a Value, resource: &'a mut Value) -> Result<HookOutcome> {
        let (source_path, schemas, requirements) = validate_python_source(resource)?;

        resource["input_schema"] = schemas.input_schema;
        resource["output_schema"] = schemas.output_schema;
        resource["binding"]["python"]["requirements"] = serde_json::to_value(requirements)?;
        sanitize_connection_app_ids(resource);
        inject_agent_run_schema(resource);
        translate_agent_run_parameter(resource);

        crate::util::reconcile_artifact_by_hash(current, resource, || async move {
            let builder = ArtifactBuilder::new(source_path)?;
            builder.build()
        })
        .await?;

        Ok(HookOutcome::Continue)
    }
}

// OpenAPI binding specific implementations
impl ToolHandler {
    async fn pre_create_openapi<'a>(&'a self, resource: &'a mut Value) -> Result<HookOutcome> {
        let spec_path_str = resource.get("spec_path").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("spec_path is required for OpenAPI binding"))?;

        let spec_path = std::path::PathBuf::from(spec_path_str);
        let spec_path = crate::util::validate_path(&spec_path)?;

        let builder = super::super::openapi::OpenApiArtifactBuilder::new(spec_path)?;
        let (artifact_path, source_hash) = builder.build()?;

        resource["artifact"] = json!({"path": artifact_path.to_string_lossy().to_string()});
        set_source_hash_tag(resource, &source_hash);

        Ok(HookOutcome::Continue)
    }

    async fn pre_update_openapi<'a>(&'a self, current: &'a Value, resource: &'a mut Value) -> Result<HookOutcome> {
        // Capture spec_path as Option before the &mut borrow; the original reads it
        // ONLY inside the build branch, so a hash-unchanged update with a missing
        // spec_path must NOT error — defer the error into the closure.
        let spec_path_str = resource.get("spec_path").and_then(|v| v.as_str()).map(str::to_string);

        crate::util::reconcile_artifact_by_hash(current, resource, || async move {
            let spec_path_str = spec_path_str.ok_or_else(|| anyhow!("spec_path is required for OpenAPI binding"))?;
            let builder = super::super::openapi::OpenApiArtifactBuilder::new(std::path::PathBuf::from(&spec_path_str))?;
            builder.build()
        })
        .await?;

        Ok(HookOutcome::Continue)
    }
}

/// When `agent_run_parameter` is set, inject the named parameter into `input_schema`
/// with the AgentRun object schema. This matches what the ADK CLI does via Python AST
/// introspection — the cloud runtime expects this property in the schema to know
/// which function parameter receives the injected context.
fn inject_agent_run_schema(resource: &mut Value) {
    // Check both the original config field and the internal field set by post_validate
    let param_name = resource.pointer("/binding/python/agent_run_parameter").or_else(|| resource.get("_agent_run_parameter")).and_then(|v| v.as_str()).map(|s| s.to_string());

    let param_name = match param_name {
        Some(name) => name,
        None => return,
    };

    // Already injected — no-op (idempotency guard)
    if resource.pointer(&format!("/input_schema/properties/{}", param_name)).is_some() {
        return;
    }

    // Ensure input_schema.properties exists
    if resource.get("input_schema").is_none() {
        resource["input_schema"] = json!({"type": "object", "properties": {}});
    }
    if resource["input_schema"].get("properties").is_none() {
        resource["input_schema"]["properties"] = json!({});
    }

    // Inject AgentRun property — must match the full Pydantic-generated schema
    // that the ADK CLI sends, including dynamic_input_schema and
    // dynamic_output_schema with the complete JsonSchemaObject definition.
    // The runtime validates this schema structure before injecting context.
    let json_schema_object = json!({
        "type": "object",
        "title": "JsonSchemaObject",
        "properties": {
            "type": {
                "title": "Type",
                "default": null,
                "anyOf": [
                    {"type": "string", "enum": ["object","string","number","integer","boolean","array","null"]},
                    {"type": "array", "items": {"type": "string", "enum": ["object","string","number","integer","boolean","array","null"]}},
                    {"type": "null"}
                ]
            },
            "title": {
                "title": "Title",
                "default": null,
                "anyOf": [{"type": "string"}, {"type": "null"}]
            },
            "description": {
                "title": "Description",
                "default": null,
                "anyOf": [{"type": "string"}, {"type": "null"}]
            },
            "properties": {
                "title": "Properties",
                "default": null,
                "anyOf": [{"type": "object", "additionalProperties": {}}, {"type": "null"}]
            },
            "required": {
                "title": "Required",
                "default": null,
                "anyOf": [{"type": "array", "items": {"type": "string"}}, {"type": "null"}]
            },
            "items": {
                "default": null,
                "anyOf": [{}, {"type": "null"}]
            },
            "uniqueItems": {
                "title": "Uniqueitems",
                "default": null,
                "anyOf": [{"type": "boolean"}, {"type": "null"}]
            },
            "default": {
                "title": "Default",
                "default": null,
                "anyOf": [{}, {"type": "null"}]
            },
            "enum": {
                "title": "Enum",
                "default": null,
                "anyOf": [{"type": "array", "items": {}}, {"type": "null"}]
            },
            "minimum": {
                "title": "Minimum",
                "default": null,
                "anyOf": [{"type": "number"}, {"type": "null"}]
            },
            "maximum": {
                "title": "Maximum",
                "default": null,
                "anyOf": [{"type": "number"}, {"type": "null"}]
            },
            "minLength": {
                "title": "Minlength",
                "default": null,
                "anyOf": [{"type": "integer"}, {"type": "null"}]
            },
            "maxLength": {
                "title": "Maxlength",
                "default": null,
                "anyOf": [{"type": "integer"}, {"type": "null"}]
            },
            "format": {
                "title": "Format",
                "default": null,
                "anyOf": [{"type": "string"}, {"type": "null"}]
            },
            "pattern": {
                "title": "Pattern",
                "default": null,
                "anyOf": [{"type": "string"}, {"type": "null"}]
            },
            "anyOf": {
                "title": "Anyof",
                "default": null,
                "anyOf": [{"type": "array", "items": {}}, {"type": "null"}]
            },
            "in": {
                "title": "In",
                "default": null,
                "anyOf": [{"type": "string", "enum": ["query","header","path","body"]}, {"type": "null"}]
            },
            "aliasName": {
                "title": "Aliasname",
                "default": null,
                "anyOf": [{"type": "string"}, {"type": "null"}]
            },
            "wrap_data": {
                "title": "Wrap Data",
                "default": true,
                "anyOf": [{"type": "boolean"}, {"type": "null"}]
            }
        },
        "required": [],
        "additionalProperties": true
    });

    resource["input_schema"]["properties"][&param_name] = json!({
        "type": "object",
        "title": "AgentRun",
        "description": "The agent run context containing request metadata.",
        "properties": {
            "request_context": {
                "title": "Request Context",
                "default": null,
                "anyOf": [
                    {"type": "object", "additionalProperties": true},
                    {"type": "null"}
                ]
            },
            "dynamic_input_schema": {
                "title": "Dynamic Input Schema",
                "default": null,
                "anyOf": [
                    json_schema_object.clone(),
                    {"type": "object", "additionalProperties": true},
                    {"type": "null"}
                ]
            },
            "dynamic_output_schema": {
                "title": "Dynamic Output Schema",
                "default": null,
                "anyOf": [
                    json_schema_object,
                    {"type": "object", "additionalProperties": true},
                    {"type": "null"}
                ]
            }
        },
        "required": []
    });

    // Add to required array
    let mut required: Vec<Value> = resource["input_schema"].get("required").and_then(|v| v.as_array()).cloned().unwrap_or_default();

    if !required.iter().any(|v| v.as_str() == Some(&param_name)) {
        required.push(json!(param_name));
        resource["input_schema"]["required"] = json!(required);
    }
}

/// Sanitize the app-id KEYS of `binding.python.connections` the way the wxO ADK does
/// (`sanitize_app_id`: every non-alphanumeric run → one `_`).
///
/// The platform wires runtime credential injection off these keys — the deployed tool's
/// Code Engine runtime receives `WXO_SECURITY_SCHEMA_<key>` / `WXO_CONNECTION[_CUSTOM]_<key>_*`
/// env vars named after them, and the ADK runtime (`connections.key_value(app_id)`) looks them
/// up with the app id SANITIZED. A raw key like `churn-scoring` therefore produces env names
/// the runtime can never match (`-` is not even legal in an env var), so the tool gets no
/// credentials at all ("No credentials found for connections '<app>'"). The ADK CLI sanitizes
/// the keys at import time (`__parse_app_ids`); mirror it here so wxctl-deployed tools receive
/// their connections. Values (connection ids) are untouched.
fn sanitize_connection_app_ids(resource: &mut Value) {
    let Some(conns) = resource.pointer_mut("/binding/python/connections").and_then(|v| v.as_object_mut()) else {
        return;
    };
    let keys: Vec<String> = conns.keys().cloned().collect();
    for key in keys {
        let sanitized = sanitize_app_id(&key);
        if sanitized != key
            && let Some(v) = conns.remove(&key)
        {
            conns.insert(sanitized, v);
        }
    }
}

/// ADK `sanitize_app_id`: `re.sub(r"[^a-zA-Z0-9]+", '_', app_id)` — each run of
/// non-alphanumeric characters collapses to a single underscore.
fn sanitize_app_id(app_id: &str) -> String {
    let mut out = String::with_capacity(app_id.len());
    let mut in_run = false;
    for c in app_id.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('_');
            in_run = true;
        }
    }
    out
}

/// Translate user-facing `agent_run_parameter` (correct spelling) to the
/// API field `agent_run_paramater` (misspelled to match WXO API).
fn translate_agent_run_parameter(resource: &mut Value) {
    // Check both the original config field and the internal field set by post_validate
    let param_value = resource.pointer("/binding/python/agent_run_parameter").or_else(|| resource.get("_agent_run_parameter")).cloned();

    if let Some(param_value) = param_value {
        if let Some(python_obj) = resource.pointer_mut("/binding/python").and_then(|v| v.as_object_mut()) {
            python_obj.remove("agent_run_parameter");
            python_obj.insert("agent_run_paramater".to_string(), param_value);
        }
        // Clean up internal field
        if let Some(obj) = resource.as_object_mut() {
            obj.remove("_agent_run_parameter");
        }
    }
}

/// Upload artifact and clean up temp directory, wrapped in a tracing span
fn upload_and_cleanup_artifact<'a>(client: &'a HttpClient, tool_id: &'a str, artifact_path: &'a str, operation_id: &'a str) -> impl Future<Output = Result<()>> + Send + 'a {
    let span = tracing::debug_span!(
        target: "wxctl::substage::provider",
        "upload_tool_artifact",
        operation_id = %operation_id,
        tool_id = %tool_id,
        artifact_path = %artifact_path
    );

    async move { crate::util::upload_artifact_and_cleanup(artifact_path, || async { upload_tool_artifact(client, tool_id, artifact_path, operation_id).await.context("Failed to upload tool artifact") }).await }.instrument(span)
}

/// Validate Python source: extract source_path, check existence, validate path,
/// validate function ref, load schemas, and parse requirements
fn validate_python_source(resource: &Value) -> Result<(PathBuf, ToolSchemas, Vec<String>)> {
    let source_path_str = resource["source_path"].as_str().ok_or_else(|| anyhow!("source_path is required for Python binding"))?;

    let source_path = PathBuf::from(source_path_str);

    if !source_path.exists() {
        bail!("Source path '{}' does not exist", source_path.display());
    }

    let source_path = validate_path(&source_path)?;

    let _function_ref = resource.pointer("/binding/python/function").and_then(|v| v.as_str()).ok_or_else(|| anyhow!("binding.python.function is required"))?;

    let schemas = load_schemas(&source_path)?;
    let requirements = parse_requirements_file(&source_path)?;

    Ok((source_path, schemas, requirements))
}

/// Upload tool artifact ZIP file.
///
/// On first apply against a fresh Orchestrate instance, TRM's executor reconciler
/// races on the shared per-instance K8s Service `executor-deployment-<guid>-svc`.
/// The second racer surfaces as `HTTP 500 ... services "executor-deployment-...-svc"
/// already exists`. Once any tool's Service create wins, subsequent uploads succeed —
/// and the upload endpoint is idempotent on `tool_id`, so a bounded retry recovers
/// without re-running apply. Other 5xxs propagate immediately (not retryable on POST
/// per `wxctl-core::client::retry`).
async fn upload_tool_artifact<'a>(client: &'a HttpClient, tool_id: &'a str, artifact_path: &'a str, operation_id: &'a str) -> Result<()> {
    const MAX_RACE_ATTEMPTS: u32 = 4;

    let endpoint = format!("/v1/orchestrate/tools/{}/upload", tool_id);
    let path = Path::new(artifact_path);

    if !path.exists() {
        return Err(anyhow::anyhow!("Tool artifact file not found: {}", artifact_path));
    }

    for attempt in 0..MAX_RACE_ATTEMPTS {
        match client.upload_file(operation_id, &endpoint, path, "file").await {
            Ok(_) => return Ok(()),
            Err(e) if attempt + 1 < MAX_RACE_ATTEMPTS && is_trm_executor_svc_race(&e) => {
                let delay = Duration::from_millis(1000u64 << attempt);
                tracing::warn!(
                    target: "wxctl::substage::provider",
                    operation_id = %operation_id,
                    tool_id = %tool_id,
                    attempt = attempt + 1,
                    delay_ms = delay.as_millis() as u64,
                    "TRM executor-service race on artifact upload; retrying"
                );
                tokio::time::sleep(delay).await;
            }
            Err(e) => return Err(e).context("Failed to upload tool artifact"),
        }
    }

    unreachable!("loop body always returns on the last attempt")
}

/// Matches TRM's `executor-deployment-<guid>-svc already exists` race condition.
fn is_trm_executor_svc_race(err: &anyhow::Error) -> bool {
    error_matches(err, 500, &["executor-deployment-", "-svc", "already exists"])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_inject_agent_run_schema() {
        let mut resource = json!({
            "binding": {
                "python": {
                    "function": "my_module:my_func",
                    "agent_run_parameter": "context"
                }
            },
            "input_schema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                }
            }
        });

        inject_agent_run_schema(&mut resource);

        // context property should be injected
        let context_prop = resource.pointer("/input_schema/properties/context").unwrap();
        assert_eq!(context_prop["title"], "AgentRun");
        assert_eq!(context_prop["type"], "object");

        // Must include all three AgentRun properties (matching ADK Pydantic schema)
        let props = context_prop["properties"].as_object().unwrap();
        assert!(props.contains_key("request_context"));
        assert!(props.contains_key("dynamic_input_schema"));
        assert!(props.contains_key("dynamic_output_schema"));

        // dynamic_input_schema should reference JsonSchemaObject
        let dyn_input = &props["dynamic_input_schema"];
        let any_of = dyn_input["anyOf"].as_array().unwrap();
        assert!(any_of.iter().any(|v| v["title"] == "JsonSchemaObject"));

        // message property should still be there
        assert!(resource.pointer("/input_schema/properties/message").is_some());

        // context should be in required
        let required = resource["input_schema"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("context")));
    }

    #[test]
    fn test_inject_agent_run_schema_absent() {
        let mut resource = json!({
            "binding": {
                "python": {
                    "function": "my_module:my_func"
                }
            },
            "input_schema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                }
            }
        });

        let original = resource.clone();
        inject_agent_run_schema(&mut resource);

        // No agent_run_parameter — resource unchanged
        assert_eq!(resource, original);
    }

    #[tokio::test]
    async fn pre_create_flow_reads_flow_path_alias_when_source_path_absent() {
        use std::io::Write;
        // validate_path requires the file under CWD (the crate dir during `cargo test`),
        // so write the temp flow there rather than the system temp dir.
        let mut tmp = tempfile::Builder::new().prefix("wxctl_flow_path_test_").suffix(".json").tempfile_in(".").unwrap();
        write!(tmp, r#"{{"spec":{{"name":"wxo_greeting_flow","description":"greets a person"}},"nodes":{{}},"edges":[]}}"#).unwrap();
        tmp.flush().unwrap();

        // Only flow_path is set (no source_path) — the handler must fall back to it.
        let mut resource = json!({
            "name": "greeting_flow_tool",
            "permission": "read_only",
            "flow_path": tmp.path().to_str().unwrap(),
            "binding": { "flow": { "version": "TIP" } }
        });

        let outcome = ToolHandler.pre_create_flow(&mut resource).await.unwrap();
        assert!(matches!(outcome, HookOutcome::Continue));
        // flow_id + model injected from the file referenced by flow_path.
        assert_eq!(resource.pointer("/binding/flow/flow_id").unwrap(), "wxo_greeting_flow");
        assert_eq!(resource.pointer("/binding/flow/model/spec/name").unwrap(), "wxo_greeting_flow");
        // description backfilled from the flow model.
        assert_eq!(resource.get("description").unwrap(), "greets a person");
    }

    #[test]
    fn inject_flow_llm_model_cases() {
        // Sets metadata.llm_model and preserves existing metadata keys.
        let mut preserves = json!({"flow_llm_model": "virtual-model/watsonx/openai/gpt-oss-120b", "binding": {"flow": {"model": {"spec": {"name": "f"}, "metadata": {"source_kind": "adk/python"}}}}});
        inject_flow_llm_model(&mut preserves);
        assert_eq!(preserves.pointer("/binding/flow/model/metadata/llm_model").unwrap(), "virtual-model/watsonx/openai/gpt-oss-120b", "sets llm_model");
        assert_eq!(preserves.pointer("/binding/flow/model/metadata/source_kind").unwrap(), "adk/python", "existing metadata keys preserved");

        // Creates the metadata object when the flow model lacks one.
        let mut creates = json!({"flow_llm_model": "virtual-model/watsonx/openai/gpt-oss-120b", "binding": {"flow": {"model": {"spec": {"name": "f"}}}}});
        inject_flow_llm_model(&mut creates);
        assert_eq!(creates.pointer("/binding/flow/model/metadata/llm_model").unwrap(), "virtual-model/watsonx/openai/gpt-oss-120b", "creates metadata when absent");

        // Absent flow_llm_model: untouched (falls back to instance DEFAULT_FLOW_LLM_MODEL).
        let mut absent = json!({"binding": {"flow": {"model": {"metadata": {"source_kind": "adk/python"}}}}});
        let before = absent.clone();
        inject_flow_llm_model(&mut absent);
        assert_eq!(absent, before, "absent flow_llm_model → no-op");

        // Empty string is treated as unset.
        let mut empty = json!({"flow_llm_model": "", "binding": {"flow": {"model": {"metadata": {}}}}});
        inject_flow_llm_model(&mut empty);
        assert!(empty.pointer("/binding/flow/model/metadata/llm_model").is_none(), "empty string → unset → no-op");
    }

    #[test]
    fn test_translate_agent_run_parameter_present() {
        let mut resource = json!({
            "binding": {
                "python": {
                    "function": "my_module:my_func",
                    "agent_run_parameter": "context"
                }
            }
        });

        translate_agent_run_parameter(&mut resource);

        // The correctly-spelled key should be removed
        assert!(resource.pointer("/binding/python/agent_run_parameter").is_none());
        // The misspelled API key should be present with the original value
        assert_eq!(resource.pointer("/binding/python/agent_run_paramater").unwrap(), "context");
    }

    #[test]
    fn test_inject_via_internal_field() {
        // Simulates the post_validate → pre_create flow:
        // post_validate strips agent_run_parameter and saves to _agent_run_parameter
        let mut resource = json!({
            "_agent_run_parameter": "context",
            "binding": {
                "python": {
                    "function": "my_module:my_func"
                }
            },
            "input_schema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                }
            }
        });

        inject_agent_run_schema(&mut resource);

        // context property should be injected via _agent_run_parameter
        let context_prop = resource.pointer("/input_schema/properties/context").unwrap();
        assert_eq!(context_prop["title"], "AgentRun");

        translate_agent_run_parameter(&mut resource);

        // agent_run_paramater (misspelled) should be set on the binding
        assert_eq!(resource.pointer("/binding/python/agent_run_paramater").unwrap(), "context");
        // Internal field should be cleaned up
        assert!(resource.get("_agent_run_parameter").is_none());
    }

    #[test]
    fn test_translate_agent_run_parameter_noop_cases() {
        // No-op when there's no agent_run_parameter to translate: python binding without
        // the field, and a non-python (flow) binding.
        for resource in [json!({"binding": {"python": {"function": "my_module:my_func"}}}), json!({"binding": {"flow": {"flow_id": "some_flow"}}})] {
            let mut r = resource.clone();
            translate_agent_run_parameter(&mut r);
            assert_eq!(r, resource, "resource unchanged");
        }
    }

    #[test]
    fn test_inject_agent_run_schema_preserves_existing_required() {
        let mut resource = json!({
            "binding": {
                "python": {
                    "function": "my_module:my_func",
                    "agent_run_parameter": "context"
                }
            },
            "input_schema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                },
                "required": ["message"]
            }
        });

        inject_agent_run_schema(&mut resource);

        let required = resource["input_schema"]["required"].as_array().unwrap();
        assert!(required.iter().any(|v| v.as_str() == Some("message")), "existing required entry preserved");
        assert!(required.iter().any(|v| v.as_str() == Some("context")), "context added to required");
        assert_eq!(required.len(), 2);
    }

    #[test]
    fn test_inject_agent_run_schema_idempotent() {
        let mut resource = json!({
            "_agent_run_parameter": "context",
            "binding": {
                "python": {
                    "function": "my_module:my_func"
                }
            },
            "input_schema": {
                "type": "object",
                "properties": {
                    "message": { "type": "string" }
                }
            }
        });

        inject_agent_run_schema(&mut resource);
        let after_first = resource.clone();

        inject_agent_run_schema(&mut resource);
        assert_eq!(resource, after_first, "second call should be a no-op");
    }

    #[test]
    fn sanitize_connection_app_ids_cases() {
        // Hyphenated keys are sanitized to the ADK form; values (connection ids) untouched.
        let mut resource = json!({
            "binding": {"python": {
                "function": "score_churn:main",
                "connections": {"churn-scoring": "506c2b4b-85d2-4594-96aa-1971e5c8f470"}
            }}
        });
        sanitize_connection_app_ids(&mut resource);
        assert_eq!(resource.pointer("/binding/python/connections/churn_scoring").unwrap(), "506c2b4b-85d2-4594-96aa-1971e5c8f470");
        assert!(resource.pointer("/binding/python/connections/churn-scoring").is_none());

        // Already-clean keys and non-python bindings are untouched.
        for resource in [json!({"binding": {"python": {"function": "f:main", "connections": {"clean_key": "id-1"}}}}), json!({"binding": {"flow": {"flow_id": "some_flow"}}}), json!({"binding": {"python": {"function": "f:main"}}})] {
            let mut r = resource.clone();
            sanitize_connection_app_ids(&mut r);
            assert_eq!(r, resource, "resource unchanged");
        }
    }

    #[test]
    fn sanitize_app_id_matches_adk() {
        // Mirrors ADK sanitize_app_id: re.sub(r"[^a-zA-Z0-9]+", '_', app_id).
        for (raw, expected) in [("churn-scoring", "churn_scoring"), ("churn-lakehouse", "churn_lakehouse"), ("a--b..c", "a_b_c"), ("already_clean0", "already_clean0"), ("-lead-trail-", "_lead_trail_")] {
            assert_eq!(sanitize_app_id(raw), expected, "{raw}");
        }
    }

    #[test]
    fn is_trm_executor_svc_race_cases() {
        // Only a 500 carrying all three markers (executor-deployment-/-svc/already exists) is the race.
        let cases: &[(&str, bool, &str)] = &[
            (
                r#"WXCTL-H001 HTTP 500 POST: HTTP 500 Internal Server Error - {"detail":"Tool deployment failed in TRM:{\"error\":\"error in deployment: failed to create executor: failed to create Service: services \\\"executor-deployment-13a84bf1-1b50-45aa-9aa8-6e302dc091d5-svc\\\" already exists\"} "}"#,
                true,
                "real TRM race error",
            ),
            ("WXCTL-H001 HTTP 500 POST: internal error", false, "other 500 — no markers"),
            (r#"WXCTL-H001 HTTP 409 POST: services "executor-deployment-abc-svc" already exists"#, false, "markers present but non-500 status"),
        ];
        for (msg, expected, why) in cases {
            assert_eq!(is_trm_executor_svc_race(&anyhow!("{msg}")), *expected, "{why}");
        }
    }
}
