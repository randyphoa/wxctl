//! rmcp stdio server: the tool routers, the read-only discovery + authoring tools
//! (Phases 1-2), the gated mutating tools (`apply`/`destroy`/`test`, Phase 3), the
//! compose pipeline tools (Phase 4), the `ServerHandler`, and the `serve` entry point.
//!
//! Tools live in four `#[tool_router]` blocks: `base_tool_router` (8 read-only live
//! tools including runs), `compose_tool_router` (3 read-only compose tools: compose_start,
//! compose_paths, compose_prompt), `mutating_tool_router` (3 mutating live tools), and
//! `compose_mutating_tool_router` (1 FS-writing compose tool). `new(profile, profile_path,
//! read_only)` composes `base() + compose()` under `--read-only`, else all four (rmcp
//! `ToolRouter: Add`). So a `--read-only` server never registers apply/destroy/test/scaffold
//! — calling them yields a standard MCP "unknown tool" (spec Error Handling table).
//!
//! The live client is built lazily + once and shared via `Arc<OnceCell<Arc<WxctlClient>>>`.
//! Mutating tools require `confirm: true`, inject `Peer`/`Meta`/`CancellationToken`, and
//! stream MCP progress by bridging a synchronous engine/SDK observer through an mpsc
//! channel drained by a spawned task that awaits `peer.notify_progress`.

use std::sync::Arc;

use rmcp::{
    Json, Peer, RoleServer, ServerHandler, ServiceExt,
    handler::server::router::tool::ToolRouter,
    handler::server::wrapper::Parameters,
    model::{Implementation, JsonObject, Meta, ProgressNotificationParam, ProgressToken, ServerCapabilities, ServerInfo},
    tool, tool_handler, tool_router,
    transport::stdio,
};
use tokio::sync::OnceCell;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio_util::sync::CancellationToken;
use wxctl_sdk::WxctlClient;

use tracing::Instrument;

use crate::compose_tools::{ComposePathsInput, ComposePromptInput, ComposeScaffoldInput, ComposeStartInput, ComposeStartOutput, PathsOutputDto, PromptOutput, ScaffoldOutputDto, compose_paths as run_paths, compose_prompt as run_prompt, compose_scaffold as run_scaffold, compose_start as run_start};
use crate::config_input::ConfigInput;
use crate::run_scope::McpRunScope;
use crate::tools::{ExplainKindInput, ExplainKindOutput, ListResourceKindsInput, ListResourceKindsOutput, explain_kind, list_resource_kinds};
use crate::tools_live::shape_validation;
use crate::tools_mutate::{ProgressEvent, ProgressExecutionObserver, ProgressTestObserver};
use crate::tools_runs::{RunDiagnoseInput, RunDiagnoseOutput, RunEventsQueryInput, RunEventsQueryOutput, RunGetInput, RunGetOutput, RunsListInput, RunsListOutput, run_diagnose, run_events_query, run_get, runs_list};
use wxctl_sdk::json::{ExecuteOutput, PlanOutput, TestOutput, ValidateOutput, execute_output, plan_output, test_output};

/// Input for `wxctl_plan`: a config input plus an opt-in `verbose` flag.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct PlanInput {
    #[serde(flatten)]
    pub config_input: ConfigInput,
    /// Include raw per-operation local/remote payloads in the response (default false).
    #[serde(default)]
    pub verbose: bool,
}

/// Input for `wxctl_validate`: a config input plus an opt-in `skip_post_validate` flag.
/// `skip_post_validate: true` skips the per-handler `post_validate` hook (source-file
/// existence / enrichment), matching the CLI's `--skip-post-validate` — the config-tier
/// (pre-scaffold) validation the compose flow uses before source files are materialized.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ValidateInput {
    #[serde(flatten)]
    pub config_input: ConfigInput,
    /// Skip the `post_validate` hook (source-file-existence / handler enrichment). Default
    /// false. Set true to validate a config whose source files do not yet exist (pre-scaffold).
    #[serde(default)]
    pub skip_post_validate: bool,
}

/// Input for `wxctl_apply` / `wxctl_destroy`: config input + required `confirm` + `verbose`.
#[derive(Debug, serde::Deserialize, schemars::JsonSchema)]
pub struct ExecuteInput {
    #[serde(flatten)]
    pub config_input: ConfigInput,
    /// Must be `true` to proceed. Run `wxctl_plan` first, review the diff, then re-call
    /// with `confirm: true`. Absent/false returns an error directing to plan-then-confirm.
    #[serde(default)]
    pub confirm: bool,
    /// Include the raw per-resource API `response` for each result (default false).
    #[serde(default)]
    pub verbose: bool,
}

/// Which mutating engine call `run_mutation` drives. Selects the SDK method, the
/// `McpRunScope` command label, the confirm-gate message, and the error prose —
/// the only things that differ between `wxctl_apply` and `wxctl_destroy`.
#[derive(Clone, Copy)]
enum MutationOp {
    Apply,
    Destroy,
}

impl MutationOp {
    /// `McpRunScope::begin` command label / run-record command.
    fn command(self) -> &'static str {
        match self {
            MutationOp::Apply => "apply",
            MutationOp::Destroy => "destroy",
        }
    }

    /// Verb used in the `Err(...)` prose: `"<verb> failed: ..."`.
    fn verb(self) -> &'static str {
        match self {
            MutationOp::Apply => "apply",
            MutationOp::Destroy => "destroy",
        }
    }

    /// The exact message returned when `confirm` is not `true`. Byte-identical to the
    /// pre-refactor per-tool gate strings.
    fn confirm_error(self) -> &'static str {
        match self {
            MutationOp::Apply => "apply requires confirm:true, and only after an error-free wxctl_plan. Run wxctl_plan first to review the diff and confirm it reports no errors, then re-call wxctl_apply with confirm:true.",
            MutationOp::Destroy => "destroy requires confirm:true. Run wxctl_plan first to review what will be removed, then re-call wxctl_destroy with confirm:true to proceed.",
        }
    }
}

/// The server. Holds the (possibly read-only) tool router, the lazily-built shared live
/// client, and the launch-profile selectors.
#[derive(Clone)]
pub struct WxctlMcpServer {
    tool_router: ToolRouter<Self>,
    profile: Arc<str>,
    profile_path: Option<Arc<str>>,
    client: Arc<OnceCell<Arc<WxctlClient>>>,
    full_trace: bool,
}

impl WxctlMcpServer {
    /// Build the server. `read_only` registers the read-only tools (live discovery/validate/plan/runs + compose_start/compose_paths/compose_prompt);
    /// otherwise all tools (+ 3 mutating live + 1 compose_scaffold). `full_trace` enables
    /// full HTTP-exchange capture in run records.
    pub fn new(profile: &str, profile_path: Option<&str>, read_only: bool, full_trace: bool) -> Self {
        let mut tool_router = if read_only { Self::base_tool_router() + Self::compose_tool_router() } else { Self::base_tool_router() + Self::compose_tool_router() + Self::mutating_tool_router() + Self::compose_mutating_tool_router() };
        // schemars stamps Rust integer widths into JSON Schema `format` ("uint", "int64", …),
        // which the spec does not define; ajv-based MCP clients (IBM Bob among them) then log
        // `unknown format "uint" ignored in schema at path …` once per field per tool. Strip
        // them at registration — `type: integer` + `minimum: 0` already carry the constraint.
        for route in tool_router.map.values_mut() {
            route.attr.input_schema = Arc::new(strip_rust_numeric_formats((*route.attr.input_schema).clone()));
            if let Some(output_schema) = route.attr.output_schema.take() {
                route.attr.output_schema = Some(Arc::new(strip_rust_numeric_formats((*output_schema).clone())));
            }
        }
        Self { tool_router, profile: Arc::from(profile), profile_path: profile_path.map(Arc::from), client: Arc::new(OnceCell::new()), full_trace }
    }

    /// Build the live client once, on first live-tool use, and share it across calls.
    /// `Err(String)` carries the surfaced `WxctlError` (becomes an `isError` result).
    async fn live_client(&self) -> Result<Arc<WxctlClient>, String> {
        self.client.get_or_try_init(|| async { WxctlClient::new(&self.profile, self.profile_path.as_deref()).map(Arc::new).map_err(|e| format!("could not initialize wxctl client for profile '{}': {e}", self.profile)) }).await.cloned()
    }
}

impl WxctlMcpServer {
    /// Shared apply/destroy runner: confirm-gate → load → scope → progress channel +
    /// drain → observer → instrumented engine call → match-on-outcome. `op` selects
    /// `apply_with` vs `destroy_with`, the run-record command label, the confirm-gate
    /// message, and the error prose. Behavior is byte-identical to the pre-refactor
    /// `wxctl_apply` / `wxctl_destroy` bodies.
    async fn run_mutation(&self, op: MutationOp, input: ExecuteInput, peer: Peer<RoleServer>, meta: Meta, cancel: CancellationToken) -> Result<Json<ExecuteOutput>, String> {
        if !input.confirm {
            return Err(op.confirm_error().to_string());
        }
        let mut config = input.config_input.load_deployable()?;
        let client = self.live_client().await?;
        let scope = McpRunScope::begin(op.command(), &self.profile, input.config_input.scope_paths(), self.full_trace);
        // Record the run's deployment scope now that the live client (and its resolved
        // profile) is available — mirrors `CommandContext::setup_with_render`'s recording.
        let deployment = client.profile_deployment().unwrap_or(wxctl_core::types::Deployment::Saas);
        scope.set_deployment(Some(deployment.flavor().to_string()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
        spawn_progress_drain(peer, meta.get_progress_token(), rx);
        let observer = Arc::new(ProgressExecutionObserver::new(tx));
        let result = match op {
            MutationOp::Apply => client.apply_with(&mut config, observer, cancel).instrument(scope.span()).await,
            MutationOp::Destroy => client.destroy_with(&mut config, observer, cancel).instrument(scope.span()).await,
        };
        match result {
            Ok(results) => {
                let outcome = if results.failed.is_empty() && !results.cancelled { "success" } else { "failed" };
                scope.finish(outcome);
                Ok(Json(execute_output(scope.run_id.clone(), &results, input.verbose)))
            }
            Err(e) => {
                scope.finish("failed");
                Err(format!("{} failed: {e} (run_id: {})", op.verb(), scope.run_id))
            }
        }
    }
}

/// Spawn a task that forwards `ProgressEvent`s from `rx` to the MCP client as
/// `notifications/progress`, until the channel closes (all senders dropped). Returns
/// immediately. A missing progress token (client did not request progress) → no-op drain.
fn spawn_progress_drain(peer: Peer<RoleServer>, token: Option<ProgressToken>, mut rx: UnboundedReceiver<ProgressEvent>) {
    tokio::spawn(async move {
        let Some(token) = token else {
            // Drain to completion so senders never block; drop events.
            while rx.recv().await.is_some() {}
            return;
        };
        while let Some(ev) = rx.recv().await {
            let _ = peer.notify_progress(ProgressNotificationParam::new(token.clone(), ev.done).with_message(ev.message)).await;
        }
    });
}

#[tool_router(router = base_tool_router)]
impl WxctlMcpServer {
    /// List every resource kind wxctl supports, optionally filtered by service and
    /// deployment. Read-only: no profile, no network. Mirrors `wxctl resources`.
    #[tool(
        name = "wxctl_list_resource_kinds",
        description = "List wxctl resource kinds (kind, service, deployment support, summary). Optional filters: service (e.g. watsonx_data), deployment (saas|software). No profile or network needed.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn wxctl_list_resource_kinds(&self, Parameters(input): Parameters<ListResourceKindsInput>) -> Result<Json<ListResourceKindsOutput>, String> {
        list_resource_kinds(&input).map(Json)
    }

    /// Full field/dependency/endpoint descriptor for one kind — the exact JSON
    /// `wxctl explain -o json` emits. Read-only: no profile, no network.
    #[tool(name = "wxctl_explain_kind", description = "Explain one wxctl resource kind: fields, types, defaults, enums, validation, nested sub-fields, dependencies, and endpoints. Input: { kind }. No profile or network needed.", annotations(read_only_hint = true, destructive_hint = false))]
    pub async fn wxctl_explain_kind(&self, Parameters(input): Parameters<ExplainKindInput>) -> Result<Json<ExplainKindOutput>, String> {
        let view = explain_kind(&input)?;
        let value = serde_json::to_value(view).map_err(|e| format!("serialization error: {e}"))?;
        Ok(Json(ExplainKindOutput(value)))
    }

    /// Validate a config against the selected profile's schemas + deployment constraints.
    /// A validation that finds problems returns `{ valid: false, errors }` as a successful
    /// (non-error) tool result so the host can read and self-correct.
    #[tool(
        name = "wxctl_validate",
        description = "Validate a wxctl config. Input: exactly one of { config } (inline YAML) or { config_path } (file path), plus optional skip_post_validate:true to skip source-file-existence checks (config-tier / pre-scaffold validation). Returns { valid, errors, warnings }. Finding validation errors is a successful result (not a tool failure). warnings are non-blocking advisories (e.g. an orphaned one-sided cross-service bridge); they never change valid.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn wxctl_validate(&self, Parameters(input): Parameters<ValidateInput>) -> Result<Json<ValidateOutput>, String> {
        let mut config = input.config_input.load_deployable()?;
        let config_yaml = serde_norway::to_string(&config).map_err(|e| format!("could not serialize config for fix prompt: {e}"))?;
        let client = self.live_client().await?;
        let result = client.validate_with(&mut config, input.skip_post_validate).await.map_err(|e| format!("validation failed: {e}"))?;
        let advisories = wxctl_engine::bridge_advisories(&result, client.profile_deployment().as_ref());
        let result = result.with_advisories(advisories);
        Ok(Json(shape_validation(&result, &config_yaml)))
    }

    /// Plan a config against the live profile and return a trimmed create/update/delete/
    /// no-change summary plus the operation list. Raw payloads only with `verbose: true`.
    #[tool(
        name = "wxctl_plan",
        description = "Plan a wxctl config: reconcile against the live environment and return a create/update/delete/no-change summary + operation list. Input: exactly one of { config } or { config_path }, plus optional verbose:true for raw payloads.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn wxctl_plan(&self, Parameters(input): Parameters<PlanInput>) -> Result<Json<PlanOutput>, String> {
        let mut config = input.config_input.load_deployable()?;
        let client = self.live_client().await?;
        let plan = client.plan(&mut config).await.map_err(|e| format!("plan failed: {e}"))?;
        Ok(Json(plan_output(&plan, input.verbose)))
    }

    #[tool(name = "runs_list", description = "List wxctl run records (newest first): run_id, command, started, outcome (success|failed|aborted|unknown), error_count. Produced by wxctl_apply/destroy/test. Read-only; no profile needed.", annotations(read_only_hint = true, destructive_hint = false))]
    pub async fn runs_list(&self, Parameters(input): Parameters<RunsListInput>) -> Result<Json<RunsListOutput>, String> {
        runs_list(&input).map(Json)
    }

    #[tool(name = "run_get", description = "Get one run record's manifest: command, profile, outcome, counts, config_paths, and the error index [{code, resource, message, fix}]. Input: { run_id }. Read-only.", annotations(read_only_hint = true, destructive_hint = false))]
    pub async fn run_get(&self, Parameters(input): Parameters<RunGetInput>) -> Result<Json<RunGetOutput>, String> {
        run_get(&input).map(Json)
    }

    #[tool(
        name = "run_events_query",
        description = "Query a run's events.jsonl, optionally filtered. Input: { run_id, level?, target?, resource?, span?, limit? }. Returns matching events ({ts, level, target, span, src?, fields}). Read-only.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn run_events_query(&self, Parameters(input): Parameters<RunEventsQueryInput>) -> Result<Json<RunEventsQueryOutput>, String> {
        run_events_query(&input).map(Json)
    }

    #[tool(
        name = "run_diagnose",
        description = "Diagnose a failed run into an agent-ready bundle: per-error code, cause, fix, triage class, failing redacted exchange, preceding decisions, matched troubleshoot docs, and a fix-instructions tail. Input: { run_id? } (defaults to latest failed run). Read-only.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn run_diagnose(&self, Parameters(input): Parameters<RunDiagnoseInput>) -> Result<Json<RunDiagnoseOutput>, String> {
        run_diagnose(&input).map(Json)
    }
}

#[tool_router(router = mutating_tool_router)]
impl WxctlMcpServer {
    /// Apply a config: provision resources against the live profile. Requires
    /// `confirm: true` (run `wxctl_plan` first). Streams progress; honors cancellation.
    #[tool(
        name = "wxctl_apply",
        description = "Apply a wxctl config — provisions real resources. PRECONDITION: run wxctl_plan first and confirm it is error-free, then re-call with confirm:true. Input: { config } or { config_path }, confirm:true, optional verbose:true. Returns succeeded/failed/skipped summary + run_id (pass to run_diagnose on failure).",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    pub async fn wxctl_apply(&self, Parameters(input): Parameters<ExecuteInput>, peer: Peer<RoleServer>, meta: Meta, cancel: CancellationToken) -> Result<Json<ExecuteOutput>, String> {
        self.run_mutation(MutationOp::Apply, input, peer, meta, cancel).await
    }

    /// Destroy a config: remove resources from the live profile. Requires `confirm: true`.
    /// Streams progress; honors cancellation. Destructive.
    #[tool(
        name = "wxctl_destroy",
        description = "Destroy a wxctl config — removes real resources. Requires confirm:true (run wxctl_plan first, review, then re-call with confirm:true). Input: { config } or { config_path }, confirm:true, optional verbose:true. Returns succeeded/failed/skipped summary + run_id (pass to run_diagnose on failure).",
        annotations(read_only_hint = false, destructive_hint = true)
    )]
    pub async fn wxctl_destroy(&self, Parameters(input): Parameters<ExecuteInput>, peer: Peer<RoleServer>, meta: Meta, cancel: CancellationToken) -> Result<Json<ExecuteOutput>, String> {
        self.run_mutation(MutationOp::Destroy, input, peer, meta, cancel).await
    }

    /// Run a config's `kind: test` suite against the live profile (chats deployed
    /// agents/deployments/flows and checks expectations). Streams per-test progress.
    #[tool(
        name = "wxctl_test",
        description = "Run the kind:test suite in a wxctl config against the live profile (chats deployed agents/deployments/flows, checks tool/answer expectations). Input: exactly one of { config } or { config_path }. Pass the FULL config — the real resources together with the appended kind:test documents — NOT the kind:test documents alone: test resolves the `${kind.ref}` references in the suite (e.g. the agent under test) by discovering the real resources declared in the same config, so a test-only config fails with 'not found in store'. (Non-test resources are filtered automatically; harmless to include.) Returns per-test pass/fail + per-turn outcomes + run_id (pass to run_diagnose on failure).",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    // NOTE: asymmetry with wxctl_apply/wxctl_destroy — wxctl_test takes no CancellationToken because
    // the SDK's `test_with_observer` has no cancel parameter (adding one would require editing wxctl-sdk).
    pub async fn wxctl_test(&self, Parameters(input): Parameters<ConfigInput>, peer: Peer<RoleServer>, meta: Meta) -> Result<Json<TestOutput>, String> {
        let mut config = input.load()?;
        let client = self.live_client().await?;
        let scope = McpRunScope::begin("test", &self.profile, input.scope_paths(), self.full_trace);
        // Record the run's deployment scope now that the live client (and its resolved
        // profile) is available — mirrors `CommandContext::setup_with_render`'s recording.
        let deployment = client.profile_deployment().unwrap_or(wxctl_core::types::Deployment::Saas);
        scope.set_deployment(Some(deployment.flavor().to_string()));
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<ProgressEvent>();
        spawn_progress_drain(peer, meta.get_progress_token(), rx);
        let observer = Arc::new(ProgressTestObserver::new(tx));
        let result = client.test_with_observer(&mut config, observer).instrument(scope.span()).await;
        match result {
            Ok(results) => {
                let outcome = if results.failed == 0 { "success" } else { "failed" };
                scope.finish(outcome);
                Ok(Json(test_output(scope.run_id.clone(), &results)))
            }
            Err(e) => {
                scope.finish("failed");
                Err(format!("test failed: {e} (run_id: {})", scope.run_id))
            }
        }
    }
}

#[tool_router(router = compose_tool_router)]
impl WxctlMcpServer {
    #[tool(
        name = "compose_start",
        description = "Compose orchestrator: entry point for the natural-language → config → apply flow. Input: { use_case, deployment? (saas|software), max_tier? (config|deploy) — config returns only the five authoring steps, default deploy returns all nine }. Returns { recipe (ordered steps: identify→paths→generate→validate/fix→scaffold→plan→apply→test), identify_prompt (ready-to-run Pass-1 prompt), fix_loop (max_iterations:3), gates (error-free-plan-before-apply) }. Pure compute, no profile/network. Run the recipe with compose_paths/compose_prompt/wxctl_validate/compose_scaffold/wxctl_plan/wxctl_apply/wxctl_test.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn compose_start(&self, Parameters(input): Parameters<ComposeStartInput>) -> Result<Json<ComposeStartOutput>, String> {
        run_start(&input).map(Json)
    }

    #[tool(
        name = "compose_paths",
        description = "Compose pipeline Pass 2: resolve dependencies + bridges to the recommended deployment path. Input: { config (resource-list or partial-config YAML), deployment? (saas|software) }. Returns { paths_yaml } (compose/v1).",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn compose_paths(&self, Parameters(input): Parameters<ComposePathsInput>) -> Result<Json<PathsOutputDto>, String> {
        run_paths(&input).map(Json)
    }

    #[tool(
        name = "compose_prompt",
        description = "Compose pipeline prompt assembly. Config mode: { input, paths?, existing_resources? } — existing_resources is a pre-rendered \"files already exist\" block injected verbatim (the caller renders it; this tool performs no FS scan). Implementation mode: { scaffold_dir, input?, config? }. Test mode: { config, test_config:true, input? }. Data mode: { config, data_config:true, input? } — returns the generic data-generation prompt for the config's detected data needs. Returns { prompt }.",
        annotations(read_only_hint = true, destructive_hint = false)
    )]
    pub async fn compose_prompt(&self, Parameters(input): Parameters<ComposePromptInput>) -> Result<Json<PromptOutput>, String> {
        run_prompt(&input).map(Json)
    }
}

#[tool_router(router = compose_mutating_tool_router)]
impl WxctlMcpServer {
    #[tool(
        name = "compose_scaffold",
        description = "Compose pipeline: materialize every source file a config references (python tool stubs, KB docs, WML score, OpenAPI/flow specs, FastMCP server). Input: { config, output_dir?, dry_run? }. OMIT output_dir (recommended) to write into the canonical in-cwd dir <cwd>/.wxctl-scaffold/<ref_name>/ and get back a `config` with source paths rewritten to match — use that returned config for plan/apply/test. If output_dir is set it must resolve inside cwd (out-of-cwd errors); stub files are rebased by filename under it (legacy) and `config` comes back empty. Returns { manifest, failed, created, skipped, failed_count, config, config_unchanged }. config_unchanged:true means there was nothing to rewrite (the config references no source files, or legacy output_dir mode) — keep using your original config; false means use the returned `config`.",
        annotations(read_only_hint = false, destructive_hint = false)
    )]
    pub async fn compose_scaffold(&self, Parameters(input): Parameters<ComposeScaffoldInput>) -> Result<Json<ScaffoldOutputDto>, String> {
        run_scaffold(&input).map(Json)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for WxctlMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("wxctl-mcp", env!("CARGO_PKG_VERSION")))
            .with_instructions("wxctl MCP server. To build + deploy from a natural-language use case, start with compose_start (returns an ordered recipe + a ready-to-run identification prompt + a bounded fix loop + the error-free-plan-before-apply gate); then drive the 9-step recipe — config tier (pure compute): compose_paths → compose_prompt → wxctl_validate (inline fix_prompt on failure, ≤3 iterations; pass skip_post_validate:true pre-scaffold) → compose_prompt(test_config:true) for the kind:test suite; then deploy tier (needs a local profile + filesystem): compose_scaffold → wxctl_plan (must be error-free) → wxctl_apply (confirm:true) → wxctl_test. This wxctl-mcp server runs the full config + deploy flow: the config tier is pure compute (no profile or filesystem) and the deploy tier provisions against a local profile. Discovery: wxctl_list_resource_kinds, wxctl_explain_kind. Author + check: wxctl_validate (valid:false returns a fix_prompt — a successful result, not an error) and returns a warnings array of non-blocking advisories. Preview: wxctl_plan. Mutate: review the error-free wxctl_plan diff, then wxctl_apply / wxctl_destroy with confirm:true (provision/remove real resources). Compose sub-tools (pure compute, no profile): compose_paths (Pass 2), compose_prompt (config/implementation/test prompt assembly), compose_scaffold (materialize source stubs — absent on --read-only). Mutating tools (apply/destroy/test/scaffold) are absent on a --read-only server. Run records: wxctl_apply/destroy/test return a run_id; on failure call run_diagnose with it (or omit for the latest failed run). Use runs_list, run_get, run_events_query to inspect run records.")
    }
}

/// Run the stdio MCP server to completion. `read_only` gates registration of the four
/// mutating tools (apply/destroy/test/scaffold) — a read-only server exposes only the
/// read-only base + compose-authoring tools. `profile`/`profile_path` select the live profile (built lazily, shared).
/// `full_trace` enables full HTTP-exchange capture in run records for every mutating call.
/// Recursively remove schemars' Rust-numeric `format` markers ("uint", "uint32", "int64",
/// "double", …) from a generated tool schema. They are not JSON Schema spec formats, so
/// ajv-based MCP clients warn `unknown format … ignored in schema` for every occurrence;
/// the numeric constraint itself survives in `type` + `minimum`/`maximum`. Spec-defined
/// formats ("date-time", "uri", …) pass through untouched.
fn strip_rust_numeric_formats(schema: JsonObject) -> JsonObject {
    fn walk(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(map) => {
                if let Some(serde_json::Value::String(format)) = map.get("format")
                    && matches!(format.as_str(), "int" | "int8" | "int16" | "int32" | "int64" | "int128" | "uint" | "uint8" | "uint16" | "uint32" | "uint64" | "uint128" | "float" | "double")
                {
                    map.remove("format");
                }
                map.values_mut().for_each(walk);
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(walk),
            _ => {}
        }
    }
    let mut value = serde_json::Value::Object(schema);
    walk(&mut value);
    match value {
        serde_json::Value::Object(map) => map,
        _ => unreachable!("walk never changes the root variant"),
    }
}

pub async fn serve(profile: &str, profile_path: Option<&str>, read_only: bool, full_trace: bool) -> anyhow::Result<()> {
    let service = WxctlMcpServer::new(profile, profile_path, read_only, full_trace).serve(stdio()).await.inspect_err(|e| tracing::error!(error = ?e, "wxctl-mcp serve error"))?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Guard that the sanitizer isn't dead code: schemars really does stamp `format: "uint"`
    /// on `usize` fields. If this fails, schemars stopped emitting Rust-numeric formats and
    /// `strip_rust_numeric_formats` may be removable.
    #[test]
    fn schemars_still_emits_rust_numeric_formats() {
        let schema = serde_json::to_string(&schemars::schema_for!(wxctl_sdk::json::ExecuteSummary)).unwrap();
        assert!(schema.contains(r#""format":"uint""#), "schemars no longer emits format:uint — reassess the sanitizer: {schema}");
    }

    /// Every `format` in every registered tool's input/output schema is a JSON Schema
    /// spec-defined one, in both modes — an allowlist, so any future non-standard marker
    /// (not just the Rust-numeric set stripped today) fails here instead of shipping and
    /// making ajv-based clients (IBM Bob among them) warn once per field.
    #[test]
    fn tool_schemas_carry_only_spec_defined_formats() {
        const SPEC_FORMATS: &[&str] = &["date-time", "date", "time", "duration", "email", "idn-email", "hostname", "idn-hostname", "ipv4", "ipv6", "uri", "uri-reference", "iri", "iri-reference", "uuid", "uri-template", "json-pointer", "relative-json-pointer", "regex"];
        fn collect_formats(value: &serde_json::Value, found: &mut Vec<String>) {
            match value {
                serde_json::Value::Object(map) => {
                    if let Some(serde_json::Value::String(format)) = map.get("format") {
                        found.push(format.clone());
                    }
                    map.values().for_each(|v| collect_formats(v, found));
                }
                serde_json::Value::Array(items) => items.iter().for_each(|v| collect_formats(v, found)),
                _ => {}
            }
        }
        for read_only in [false, true] {
            let server = WxctlMcpServer::new("nonexistent-profile-for-smoke", None, read_only, false);
            for route in server.tool_router.map.values() {
                for schema in std::iter::once(&route.attr.input_schema).chain(route.attr.output_schema.as_ref()) {
                    let mut found = Vec::new();
                    collect_formats(&serde_json::Value::Object(schema.as_ref().clone()), &mut found);
                    for format in &found {
                        assert!(SPEC_FORMATS.contains(&format.as_str()), "tool {} (read_only={read_only}) ships non-spec format {format:?} — extend strip_rust_numeric_formats: {}", route.attr.name, serde_json::to_string(schema.as_ref()).unwrap());
                    }
                }
            }
        }
    }
}
