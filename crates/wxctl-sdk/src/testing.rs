use anyhow::{Context, Result, bail};
use heck::ToSnakeCase;
use serde_json::Value;
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tracing::Instrument;
use wxctl_core::{Config, ResourceKey, parse_reference};
use wxctl_engine::RuntimeIdStore;

// ── Observer trait ──

/// Observer for test execution progress.
/// Called from the collection loop as each test case completes.
pub trait TestObserver: Send + Sync {
    /// Called when a test case starts executing.
    fn on_test_start(&self, _test_name: &str) {}
    /// Called when a test case finishes.
    fn on_test_complete(&self, _test_name: &str, _passed: bool, _completed: usize, _total: usize) {}
}

pub struct NoOpTestObserver;
impl TestObserver for NoOpTestObserver {}

// ── Public result types ──

/// Result of running all test cases.
#[derive(Debug)]
pub struct TestResults {
    pub tests: Vec<TestCaseResult>,
    pub passed: usize,
    pub failed: usize,
}

impl TestResults {
    pub fn total(&self) -> usize {
        self.tests.len()
    }

    pub fn has_failures(&self) -> bool {
        self.failed > 0
    }
}

/// Result of a single test case.
#[derive(Debug)]
pub struct TestCaseResult {
    pub ref_name: String,
    pub agent_ref: Option<String>,
    pub agent_id: Option<String>,
    pub deployment_ref: Option<String>,
    pub deployment_id: Option<String>,
    pub flow_ref: Option<String>,
    pub flow_id: Option<String>,
    pub passed: bool,
    pub turns: Vec<TurnResult>,
}

/// Result of a single conversation turn.
#[derive(Debug)]
pub struct TurnResult {
    pub turn_num: usize,
    pub total_turns: usize,
    pub message: String,
    pub expect_tools: Vec<String>,
    pub expect_answer: Option<String>,
    pub outcome: TurnOutcome,
}

/// Outcome of a single turn.
#[derive(Debug)]
pub enum TurnOutcome {
    Success { content: String, tool_calls: Vec<String> },
    ToolMismatch { expected: Vec<String>, actual: Vec<String>, content: String },
    Error(String),
}

// ── Internal types ──

#[derive(Debug)]
struct TestCase {
    ref_name: String,
    agent_ref: Option<String>,
    deployment_ref: Option<String>,
    flow_ref: Option<String>,
    turns: Vec<TestTurn>,
}

#[derive(Debug)]
struct TestTurn {
    message: String,
    expect_tools: Vec<ExpectedTool>,
    expect_answer: Option<String>,
    payload: Option<Value>,
    expect_response: Option<Value>,
}

/// A resolved `expect_tools` entry: the set of runtime tool-call names the agent gateway
/// might surface this tool under. A turn is satisfied if the agent calls ANY of the aliases.
///
/// wxO derives a Python tool's LLM-facing (runtime) name from its `display_name`, snake-cased —
/// NOT from the stored `name` — so `display_name: "QRadar Query"` is invoked as the tool call
/// `q_radar_query` (the `QR` acronym splits). OpenAPI- and MCP-backed tools instead keep their
/// stored `name` (the sanitized operationId / server tool name). We can't tell which derivation
/// a given tool uses without making the live call, so the resolved entry carries every plausible
/// name and we match against the whole set rather than betting on one — which also means a tool
/// referenced by `${tool.ref}` no longer needs `display_name` hand-aligned to its `name`.
#[derive(Debug, Clone, PartialEq)]
struct ExpectedTool {
    /// Name shown in mismatch reports — the tool's canonical `name`, falling back to the
    /// snake-cased `display_name` or the raw entry when the tool isn't in the store.
    label: String,
    /// Acceptable runtime tool-call names; the entry matches if any appears in the call list.
    aliases: Vec<String>,
}

#[derive(Debug)]
struct ChatResult {
    content: String,
    thread_id: Option<String>,
    tool_calls: Vec<String>,
}

struct IndexedResult {
    index: usize,
    result: TestCaseResult,
}

// ── Public entry point (called by WxctlClient::test) ──

pub(crate) async fn run_tests(config: &mut Config, pipeline: &wxctl_engine::Pipeline, concurrency_limit: usize, observer: Arc<dyn TestObserver>) -> Result<TestResults> {
    // 1. Partition resources
    let mut test_resources: Vec<Value> = Vec::new();
    let mut real_resources = Config { resources: Vec::new() };

    for resource in &config.resources {
        if resource.kind == "test" {
            let mut data = resource.data.clone();
            if let Some(obj) = data.as_object_mut() {
                obj.insert("kind".to_string(), Value::String("test".to_string()));
            }
            test_resources.push(data);
        } else {
            real_resources.resources.push(resource.clone());
        }
    }

    if test_resources.is_empty() {
        bail!("No test resources found in configuration");
    }

    // A `kind: test`-only config can't resolve `${kind.ref}` references: `wxctl test`
    // discovers live IDs by planning the REAL resources declared in the SAME config, so an
    // empty real-resource set yields an empty store and every reference fails with the
    // misleading "not found in store. Run 'wxctl apply' first.". Detect this config-handoff
    // mistake up front with an actionable message (an MCP agent is prone to passing test the
    // test documents alone — see harness/compose-e2e-sdk.mts).
    if let Some(msg) = test_only_config_error(&real_resources, &test_resources) {
        bail!("{msg}");
    }

    // 2. Plan to discover deployed resources
    let plan = pipeline.plan(&mut real_resources).await?;

    // 3. Build RuntimeIdStore from plan results
    let store = RuntimeIdStore::new();
    for planned_op in &plan.operations {
        let op = planned_op;
        if let Some(ref remote) = op.remote
            && remote.exists
        {
            store.insert(op.key.clone(), remote.data.clone());
        }
    }

    // 4. Get HTTP client credentials (lazy — only resolve for test types that exist)
    let has_agent_tests = test_resources.iter().any(|r| r.get("agent").is_some());
    let has_deployment_tests = test_resources.iter().any(|r| r.get("deployment").is_some());
    let has_flow_tests = test_resources.iter().any(|r| r.get("flow").is_some());

    // Agent chat and flow-run both hit the watsonx_orchestrate API → share its client.
    let (agent_base_url, agent_token, agent_auth_type) = if has_agent_tests || has_flow_tests {
        let c = pipeline.client_factory().create_client("watsonx_orchestrate")?;
        (c.base_url().to_string(), c.get_token().await?, c.auth_type().to_string())
    } else {
        Default::default()
    };

    let (deploy_base_url, deploy_token, deploy_auth_type) = if has_deployment_tests {
        let c = pipeline.client_factory().create_client("watsonx_ai")?;
        (c.base_url().to_string(), c.get_token().await?, c.auth_type().to_string())
    } else {
        Default::default()
    };

    // 5. Execute tests in parallel
    let reqwest_client = reqwest::Client::builder().timeout(std::time::Duration::from_secs(120)).build()?;

    let parallelism = concurrency_limit.max(1).min(test_resources.len());
    let semaphore = Arc::new(Semaphore::new(parallelism));
    let mut join_set = JoinSet::new();

    for (index, test_resource) in test_resources.iter().enumerate() {
        let mut test_case = parse_test_case(test_resource)?;

        for turn in &mut test_case.turns {
            for entry in &mut turn.expect_tools {
                *entry = resolve_expect_tool(&entry.label, &store);
            }
        }

        let client = reqwest_client.clone();
        let permit = Arc::clone(&semaphore);
        let observer = Arc::clone(&observer);

        if let Some(ref dep_ref) = test_case.deployment_ref {
            // Deployment test — try store, fall back to API for space-scoped resources
            let (deployment_id, space_id) = match resolve_resource_id(dep_ref, "Deployment", &store) {
                Ok(id) => (id, String::new()),
                Err(_) => resolve_deployment_id_from_api(&reqwest_client, &deploy_base_url, &deploy_token, &deploy_auth_type, dep_ref, &real_resources, &store).await?,
            };
            let base_url = deploy_base_url.clone();
            let token = deploy_token.clone();
            let auth_type = deploy_auth_type.clone();

            let asset_type = detect_asset_type(&reqwest_client, &base_url, &token, &auth_type, &deployment_id, &space_id).await?;

            join_set.spawn(async move {
                let _permit = permit.acquire().await.expect("semaphore closed");
                observer.on_test_start(&test_case.ref_name);
                let result = run_deployment_test(test_case, deployment_id, space_id, asset_type, client, base_url, token, auth_type).await;
                IndexedResult { index, result }
            });
        } else if let Some(ref agent_ref) = test_case.agent_ref {
            let agent_id = resolve_resource_id(agent_ref, "Agent", &store)?;
            let base_url = agent_base_url.clone();
            let token = agent_token.clone();
            let auth_type = agent_auth_type.clone();

            join_set.spawn(async move {
                let _permit = permit.acquire().await.expect("semaphore closed");
                observer.on_test_start(&test_case.ref_name);
                let result = run_single_test(test_case, agent_id, client, base_url, token, auth_type).await;
                IndexedResult { index, result }
            });
        } else if let Some(ref flow_ref) = test_case.flow_ref {
            // Flow tool runs directly via the flow engine — flow_id is the registered
            // tool's id (== binding.flow.flow_id). No LLM, no gateway.
            let flow_id = resolve_resource_id(flow_ref, "Flow tool", &store)?;
            let base_url = agent_base_url.clone();
            let token = agent_token.clone();
            let auth_type = agent_auth_type.clone();

            join_set.spawn(async move {
                let _permit = permit.acquire().await.expect("semaphore closed");
                observer.on_test_start(&test_case.ref_name);
                let result = run_flow_test(test_case, flow_id, client, base_url, token, auth_type).await;
                IndexedResult { index, result }
            });
        }
    }

    // 6. Collect results
    let total = test_resources.len();
    let mut completed = 0usize;
    let mut indexed_results: Vec<IndexedResult> = Vec::with_capacity(total);
    while let Some(result) = join_set.join_next().await {
        match result {
            Ok(indexed) => {
                completed += 1;
                observer.on_test_complete(&indexed.result.ref_name, indexed.result.passed, completed, total);
                indexed_results.push(indexed);
            }
            Err(e) => bail!("Test task panicked: {}", e),
        }
    }

    // Sort by original index for deterministic ordering
    indexed_results.sort_by_key(|r| r.index);

    let tests: Vec<TestCaseResult> = indexed_results.into_iter().map(|r| r.result).collect();
    let passed = tests.iter().filter(|t| t.passed).count();
    let failed = tests.len() - passed;

    Ok(TestResults { tests, passed, failed })
}

// ── Single test execution ──

async fn run_single_test(test_case: TestCase, agent_id: String, client: reqwest::Client, base_url: String, token: String, auth_type: String) -> TestCaseResult {
    let span = tracing::info_span!(
        target: "wxctl::stage::test",
        "test_case",
        test_name = %test_case.ref_name,
        agent_ref = %test_case.agent_ref.as_deref().unwrap_or(""),
        agent_id = %agent_id,
    );

    async {
        let mut passed = true;
        let mut turn_results = Vec::new();
        let mut thread_id: Option<String> = None;
        let total_turns = test_case.turns.len();

        for (i, turn) in test_case.turns.iter().enumerate() {
            let turn_num = i + 1;

            let turn_span = tracing::info_span!(
                target: "wxctl::substage::test_turn",
                "test_turn",
                test_name = %test_case.ref_name,
                turn = turn_num,
            );

            let outcome = async {
                match chat(&client, &base_url, &token, &auth_type, &agent_id, &turn.message, thread_id.as_deref()).await {
                    Ok(result) => {
                        if result.thread_id.is_some() {
                            thread_id = result.thread_id.clone();
                        }

                        if !turn.expect_tools.is_empty() {
                            // An entry is satisfied if the agent called the tool under ANY of its
                            // accepted runtime names (stored `name` or snake(display_name)).
                            let missing: Vec<&str> = turn.expect_tools.iter().filter(|t| !t.aliases.iter().any(|a| result.tool_calls.contains(a))).map(|t| t.label.as_str()).collect();

                            if !missing.is_empty() {
                                passed = false;
                                return TurnOutcome::ToolMismatch { expected: turn.expect_tools.iter().map(|t| t.label.clone()).collect(), actual: result.tool_calls, content: result.content };
                            }
                        }

                        TurnOutcome::Success { content: result.content, tool_calls: result.tool_calls }
                    }
                    Err(e) => {
                        passed = false;
                        TurnOutcome::Error(e.to_string())
                    }
                }
            }
            .instrument(turn_span)
            .await;

            let is_error = matches!(outcome, TurnOutcome::Error(_));

            turn_results.push(TurnResult { turn_num, total_turns, message: turn.message.clone(), expect_tools: turn.expect_tools.iter().map(|t| t.label.clone()).collect(), expect_answer: turn.expect_answer.clone(), outcome });

            if is_error {
                break;
            }
        }

        TestCaseResult { ref_name: test_case.ref_name.clone(), agent_ref: test_case.agent_ref.clone(), agent_id: Some(agent_id), deployment_ref: None, deployment_id: None, flow_ref: None, flow_id: None, passed, turns: turn_results }
    }
    .instrument(span)
    .await
}

// ── Deployment test execution ──

/// Deployed asset type determines which endpoint to call.
#[derive(Debug, Clone, PartialEq)]
enum DeployedAssetType {
    /// AI service — uses /ai_service and /ai_service_stream endpoints
    AiService,
    /// Python function or a stored model (e.g. AutoAI wml-hybrid) — uses the /predictions endpoint
    Function,
}

/// Fetch the deployment's `deployed_asset_type` from the WML API.
async fn detect_asset_type(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, deployment_id: &str, space_id: &str) -> Result<DeployedAssetType> {
    let url = format!("{}/ml/v4/deployments/{}?space_id={}&version=2024-01-01", base_url, deployment_id, space_id);

    let resp = apply_auth_scheme(client.get(&url), auth_type, token).send().await.context("Failed to fetch deployment details")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Failed to fetch deployment {} ({}): {}", deployment_id, status, body);
    }

    let data: Value = resp.json().await.context("Failed to parse deployment response")?;

    let asset_type = data.pointer("/entity/deployed_asset_type").and_then(|v| v.as_str()).unwrap_or("");

    match asset_type {
        "py_script" | "ai_service" => Ok(DeployedAssetType::AiService),
        // `model` covers stored WML models (e.g. an AutoAI wml-hybrid pipeline), which
        // score through the same /predictions endpoint as a Python function.
        "function" | "model" => Ok(DeployedAssetType::Function),
        other => bail!("Unsupported deployed_asset_type '{}' for deployment {}. Expected 'py_script', 'ai_service', 'function', or 'model'.", other, deployment_id),
    }
}

/// POST to a WML deployment endpoint and return the successful response.
#[allow(clippy::too_many_arguments)]
async fn post_wml(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, deployment_id: &str, space_id: &str, endpoint: &str, payload: &Value) -> Result<reqwest::Response> {
    let url = format!("{}/ml/v4/deployments/{}/{}?space_id={}&version=2024-01-01", base_url, deployment_id, endpoint, space_id);

    tracing::debug!(target: "wxctl::substage::test_turn", %url, "Sending {} request", endpoint);

    let resp = apply_auth_scheme(client.post(&url), auth_type, token).json(payload).send().await.with_context(|| format!("Failed to send {} request", endpoint))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let err = resp.text().await.unwrap_or_default();
        bail!("{} request failed ({}): {}", endpoint, status, err);
    }

    Ok(resp)
}

async fn call_predictions(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, deployment_id: &str, space_id: &str, payload: &Value) -> Result<Value> {
    post_wml(client, base_url, token, auth_type, deployment_id, space_id, "predictions", payload).await?.json().await.context("Failed to parse predictions response")
}

async fn call_ai_service(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, deployment_id: &str, space_id: &str, payload: &Value) -> Result<Value> {
    post_wml(client, base_url, token, auth_type, deployment_id, space_id, "ai_service", payload).await?.json().await.context("Failed to parse ai_service response")
}

/// Call the /ai_service_stream endpoint and return the last SSE data payload.
async fn call_ai_service_stream(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, deployment_id: &str, space_id: &str, payload: &Value) -> Result<Value> {
    let text = post_wml(client, base_url, token, auth_type, deployment_id, space_id, "ai_service_stream", payload).await?.text().await.context("Failed to read ai_service_stream response")?;

    let mut last_data: Option<Value> = None;
    for line in text.lines() {
        if let Some(data_str) = line.strip_prefix("data: ")
            && data_str != "[DONE]"
            && let Ok(data) = serde_json::from_str::<Value>(data_str)
        {
            last_data = Some(data);
        }
    }

    last_data.ok_or_else(|| anyhow::anyhow!("No data received from ai_service_stream"))
}

#[allow(clippy::too_many_arguments)]
async fn run_deployment_test(test_case: TestCase, deployment_id: String, space_id: String, asset_type: DeployedAssetType, client: reqwest::Client, base_url: String, token: String, auth_type: String) -> TestCaseResult {
    let span = tracing::info_span!(
        target: "wxctl::stage::test",
        "test_case",
        test_name = %test_case.ref_name,
        deployment_ref = %test_case.deployment_ref.as_deref().unwrap_or(""),
        deployment_id = %deployment_id,
        asset_type = ?asset_type,
    );

    async {
        let mut passed = true;
        let mut turn_results = Vec::new();
        let total_turns = test_case.turns.len();
        let empty_obj = Value::Object(serde_json::Map::new());

        for (i, turn) in test_case.turns.iter().enumerate() {
            let turn_num = i + 1;
            let payload = turn.payload.as_ref().unwrap_or(&empty_obj);

            let turn_span = tracing::info_span!(
                target: "wxctl::substage::test_turn",
                "test_turn",
                test_name = %test_case.ref_name,
                turn = turn_num,
            );

            let outcome = async {
                match &asset_type {
                    DeployedAssetType::Function => match call_predictions(&client, &base_url, &token, &auth_type, &deployment_id, &space_id, payload).await {
                        Ok(response) => validate_turn_response(&response, turn, &mut passed),
                        Err(e) => {
                            passed = false;
                            TurnOutcome::Error(e.to_string())
                        }
                    },
                    DeployedAssetType::AiService => {
                        // AI service: call /ai_service (generate) first
                        let generate_result = call_ai_service(&client, &base_url, &token, &auth_type, &deployment_id, &space_id, payload).await;
                        match generate_result {
                            Ok(response) => {
                                let gen_outcome = validate_turn_response(&response, turn, &mut passed);
                                if !passed {
                                    return gen_outcome;
                                }

                                // Then call /ai_service_stream (generate_stream) — verify it succeeds
                                // but don't validate response shape (stream wraps differently)
                                match call_ai_service_stream(&client, &base_url, &token, &auth_type, &deployment_id, &space_id, payload).await {
                                    Ok(_stream_response) => gen_outcome,
                                    Err(e) => {
                                        passed = false;
                                        TurnOutcome::Error(format!("generate_stream failed: {}", e))
                                    }
                                }
                            }
                            Err(e) => {
                                passed = false;
                                TurnOutcome::Error(format!("generate failed: {}", e))
                            }
                        }
                    }
                }
            }
            .instrument(turn_span)
            .await;

            let is_error = matches!(outcome, TurnOutcome::Error(_));

            turn_results.push(TurnResult { turn_num, total_turns, message: serde_json::to_string(payload).unwrap_or_default(), expect_tools: vec![], expect_answer: None, outcome });

            if is_error {
                break;
            }
        }

        TestCaseResult { ref_name: test_case.ref_name.clone(), agent_ref: None, agent_id: None, deployment_ref: test_case.deployment_ref.clone(), deployment_id: Some(deployment_id), flow_ref: None, flow_id: None, passed, turns: turn_results }
    }
    .instrument(span)
    .await
}

/// Run a flow tool directly via /v1/orchestrate/flows/{flow_id}/run for each turn.
/// Deterministic and gateway-independent (no agent/LLM) — asserts the flow's JSON
/// output against the turn's `expect_response` (subset match).
async fn run_flow_test(test_case: TestCase, flow_id: String, client: reqwest::Client, base_url: String, token: String, auth_type: String) -> TestCaseResult {
    let span = tracing::info_span!(
        target: "wxctl::stage::test",
        "test_case",
        test_name = %test_case.ref_name,
        flow_ref = %test_case.flow_ref.as_deref().unwrap_or(""),
        flow_id = %flow_id,
    );

    async {
        let mut passed = true;
        let mut turn_results = Vec::new();
        let total_turns = test_case.turns.len();
        let empty_obj = Value::Object(serde_json::Map::new());

        for (i, turn) in test_case.turns.iter().enumerate() {
            let turn_num = i + 1;
            let payload = turn.payload.as_ref().unwrap_or(&empty_obj);

            let turn_span = tracing::info_span!(
                target: "wxctl::substage::test_turn",
                "test_turn",
                test_name = %test_case.ref_name,
                turn = turn_num,
            );

            let outcome = async {
                match run_flow(&client, &base_url, &token, &auth_type, &flow_id, payload).await {
                    Ok(response) => validate_turn_response(&response, turn, &mut passed),
                    Err(e) => {
                        passed = false;
                        TurnOutcome::Error(e.to_string())
                    }
                }
            }
            .instrument(turn_span)
            .await;

            let is_error = matches!(outcome, TurnOutcome::Error(_));

            turn_results.push(TurnResult { turn_num, total_turns, message: serde_json::to_string(payload).unwrap_or_default(), expect_tools: vec![], expect_answer: turn.expect_answer.clone(), outcome });

            if is_error {
                break;
            }
        }

        TestCaseResult { ref_name: test_case.ref_name.clone(), agent_ref: None, agent_id: None, deployment_ref: None, deployment_id: None, flow_ref: test_case.flow_ref.clone(), flow_id: Some(flow_id), passed, turns: turn_results }
    }
    .instrument(span)
    .await
}

/// POST a flow input to /v1/orchestrate/flows/{flow_id}/run and return its JSON output.
async fn run_flow(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, flow_id: &str, payload: &Value) -> Result<Value> {
    let url = format!("{}/v1/orchestrate/flows/{}/run", base_url, flow_id);
    tracing::debug!(target: "wxctl::substage::test_turn", %url, "Running flow");

    let req = apply_auth_scheme(client.post(&url).header("Content-Type", "application/json").json(payload), auth_type, token);

    let resp = req.send().await.context("Failed to send flow run request")?;
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    if !status.is_success() {
        bail!("Flow run failed ({}): {}", status, body);
    }
    serde_json::from_str(&body).with_context(|| format!("Failed to parse flow run response: {}", body))
}

/// Apply the configured auth scheme to a request: apikey→Bearer, zenapikey→ZenApiKey,
/// basic→user:pass. Single home for scheme selection across every authenticated call —
/// the wxO chat/flow paths AND the WML deployment-scoring path (`/ml/v4/...`). The WML
/// path must route through here too: under `auth_type: zenapikey` (CP4D), a bare
/// `bearer_auth(token)` sends `Authorization: Bearer <base64(user:apikey)>`, which the
/// WML API rejects 401 — it expects the `ZenApiKey` scheme.
fn apply_auth_scheme(req: reqwest::RequestBuilder, auth_type: &str, token: &str) -> reqwest::RequestBuilder {
    if auth_type == "basic" {
        match token.split_once(':') {
            Some((user, pass)) => req.basic_auth(user, Some(pass)),
            None => req,
        }
    } else if auth_type == "zenapikey" {
        // ZenApiKey wire form: `Authorization: ZenApiKey <base64(user:apikey)>`. TokenManager::get_token already
        // returns the base64-encoded credential; we just add the scheme prefix. Falling through to bearer_auth would
        // send `Authorization: Bearer …`, which wxO software rejects 401 on the per-agent chat path.
        req.header("Authorization", format!("ZenApiKey {}", token))
    } else {
        req.bearer_auth(token)
    }
}

/// Validate a response against the turn's expect_response (subset match).
fn validate_turn_response(response: &Value, turn: &TestTurn, passed: &mut bool) -> TurnOutcome {
    if let Some(expected) = &turn.expect_response
        && !is_subset_match(expected, response)
    {
        *passed = false;
        return TurnOutcome::Error(format!("Response mismatch.\n  Expected (subset): {}\n  Actual: {}", serde_json::to_string_pretty(expected).unwrap_or_default(), serde_json::to_string_pretty(response).unwrap_or_default()));
    }

    TurnOutcome::Success { content: serde_json::to_string(response).unwrap_or_default(), tool_calls: vec![] }
}

// ── Chat + SSE parsing ──

async fn chat(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, agent_id: &str, message: &str, thread_id: Option<&str>) -> Result<ChatResult> {
    let url = format!("{}/v1/orchestrate/{}/chat/completions", base_url, agent_id);

    tracing::debug!(
        target: "wxctl::substage::test_turn",
        %url,
        %message,
        thread_id = thread_id.unwrap_or("none"),
        "Sending chat request"
    );

    let body = serde_json::json!({
        "messages": [{"role": "user", "content": message}]
    });

    let mut req = apply_auth_scheme(client.post(&url).header("Content-Type", "application/json").json(&body), auth_type, token);

    if let Some(tid) = thread_id {
        req = req.header("X-Ibm-Thread-Id", tid);
    }

    let mut resp = req.send().await.context("Failed to send chat request")?;
    let status = resp.status();

    if !status.is_success() {
        let err_body = resp.text().await.unwrap_or_default();
        tracing::warn!(
            target: "wxctl::substage::test_turn",
            %url,
            %status,
            %err_body,
            "Chat request failed"
        );
        bail!("Chat request failed ({}): {}", status, err_body);
    }

    // Read the SSE body chunk-by-chunk rather than `resp.text()`. The wxO chat
    // stream is chunked `text/event-stream`; if an upstream idle timeout fires
    // mid-stream (e.g. an agent emits a tool call that never returns), the
    // connection is closed without a terminating chunk. `resp.text()` would then
    // discard everything with an opaque "error decoding response body". Instead
    // we keep whatever arrived and flag truncation so we can report precisely.
    let mut buf: Vec<u8> = Vec::new();
    let mut truncated = false;
    loop {
        match resp.chunk().await {
            Ok(Some(bytes)) => buf.extend_from_slice(&bytes),
            Ok(None) => break,
            Err(e) => {
                truncated = true;
                tracing::warn!(
                    target: "wxctl::substage::test_turn",
                    %url,
                    bytes_read = buf.len(),
                    error = %e,
                    "Chat SSE stream truncated (connection closed mid-stream)"
                );
                break;
            }
        }
    }

    let text = String::from_utf8_lossy(&buf);

    tracing::debug!(
        target: "wxctl::substage::test_turn",
        %url,
        %status,
        response_len = text.len(),
        truncated,
        "Chat response received"
    );

    finalize_chat(parse_sse_response(text.as_ref())?, truncated, buf.len())
}

/// Decide whether a (possibly truncated) chat stream is a usable result or an error.
///
/// A clean stream — or a truncated one that still delivered the assistant's answer —
/// is returned as-is. A truncation that yielded no answer means the turn never
/// completed (most often the agent emitted a tool call that never returned, e.g. the
/// wxO tools-runtime failed to provision the tool); surface an actionable message
/// instead of an empty/opaque result.
fn finalize_chat(result: ChatResult, truncated: bool, bytes_read: usize) -> Result<ChatResult> {
    if truncated && result.content.trim().is_empty() {
        if !result.tool_calls.is_empty() {
            bail!("chat SSE stream truncated after tool call(s) [{}] with no assistant answer — the tool likely never returned (check the wxO tools-runtime / tool provisioning)", result.tool_calls.join(", "));
        }
        bail!("chat SSE stream truncated with no assistant answer ({bytes_read} bytes received before the connection closed)");
    }
    Ok(result)
}

fn parse_sse_response(text: &str) -> Result<ChatResult> {
    let mut content = String::new();
    let mut thread_id: Option<String> = None;
    let mut tool_calls: Vec<String> = Vec::new();

    for line in text.lines() {
        if let Some(data_str) = line.strip_prefix("data: ")
            && let Ok(data) = serde_json::from_str::<Value>(data_str)
        {
            if thread_id.is_none()
                && let Some(tid) = data.get("thread_id").and_then(|v| v.as_str())
            {
                thread_id = Some(tid.to_string());
            }
            if let Some(choice) = data.get("choices").and_then(|c| c.get(0)) {
                let delta = choice.get("delta");
                if let Some(c) = delta.and_then(|d| d.get("content")).and_then(|c| c.as_str()) {
                    content.push_str(c);
                }
                if let Some(steps) = delta.and_then(|d| d.get("step_details"))
                    && let Some(calls) = steps.get("tool_calls").and_then(|v| v.as_array())
                {
                    for call in calls {
                        if let Some(name) = call.get("name").and_then(|v| v.as_str())
                            && !tool_calls.contains(&name.to_string())
                        {
                            tool_calls.push(name.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(ChatResult { content, thread_id, tool_calls })
}

fn parse_test_case(value: &Value) -> Result<TestCase> {
    let ref_name = value.get("ref_name").and_then(|v| v.as_str()).unwrap_or("unnamed").to_string();

    let agent_ref = value.get("agent").and_then(|v| v.as_str()).map(|s| s.to_string());

    let deployment_ref = value.get("deployment").and_then(|v| v.as_str()).map(|s| s.to_string());

    // A `flow:` target runs a flow tool directly via /v1/orchestrate/flows/{flow_id}/run
    // (deterministic, gateway-independent — bypasses the agent/LLM path).
    let flow_ref = value.get("flow").and_then(|v| v.as_str()).map(|s| s.to_string());

    if agent_ref.is_none() && deployment_ref.is_none() && flow_ref.is_none() {
        bail!("Test '{}' must have an 'agent', 'deployment', or 'flow' field", ref_name);
    }

    let turns = value.get("turns").and_then(|v| v.as_array()).ok_or_else(|| anyhow::anyhow!("Test '{}' missing 'turns' array", ref_name))?;

    let turns: Vec<TestTurn> = turns
        .iter()
        .map(|turn| {
            let message = turn.get("message").and_then(|v| v.as_str()).unwrap_or("").to_string();

            // Raw entries (`${tool.ref}`, a bare ref_name, or a literal runtime name). The
            // alias set is filled in by `resolve_expect_tool` once the RuntimeIdStore is built;
            // until then `label` holds the raw entry and `aliases` is empty.
            let expect_tools = turn.get("expect_tools").and_then(|v| v.as_array()).map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| ExpectedTool { label: s.to_string(), aliases: Vec::new() })).collect()).unwrap_or_default();

            let expect_answer = turn.get("expect_answer").and_then(|v| v.as_str()).map(|s| s.to_string());

            let payload = turn.get("payload").cloned();
            let expect_response = turn.get("expect_response").cloned();

            TestTurn { message, expect_tools, expect_answer, payload, expect_response }
        })
        .collect();

    Ok(TestCase { ref_name, agent_ref, deployment_ref, flow_ref, turns })
}

/// Resolve an `expect_tools` entry to the set of runtime tool-call names it may match.
///
/// An entry is a `${tool.ref}` / bare ref_name (looked up in the discovery store) or a literal
/// runtime name. When the tool is found, the accepted aliases are its stored `name` AND the
/// snake-cased `display_name` — because the agent gateway surfaces a Python tool to the LLM under
/// `snake(display_name)`, not the stored `name` (e.g. `display_name: "QRadar Query"` → tool call
/// `q_radar_query`), while OpenAPI/MCP tools keep their stored `name`. We can't know which a given
/// tool uses without the live call, so we accept all plausible names. The raw entry is always kept
/// as an alias so a hand-written literal (or an unresolved ref) still matches itself.
fn resolve_expect_tool(entry: &str, store: &RuntimeIdStore) -> ExpectedTool {
    let reference = parse_reference(entry);
    let data = reference.as_ref().and_then(|key| store.get(key)).or_else(|| store.get(&ResourceKey::new("tool", entry)));

    let mut canonical: Option<String> = None;
    let mut aliases: Vec<String> = Vec::new();

    if let Some(data) = &data {
        if let Some(name) = data.get("name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            canonical = Some(name.to_string());
            aliases.push(name.to_string());
        }
        if let Some(display_name) = data.get("display_name").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            aliases.push(display_name.to_snake_case());
        }
    }
    // A bare entry (not `${...}` syntax) may itself be the literal runtime name the author
    // hard-coded — keep it. The `${kind.ref}` form is never a runtime name, so we don't.
    if reference.is_none() {
        aliases.push(entry.to_string());
    }
    // Last resort (e.g. an unresolved `${...}` ref): match the raw entry so the turn reports a
    // clean miss rather than matching nothing silently.
    if aliases.is_empty() {
        aliases.push(entry.to_string());
    }

    // Dedup, preserving first-seen order.
    let mut seen = HashSet::new();
    aliases.retain(|a| !a.is_empty() && seen.insert(a.clone()));

    let label = canonical.or_else(|| aliases.first().cloned()).unwrap_or_else(|| entry.to_string());
    ExpectedTool { label, aliases }
}

/// Detect a `kind: test`-only config (no real resources) whose tests reference live resources
/// by `${kind.ref}`. Those references can't resolve because `wxctl test` discovers IDs by
/// planning the real resources declared in the SAME config — a test-only config gives an empty
/// store. Returns an actionable error for that common MCP config-handoff mistake, else `None`.
fn test_only_config_error(real_resources: &Config, test_resources: &[Value]) -> Option<String> {
    let tests_need_refs = test_resources.iter().any(|r| r.get("agent").or_else(|| r.get("deployment")).or_else(|| r.get("flow")).is_some());
    (real_resources.resources.is_empty() && tests_need_refs).then(|| {
        "The config passed to `wxctl test` contains only `kind: test` documents. `wxctl test` resolves the `${kind.ref}` references in the test suite (e.g. the agent under test) by discovering the real resources declared in the SAME config — pass the full config (the real resources together with the `kind: test` documents), not the test documents alone.".to_string()
    })
}

/// Resolve a `${kind.name}` reference to its ID in the RuntimeIdStore.
fn resolve_resource_id(ref_str: &str, label: &str, store: &RuntimeIdStore) -> Result<String> {
    let key = parse_reference(ref_str).ok_or_else(|| anyhow::anyhow!("Invalid {} reference: '{}' (expected ${{kind.name}})", label, ref_str))?;

    let data = store.get(&key).ok_or_else(|| anyhow::anyhow!("{} '{}' not found in store. Either the config passed to `wxctl test` does not include the real `{}` resource named '{}' (pass the full config — the real resources together with the `kind: test` documents — not the test documents alone), or it has not been deployed yet (run `wxctl apply` first).", label, key.name, key.kind, key.name))?;

    data.get("id").and_then(|v| v.as_str()).map(|s| s.to_string()).ok_or_else(|| anyhow::anyhow!("{} '{}' has no 'id' field in server response", label, key.name))
}

/// Resolve a deployment ID and its space_id via the WML API.
/// Fallback for space-scoped resources whose template references prevent discovery during plan().
async fn resolve_deployment_id_from_api(client: &reqwest::Client, base_url: &str, token: &str, auth_type: &str, deployment_ref: &str, real_resources: &Config, store: &RuntimeIdStore) -> Result<(String, String)> {
    let deploy_key = parse_reference(deployment_ref).ok_or_else(|| anyhow::anyhow!("Invalid deployment reference: '{}'", deployment_ref))?;

    let deploy_resource = real_resources.resources.iter().find(|r| *r.kind == *deploy_key.kind && r.data.get("ref_name").and_then(|v| v.as_str()) == Some(&*deploy_key.name)).ok_or_else(|| anyhow::anyhow!("Deployment resource '{}' not found in config", deploy_key.name))?;

    let deploy_name = deploy_resource.data.get("name").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Deployment '{}' missing 'name' field", deploy_key.name))?;

    let space_id_raw = deploy_resource.data.get("space_id").and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Deployment '{}' missing 'space_id' field", deploy_key.name))?;

    let space_id = if let Some(space_key) = parse_reference(space_id_raw) {
        let space_data = store.get(&space_key).ok_or_else(|| anyhow::anyhow!("Space '{}' not found. Run 'wxctl apply' first.", space_key.name))?;
        // Space data uses metadata.id (common_core API pattern) with fallback to top-level id
        space_data.pointer("/metadata/id").or_else(|| space_data.get("id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Space '{}' has no id in metadata.id or id", space_key.name))?.to_string()
    } else {
        space_id_raw.to_string()
    };

    // List deployments in the space, filtered by name
    let url = format!("{}/ml/v4/deployments?space_id={}&name={}&version=2024-01-01", base_url, space_id, deploy_name);

    tracing::debug!(target: "wxctl::stage::test", %url, %deploy_name, "Looking up deployment via API");

    let resp = apply_auth_scheme(client.get(&url), auth_type, token).send().await.context("Failed to list deployments")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("Failed to list deployments ({}): {}", status, body);
    }

    let data: Value = resp.json().await.context("Failed to parse deployment list")?;
    let resources = data.get("resources").and_then(|v| v.as_array()).ok_or_else(|| anyhow::anyhow!("No resources in deployment list response"))?;

    let id = resources.first().and_then(|r| r.pointer("/metadata/id")).and_then(|v| v.as_str()).ok_or_else(|| anyhow::anyhow!("Deployment '{}' not found in space. Run 'wxctl apply' first.", deploy_name))?;

    Ok((id.to_string(), space_id))
}

/// Check if `expected` is a subset of `actual` (recursive JSON comparison).
/// Objects: every key in expected must exist in actual with matching value.
/// Arrays: must match element-by-element (same length and each element matches).
/// Scalars: must be equal.
fn is_subset_match(expected: &Value, actual: &Value) -> bool {
    match (expected, actual) {
        (Value::Object(exp_map), Value::Object(act_map)) => exp_map.iter().all(|(k, v)| act_map.get(k).is_some_and(|av| is_subset_match(v, av))),
        (Value::Array(exp_arr), Value::Array(act_arr)) => exp_arr.len() == act_arr.len() && exp_arr.iter().zip(act_arr.iter()).all(|(e, a)| is_subset_match(e, a)),
        _ => expected == actual,
    }
}

// ── Unit tests ──

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::{RawResource, ResourceKey};

    #[test]
    fn test_only_config_error_handoff_branches() {
        let test_with_agent = || vec![json!({"ref_name": "t1", "agent": "${agent.x}", "turns": [{"message": "hi"}]})];
        // ONLY kind:test docs that reference an agent → actionable handoff error.
        let detected = test_only_config_error(&Config { resources: vec![] }, &test_with_agent()).expect("test-only config should be detected");
        assert!(detected.contains("only `kind: test`"), "{detected}");
        assert!(detected.contains("pass the full config"), "{detected}");
        // Real resources present alongside the test docs → no error.
        let full = Config { resources: vec![RawResource { kind: "agent".to_string(), data: json!({"ref_name": "x"}) }] };
        assert!(test_only_config_error(&full, &test_with_agent()).is_none());
        // Tests with no agent/deployment/flow reference need no real resources → no error.
        let payload_only = vec![json!({"ref_name": "t1", "payload": {"k": "v"}, "turns": [{"message": "hi"}]})];
        assert!(test_only_config_error(&Config { resources: vec![] }, &payload_only).is_none());
    }

    #[test]
    #[allow(clippy::type_complexity)]
    fn parse_sse_response_extracts_content_thread_id_and_deduped_tool_calls() {
        // (label, sse, expected content, expected thread_id, expected tool_calls)
        let cases: &[(&str, &str, &str, Option<&str>, &[&str])] = &[
            // content from successive delta chunks is concatenated; no tool calls
            ("content_concatenation", "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"}}]}", "Hello world", None, &[]),
            // thread_id is lifted off the chunks
            ("thread_id", "data: {\"thread_id\":\"tid-123\",\"choices\":[{\"delta\":{\"content\":\"Hi\"}}]}\ndata: {\"thread_id\":\"tid-123\",\"choices\":[{\"delta\":{\"content\":\"!\"}}]}", "Hi!", Some("tid-123"), &[]),
            // tool calls are harvested from step_details, content still concatenated
            ("tool_calls_from_step_details", "data: {\"choices\":[{\"delta\":{\"step_details\":{\"tool_calls\":[{\"name\":\"calculator_tool\"}]}}}]}\ndata: {\"choices\":[{\"delta\":{\"content\":\"The answer is 42\"}}]}", "The answer is 42", None, &["calculator_tool"]),
            // repeated tool calls are deduplicated
            ("duplicate_tool_calls_deduped", "data: {\"choices\":[{\"delta\":{\"step_details\":{\"tool_calls\":[{\"name\":\"calc\"}]}}}]}\ndata: {\"choices\":[{\"delta\":{\"step_details\":{\"tool_calls\":[{\"name\":\"calc\"}]}}}]}", "", None, &["calc"]),
            // non-data lines (event/comment) and the [DONE] sentinel are ignored
            ("non_data_lines_ignored", "event: ping\n: comment\ndata: {\"choices\":[{\"delta\":{\"content\":\"ok\"}}]}\n\ndata: [DONE]", "ok", None, &[]),
        ];
        for (label, sse, content, thread_id, tool_calls) in cases {
            let result = parse_sse_response(sse).unwrap_or_else(|e| panic!("{label} failed to parse: {e}"));
            assert_eq!(result.content, *content, "content for {label}");
            assert_eq!(result.thread_id.as_deref(), *thread_id, "thread_id for {label}");
            assert_eq!(result.tool_calls, *tool_calls, "tool_calls for {label}");
        }
    }

    #[test]
    fn finalize_chat_keeps_usable_streams_and_errors_on_empty_truncation() {
        // Expected outcome per case.
        enum Want {
            Content(&'static str),
            ErrContains(&'static [&'static str]),
        }
        // (label, ChatResult, truncated, len, expected)
        let cases: Vec<(&str, ChatResult, bool, usize, Want)> = vec![
            // Clean (non-truncated) stream passes through verbatim.
            ("clean_pass_through", ChatResult { content: "hello".into(), thread_id: None, tool_calls: vec![] }, false, 100, Want::Content("hello")),
            // Truncated but the answer already arrived → kept (terminating chunk merely dropped).
            ("truncated_with_answer", ChatResult { content: "IBM was founded in 1911".into(), thread_id: None, tool_calls: vec!["search".into()] }, true, 500, Want::Content("IBM was founded in 1911")),
            // cp4d case: tool call emitted, stream stalls, no answer → error names the tool + cause.
            ("truncated_after_tool_call", ChatResult { content: String::new(), thread_id: None, tool_calls: vec!["calculator".into()] }, true, 426, Want::ErrContains(&["calculator", "tools-runtime"])),
            // Truncated with neither content nor tools → generic truncation error.
            ("truncated_empty", ChatResult { content: "   ".into(), thread_id: None, tool_calls: vec![] }, true, 12, Want::ErrContains(&["truncated"])),
        ];
        for (label, r, truncated, len, want) in cases {
            match (finalize_chat(r, truncated, len), want) {
                (Ok(out), Want::Content(c)) => assert_eq!(out.content, c, "content for {label}"),
                (Err(e), Want::ErrContains(needles)) => {
                    let msg = e.to_string();
                    for needle in needles {
                        assert!(msg.contains(needle), "{label} error should contain {needle:?}: {msg}");
                    }
                }
                (Ok(out), Want::ErrContains(_)) => panic!("{label} expected error, got Ok({:?})", out.content),
                (Err(e), Want::Content(_)) => panic!("{label} expected Ok, got error: {e}"),
            }
        }
    }

    #[test]
    fn parse_test_case_valid_for_each_target_kind() {
        // Agent target with mixed turns: expect_tools label is the raw string pre-resolution.
        let agent = json!({
            "kind": "test", "ref_name": "test_calc", "agent": "${agent.calculator_agent}",
            "turns": [{"message": "What is 2+2?", "expect_tools": ["calculator_tool"], "expect_answer": "4"}, {"message": "Tell me about IBM"}]
        });
        let tc = parse_test_case(&agent).unwrap();
        assert_eq!(tc.ref_name, "test_calc");
        assert_eq!(tc.agent_ref.as_deref(), Some("${agent.calculator_agent}"));
        assert_eq!(tc.turns.len(), 2);
        assert_eq!(tc.turns[0].expect_tools.iter().map(|t| t.label.as_str()).collect::<Vec<_>>(), vec!["calculator_tool"]);
        assert!(tc.turns[1].expect_tools.is_empty());

        // Deployment target: payload + expect_response turn, only deployment_ref set.
        let deployment = json!({
            "kind": "test", "ref_name": "test_deploy", "deployment": "${wml_deployment.my_deploy}",
            "turns": [{"payload": {"input_data": [{"values": [[1, 2]]}]}, "expect_response": {"predictions": [{"values": [[1, 2]]}]}}]
        });
        let tc = parse_test_case(&deployment).unwrap();
        assert_eq!(tc.ref_name, "test_deploy");
        assert!(tc.agent_ref.is_none());
        assert_eq!(tc.deployment_ref.as_deref(), Some("${wml_deployment.my_deploy}"));
        assert!(tc.turns[0].payload.is_some() && tc.turns[0].expect_response.is_some());

        // Flow target: only flow_ref set.
        let flow = json!({
            "kind": "test", "ref_name": "test_flow", "flow": "${tool.insurance_flow_tool}",
            "turns": [{"payload": {"loan_amount": 200000, "grade": "A"}, "expect_response": {"insurance_required": true, "insurance_rate": 0.001}}]
        });
        let tc = parse_test_case(&flow).unwrap();
        assert_eq!(tc.ref_name, "test_flow");
        assert!(tc.agent_ref.is_none() && tc.deployment_ref.is_none());
        assert_eq!(tc.flow_ref.as_deref(), Some("${tool.insurance_flow_tool}"));
        assert!(tc.turns[0].payload.is_some() && tc.turns[0].expect_response.is_some());
    }

    #[test]
    fn parse_test_case_error_branches() {
        // No agent/deployment/flow target → names the three valid keys.
        assert!(parse_test_case(&json!({"ref_name": "bad_test", "turns": [{"message": "hi"}]})).unwrap_err().to_string().contains("'agent', 'deployment', or 'flow'"));
        // Target present but no turns → names the missing array.
        assert!(parse_test_case(&json!({"ref_name": "bad_test", "agent": "${agent.foo}"})).unwrap_err().to_string().contains("missing 'turns' array"));
    }

    #[test]
    fn resolve_resource_id_valid_missing_and_invalid_ref() {
        let store = RuntimeIdStore::new();
        store.insert(ResourceKey::new("agent", "my_agent"), json!({"id": "agent-uuid-123"}));
        store.insert(ResourceKey::new("wml_deployment", "my_deploy"), json!({"id": "deploy-uuid-456"}));
        // Valid: same resolution path regardless of kind/label (agent, deployment, …).
        assert_eq!(resolve_resource_id("${agent.my_agent}", "Agent", &store).unwrap(), "agent-uuid-123");
        assert_eq!(resolve_resource_id("${wml_deployment.my_deploy}", "Deployment", &store).unwrap(), "deploy-uuid-456");
        // Missing entry → "not found in store".
        assert!(resolve_resource_id("${agent.nonexistent}", "Agent", &store).unwrap_err().to_string().contains("not found in store"));
        // Malformed reference (no `${...}`) → "Invalid Agent reference".
        assert!(resolve_resource_id("agent.foo", "Agent", &store).unwrap_err().to_string().contains("Invalid Agent reference"));
    }

    #[test]
    fn apply_auth_scheme_schemes() {
        // Smoke: the helper builds without panicking for each scheme; bearer is the default.
        let client = reqwest::Client::new();
        let _ = apply_auth_scheme(client.post("http://localhost/x"), "apikey", "tok");
        let _ = apply_auth_scheme(client.post("http://localhost/x"), "zenapikey", "tok");
        let _ = apply_auth_scheme(client.post("http://localhost/x"), "basic", "user:pass");
    }

    #[test]
    fn is_subset_match_accepts_extra_keys_rejects_value_mismatch() {
        // (label, expected, actual, want)
        let cases: &[(&str, serde_json::Value, serde_json::Value, bool)] = &[
            // actual is a superset (extra key) of expected → match
            ("superset", json!({"predictions": [{"values": [[1, 2, 3]]}]}), json!({"predictions": [{"values": [[1, 2, 3]]}], "extra": true}), true),
            // differing leaf values → no match
            ("value_mismatch", json!({"predictions": [{"values": [[1, 2]]}]}), json!({"predictions": [{"values": [[9, 9]]}]}), false),
        ];
        for (label, expected, actual, want) in cases {
            assert_eq!(is_subset_match(expected, actual), *want, "{label}");
        }
    }

    #[test]
    fn resolve_expect_tool_via_reference_or_bare_ref_name() {
        // Both `${tool.<ref>}` and a bare `<ref>` resolve identically against the store.
        // name and snake(display_name) coincide here → a single alias.
        let store = RuntimeIdStore::new();
        store.insert(ResourceKey::new("tool", "calculator_tool"), json!({"name": "calculator_tool", "display_name": "Calculator Tool"}));
        for input in ["${tool.calculator_tool}", "calculator_tool"] {
            let et = resolve_expect_tool(input, &store);
            assert_eq!(et.label, "calculator_tool", "label for {input}");
            assert_eq!(et.aliases, vec!["calculator_tool"], "aliases for {input}");
        }
    }

    #[test]
    fn resolve_expect_tool_accepts_snake_display_name_alias() {
        // The bug this fixes: wxO surfaces a Python tool to the LLM under snake(display_name),
        // not its stored `name`. `display_name: "QRadar Query"` → runtime tool call
        // `q_radar_query`. Referencing the tool by ref_name must accept BOTH, so the author
        // need not hand-align display_name to the name.
        let store = RuntimeIdStore::new();
        store.insert(ResourceKey::new("tool", "qradar_query"), json!({"name": "qradar_query", "display_name": "QRadar Query"}));

        let et = resolve_expect_tool("${tool.qradar_query}", &store);
        assert!(et.aliases.contains(&"qradar_query".to_string()), "stored name accepted: {:?}", et.aliases);
        assert!(et.aliases.contains(&"q_radar_query".to_string()), "snake(display_name) accepted: {:?}", et.aliases);
        // Bare ref_name resolves identically.
        assert_eq!(resolve_expect_tool("qradar_query", &store).aliases, et.aliases);
    }

    #[test]
    fn resolve_expect_tool_returns_server_name_when_ref_and_name_differ() {
        // OpenAPI-expanded tool: ref_name `httpbin_tools_echoGet`, server `name` `echo_get`.
        // No display_name → the stored name is the only resolved alias (plus the raw entry).
        let store = RuntimeIdStore::new();
        store.insert(ResourceKey::new("tool", "httpbin_tools_echoGet"), json!({"name": "echo_get"}));
        let et = resolve_expect_tool("${tool.httpbin_tools_echoGet}", &store);
        assert_eq!(et.label, "echo_get");
        assert!(et.aliases.contains(&"echo_get".to_string()));
        assert_eq!(resolve_expect_tool("httpbin_tools_echoGet", &store).label, "echo_get");
    }

    #[test]
    fn resolve_expect_tool_literal_fallback() {
        let store = RuntimeIdStore::new();
        // Nothing in the store — entry passes through unchanged so legacy slugified
        // strings keep working.
        let echo = resolve_expect_tool("echo_get", &store);
        assert_eq!(echo.label, "echo_get");
        assert_eq!(echo.aliases, vec!["echo_get"]);
        assert_eq!(resolve_expect_tool("Calculator Tool", &store).aliases, vec!["Calculator Tool"]);
    }

    #[test]
    fn snake_case_matches_observed_wxo_runtime_names() {
        // Guards against `heck` behaviour drift — these are the exact derivations wxO performs.
        assert_eq!("QRadar Query".to_snake_case(), "q_radar_query");
        assert_eq!("Calculator Tool".to_snake_case(), "calculator_tool");
        assert_eq!("agentdeps Calculator".to_snake_case(), "agentdeps_calculator");
        // Already-snake names are stable (idempotent).
        assert_eq!("qradar_query".to_snake_case(), "qradar_query");
    }
}
