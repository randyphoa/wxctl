#![cfg(feature = "live-tests")]

use std::future::Future;
use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};
use tracing::Instrument;
use wxctl_core::Config;
use wxctl_engine::{CompiledPlan, OperationType};
use wxctl_sdk::{TestObserver, WxctlClient};

// ---------------------------------------------------------------------------
// Logging target — every harness-emitted event uses this so a single jq filter
//   jq 'select(.test=="X")' <test-logs.jsonl>   # resolved path printed to stderr at startup
// reconstructs everything the test did.
// ---------------------------------------------------------------------------

const LIVE_TARGET: &str = "wxctl::test::live";

// ---------------------------------------------------------------------------
// Test observer — live progress on stderr
// ---------------------------------------------------------------------------

pub struct StderrTestObserver;

impl TestObserver for StderrTestObserver {
    fn on_test_start(&self, test_name: &str) {
        eprintln!("  ▶ {test_name} ...");
    }

    fn on_test_complete(&self, test_name: &str, passed: bool, completed: usize, total: usize) {
        let icon = if passed { "✓" } else { "✗" };
        eprintln!("  {icon} {test_name} [{completed}/{total}]");
    }
}

// ---------------------------------------------------------------------------
// Tracing
// ---------------------------------------------------------------------------

/// Initialize tracing once per process. Two output sinks, written under Cargo's
/// integration-test scratch dir (`CARGO_TARGET_TMPDIR`, inside `target/`) so they
/// are git-ignored by construction and never land in the source tree:
///
///   - `<CARGO_TARGET_TMPDIR>/debug.json` — engine firehose (target `wxctl=debug`), append mode
///   - `<CARGO_TARGET_TMPDIR>/test-logs.jsonl` — test-level signal (target `wxctl::test::live`), append mode
///
/// Both files persist across multiple tests in the same `cargo test` invocation; the
/// resolved paths are printed to stderr at startup. Stderr also receives a filtered
/// fmt layer (warn+ by default, overridable via `RUST_LOG`).
pub fn init_tracing() {
    use tracing_subscriber::{EnvFilter, Layer, fmt, layer::SubscriberExt, util::SubscriberInitExt};

    let stderr_layer = fmt::layer().json().with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("wxctl=warn")));

    // CARGO_TARGET_TMPDIR is the scratch dir Cargo creates under target/ for integration
    // tests — git-ignored by construction, so these logs never reach the source tree.
    let log_dir = env!("CARGO_TARGET_TMPDIR");
    let _ = std::fs::create_dir_all(log_dir);
    let debug_path = format!("{log_dir}/debug.json");
    let test_log_path = format!("{log_dir}/test-logs.jsonl");

    write_run_separator(&debug_path, &test_log_path);

    let debug_layer = open_append(&debug_path).map(|file| fmt::layer().json().with_writer(std::sync::Mutex::new(file)).with_filter(EnvFilter::new("wxctl=debug")));
    let test_log_layer = open_append(&test_log_path).map(|file| fmt::layer().json().with_writer(std::sync::Mutex::new(file)).with_filter(EnvFilter::new(format!("{LIVE_TARGET}=info"))));

    let _ = tracing_subscriber::registry().with(stderr_layer).with(debug_layer).with(test_log_layer).try_init();
}

fn open_append(path: &str) -> Option<std::fs::File> {
    std::fs::OpenOptions::new().create(true).append(true).open(path).ok()
}

fn write_run_separator(debug_path: &str, test_log_path: &str) {
    use std::io::Write;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        eprintln!("  live-test logs → {debug_path} (firehose), {test_log_path} (test signal)");
        let line = format!("=== run started {} ===\n", chrono::Utc::now().to_rfc3339());
        if let Some(mut f) = open_append(debug_path) {
            let _ = f.write_all(line.as_bytes());
        }
        if let Some(mut f) = open_append(test_log_path) {
            let _ = f.write_all(line.as_bytes());
        }
    });
}

/// Create a top-level span for a test function. Use with `.instrument(...)`.
/// Tests using the `LiveTest` builder don't need this directly.
macro_rules! test_span {
    ($name:expr) => {
        tracing::info_span!(target: "wxctl::test::live", "live_test", test = $name)
    };
}

// ---------------------------------------------------------------------------
// Profile / client setup
// ---------------------------------------------------------------------------

fn test_profiles_path() -> anyhow::Result<PathBuf> {
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot determine home directory"))?;
    Ok(home.join(".wxctl/test_profiles.json"))
}

fn create_test_client_for(profile: &str) -> anyhow::Result<Option<WxctlClient>> {
    let path = test_profiles_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let path_str = path.to_str().ok_or_else(|| anyhow::anyhow!("Invalid test_profiles.json path: {}", path.display()))?;
    match WxctlClient::new(profile, Some(path_str)) {
        Ok(c) => Ok(Some(c)),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("not found") { Ok(None) } else { Err(anyhow::anyhow!("{msg}")) }
        }
    }
}

// ---------------------------------------------------------------------------
// Fixture helpers
// ---------------------------------------------------------------------------

pub fn short_id() -> String {
    uuid::Uuid::new_v4().to_string().replace('-', "")[..8].to_string()
}

/// Read an auxiliary field out of a `test_profiles.json` service entry
/// (e.g. `cos.access_key`, `db2.password`). Returns `Ok(None)` if the
/// profile file or field doesn't exist — callers treat that as a skip.
/// These fields aren't part of `ServiceConfig`, so we read the raw JSON.
pub fn read_profile_field(profile: &str, service: &str, field: &str) -> anyhow::Result<Option<String>> {
    let path = test_profiles_path()?;
    if !path.exists() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(&path)?;
    let json: serde_json::Value = serde_json::from_str(&raw)?;
    let v = json.get("profiles").and_then(|p| p.get(profile)).and_then(|p| p.get(service)).and_then(|p| p.get(field));
    match v {
        Some(serde_json::Value::String(s)) => Ok(Some(s.clone())),
        Some(serde_json::Value::Number(n)) => Ok(Some(n.to_string())),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b.to_string())),
        _ => Ok(None),
    }
}

/// Populate env vars from `test_profiles.json` so ${env:...} references in
/// YAML resolve without the caller having to export anything. Returns the
/// first missing field name if any are absent — tests use that to skip.
pub fn set_env_from_profile(profile: &str, mappings: &[(&str, &str, &str)]) -> anyhow::Result<Option<String>> {
    for (env_name, service, field) in mappings {
        match read_profile_field(profile, service, field)? {
            Some(v) => unsafe { std::env::set_var(env_name, v) },
            None => return Ok(Some(format!("{service}.{field}"))),
        }
    }
    Ok(None)
}

/// Standard COS env-var tuple for `set_env_from_profile` — the full set a
/// test needs to materialize a `storage_connection` + `s3_bucket` pair
/// against IBM COS.
pub const COS_ENV_MAPPINGS: &[(&str, &str, &str)] =
    &[("WXCTL_TEST_COS_ACCESS_KEY", "cos", "access_key"), ("WXCTL_TEST_COS_SECRET_KEY", "cos", "secret_key"), ("WXCTL_TEST_COS_ENDPOINT", "cos", "endpoint"), ("WXCTL_TEST_COS_BUCKET", "cos", "bucket_name"), ("WXCTL_TEST_COS_CRN", "cos", "cos_instance_crn")];

pub fn load_fixture(name: &str, test_id: &str) -> String {
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let path = format!("{}/tests/fixtures/{}", manifest_dir, name);
    let content = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("Failed to read fixture {}: {}", path, e));
    content.replace("__TEST_ID__", test_id).replace("__MANIFEST_DIR__", manifest_dir)
}

pub fn strip_test_resources(yaml: &str) -> String {
    yaml.split("\n---\n")
        .filter(|doc| {
            !doc.lines().any(|line| {
                let trimmed = line.trim();
                trimmed == "kind: test" || trimmed.starts_with("kind: test ")
            })
        })
        .collect::<Vec<_>>()
        .join("\n---\n")
}

// ---------------------------------------------------------------------------
// LiveTest builder — owns scaffolding + structured logging contract.
// ---------------------------------------------------------------------------

pub struct LiveTest {
    name: &'static str,
    profile: &'static str,
    timeout: Duration,
    yaml: Option<String>,
    update: Option<(String, String)>,
    expected_resources: Option<usize>,
    skip_idempotency: bool,
    skip_destroyed_check: bool,
    guard_yaml: Option<String>,
}

impl LiveTest {
    pub fn new(name: &'static str) -> Self {
        Self { name, profile: "test", timeout: Duration::from_secs(300), yaml: None, update: None, expected_resources: None, skip_idempotency: false, skip_destroyed_check: false, guard_yaml: None }
    }

    pub fn profile(mut self, p: &'static str) -> Self {
        self.profile = p;
        self
    }

    pub fn timeout(mut self, secs: u64) -> Self {
        self.timeout = Duration::from_secs(secs);
        self
    }

    pub fn yaml(mut self, y: impl Into<String>) -> Self {
        self.yaml = Some(y.into());
        self
    }

    pub fn update(mut self, from: impl Into<String>, to: impl Into<String>) -> Self {
        self.update = Some((from.into(), to.into()));
        self
    }

    /// Assert `succeeded.len() == n` after the create phase. Honored by `run_crud` only;
    /// `.run(...)` callers should call `ctx.expect_eq_usize(...)` directly.
    pub fn expect_resources(mut self, n: usize) -> Self {
        self.expected_resources = Some(n);
        self
    }

    pub fn skip_destroyed_check(mut self) -> Self {
        self.skip_destroyed_check = true;
        self
    }

    /// Skip the post-create / post-update plan-all-noop checks. Use for resources
    /// whose scoping params are template refs (engine defers them as Create on plan).
    pub fn skip_idempotency(mut self) -> Self {
        self.skip_idempotency = true;
        self
    }

    /// YAML for the cleanup guard if it should differ from `yaml()` (e.g. multi-doc fixture
    /// where `kind: test` resources should not be destroyed).
    pub fn guard_yaml(mut self, y: impl Into<String>) -> Self {
        self.guard_yaml = Some(y.into());
        self
    }

    /// Standard CRUD lifecycle: create → idempotency → [update → idempotency →] destroy → verify_destroyed.
    pub async fn run_crud(self) -> anyhow::Result<()> {
        let yaml = self.yaml.clone().expect("LiveTest::run_crud requires .yaml(...)");
        let update = self.update.clone();
        let expected = self.expected_resources;
        let skip_idempotency = self.skip_idempotency;
        let skip_destroyed = self.skip_destroyed_check;

        self.run(move |ctx| async move {
            ctx.phase("create", async {
                let result = ctx.apply("create", &yaml).await?;
                if let Some(n) = expected {
                    ctx.expect_eq_usize("create", "expected_resources", n, result.succeeded.len())?;
                }
                Ok(())
            })
            .await?;

            if !skip_idempotency {
                ctx.phase("idempotency", async { ctx.assert_plan_all_noop(&yaml).await }).await?;
            }

            if let Some((from, to)) = update.as_ref() {
                let updated_yaml = yaml.replace(from, to);
                ctx.phase("update", async { ctx.apply("update", &updated_yaml).await.map(|_| ()) }).await?;
                if !skip_idempotency {
                    ctx.phase("idempotency", async { ctx.assert_plan_all_noop(&updated_yaml).await }).await?;
                }
            }

            ctx.phase("destroy", async { ctx.destroy("destroy", &yaml).await.map(|_| ()) }).await?;

            if !skip_destroyed {
                ctx.phase("verify_destroyed", async { ctx.assert_destroyed(&yaml).await }).await?;
            }

            Ok(())
        })
        .await
    }

    /// Escape hatch for tests that don't fit the CRUD shape. The closure receives a
    /// `LiveCtx` exposing the client and the structured-logging helpers (`phase`, `expect`,
    /// `retry`, `assert_*`). The harness still owns tracing init, env gate, guard, timeout,
    /// and summary emission.
    pub async fn run<F, Fut>(self, body: F) -> anyhow::Result<()>
    where
        F: FnOnce(Arc<LiveCtx>) -> Fut + Send,
        Fut: Future<Output = anyhow::Result<()>> + Send,
    {
        init_tracing();
        let test_id = short_id();
        let started = Instant::now();

        let setup_start = Instant::now();
        let client = match create_test_client_for(self.profile) {
            Ok(Some(c)) => c,
            Ok(None) => {
                emit_skip(self.name, self.profile, &format!("profile '{}' not configured in test_profiles.json", self.profile));
                eprintln!("SKIP {}: profile '{}' not configured", self.name, self.profile);
                return Ok(());
            }
            Err(e) => {
                emit_summary(self.name, started.elapsed(), Cleanup::NotApplicable, false, &format!("setup failed: {e}"));
                return Err(e);
            }
        };
        emit_phase_ok(self.name, "setup", setup_start.elapsed(), &format!("profile={} id={}", self.profile, test_id));

        let guard_yaml = self.guard_yaml.clone().or_else(|| self.yaml.clone());
        let mut guard = guard_yaml.as_ref().map(|y| TestGuard::with_profile(y.clone(), self.profile).for_test(self.name));

        let ctx = Arc::new(LiveCtx { test: self.name, profile: self.profile, test_id: test_id.clone(), client });
        let span = test_span!(self.name);

        let body_result: anyhow::Result<()> = match tokio::time::timeout(self.timeout, body(ctx).instrument(span)).await {
            Ok(r) => r,
            Err(_) => {
                emit_phase_fail(self.name, "timeout", self.timeout, &FailFields::msg(format!("test exceeded {}s", self.timeout.as_secs())));
                Err(anyhow::anyhow!("{} timed out after {}s", self.name, self.timeout.as_secs()))
            }
        };

        // Run cleanup explicitly inside the test runtime when the body failed, so the
        // summary event can carry the final cleanup state. Disarm the guard either way;
        // it stays armed only for panic safety (in which case Drop reports it separately).
        let cleanup = match (&body_result, guard_yaml.as_ref()) {
            (Ok(_), Some(_)) => {
                if let Some(g) = guard.as_mut() {
                    g.disarm();
                }
                Cleanup::Disarmed
            }
            (Ok(_), None) | (Err(_), None) => Cleanup::NotApplicable,
            (Err(_), Some(yaml)) => {
                if let Some(g) = guard.as_mut() {
                    g.disarm();
                }
                run_cleanup(self.name, self.profile, yaml).await
            }
        };

        let summary_msg = match &body_result {
            Ok(_) => String::new(),
            Err(e) => format!("{e}"),
        };
        emit_summary(self.name, started.elapsed(), cleanup, body_result.is_ok(), &summary_msg);

        body_result
    }
}

// ---------------------------------------------------------------------------
// LiveCtx — passed into test bodies. Holds the client and exposes structured-
// logging helpers. Cheap to clone via Arc; closures may capture it freely.
// ---------------------------------------------------------------------------

pub struct LiveCtx {
    pub test: &'static str,
    pub profile: &'static str,
    pub test_id: String,
    pub client: WxctlClient,
}

impl LiveCtx {
    /// Run an async block as a named phase. Emits `start` / `ok` / `fail` events with
    /// duration and (on failure) HTTP trace_id / status if extractable from the error chain.
    pub async fn phase<T, Fut>(&self, name: &'static str, fut: Fut) -> anyhow::Result<T>
    where
        Fut: Future<Output = anyhow::Result<T>>,
    {
        emit_phase_start(self.test, name);
        let start = Instant::now();
        match fut.await {
            Ok(v) => {
                emit_phase_ok(self.test, name, start.elapsed(), "");
                Ok(v)
            }
            Err(e) => {
                let fields = FailFields::from_error(&e);
                emit_phase_fail(self.test, name, start.elapsed(), &fields);
                Err(e)
            }
        }
    }

    /// Bail with a structured `fail` event if `cond` is false.
    pub fn expect(&self, phase: &'static str, cond: bool, expected: impl std::fmt::Display, actual: impl std::fmt::Display) -> anyhow::Result<()> {
        if cond {
            return Ok(());
        }
        let expected_s = expected.to_string();
        let actual_s = actual.to_string();
        tracing::error!(target: LIVE_TARGET, test = self.test, phase, event = "fail", expected = %expected_s, actual = %actual_s, "assertion failed");
        anyhow::bail!("[{phase}] expected {expected_s}, got {actual_s}");
    }

    pub fn expect_eq_usize(&self, phase: &'static str, what: &'static str, expected: usize, actual: usize) -> anyhow::Result<()> {
        self.expect(phase, expected == actual, format!("{what}={expected}"), format!("{what}={actual}"))
    }

    /// Standard "no failed operations" assertion. Emits a `fail` event listing the first failure
    /// (kind, ref_name, error, trace_id) when the result has any failures.
    pub fn expect_no_failures(&self, phase: &'static str, failed: &[wxctl_engine::ExecutionResult]) -> anyhow::Result<()> {
        if failed.is_empty() {
            return Ok(());
        }
        let first = &failed[0];
        let err = first.error.as_deref().unwrap_or("unknown error");
        let trace = parse_trace_id(err);
        tracing::error!(
            target: LIVE_TARGET,
            test = self.test,
            phase,
            event = "fail",
            failed_count = failed.len(),
            kind = %first.key.kind,
            ref_name = %first.key.name,
            trace_id = trace.as_deref().unwrap_or(""),
            "{} resource(s) failed: {}",
            failed.len(),
            err
        );
        anyhow::bail!("[{phase}] {} resource(s) failed; first: {}/{}: {}", failed.len(), first.key.kind, first.key.name, err);
    }

    /// Apply YAML and assert no failures. Returns the full result for further inspection.
    /// Replaces the 3-line `Config::from_yaml` + `client.apply` + `expect_no_failures` ritual.
    pub async fn apply(&self, phase: &'static str, yaml: &str) -> anyhow::Result<wxctl_engine::ExecutionResults> {
        let mut config = Config::from_yaml(yaml)?;
        let result = self.client.apply(&mut config).await.map_err(into_anyhow)?;
        self.expect_no_failures(phase, &result.failed)?;
        Ok(result)
    }

    /// Destroy YAML and assert no failures. Returns the full result for further inspection.
    pub async fn destroy(&self, phase: &'static str, yaml: &str) -> anyhow::Result<wxctl_engine::ExecutionResults> {
        let mut config = Config::from_yaml(yaml)?;
        let result = self.client.destroy(&mut config).await.map_err(into_anyhow)?;
        self.expect_no_failures(phase, &result.failed)?;
        Ok(result)
    }

    /// Plan YAML and return the compiled plan for assertion (no event emitted on its own).
    pub async fn plan(&self, yaml: &str) -> anyhow::Result<CompiledPlan> {
        let mut config = Config::from_yaml(yaml)?;
        self.client.plan(&mut config).await.map_err(into_anyhow)
    }

    /// Retry an async attempt that returns `Ok(Some(v))` on success or `Ok(None)` to retry.
    /// Emits one `retry` event per attempt; final failure emits a `fail` event.
    pub async fn retry<T, A, Fut>(&self, phase: &'static str, max_attempts: u32, delay: Duration, mut attempt: A) -> anyhow::Result<T>
    where
        A: FnMut(u32) -> Fut,
        Fut: Future<Output = anyhow::Result<Option<T>>>,
    {
        for n in 1..=max_attempts {
            match attempt(n).await {
                Ok(Some(v)) => return Ok(v),
                Ok(None) => {
                    tracing::info!(target: LIVE_TARGET, test = self.test, phase, event = "retry", attempt = n, max_attempts, delay_ms = delay.as_millis() as u64, "still pending");
                    if n < max_attempts {
                        tokio::time::sleep(delay).await;
                    }
                }
                Err(e) => {
                    let fields = FailFields::from_error(&e);
                    emit_phase_fail(self.test, phase, Duration::ZERO, &fields);
                    return Err(e);
                }
            }
        }
        tracing::error!(target: LIVE_TARGET, test = self.test, phase, event = "fail", attempt = max_attempts, max_attempts, "retry exhausted");
        anyhow::bail!("[{phase}] retry exhausted after {max_attempts} attempts")
    }

    /// Assert that `plan()` returns NoOp for every resource. Listing differences in the fail event.
    pub async fn assert_plan_all_noop(&self, yaml: &str) -> anyhow::Result<()> {
        let mut config = Config::from_yaml(yaml)?;
        let plan = self.client.plan(&mut config).await.map_err(into_anyhow)?;
        let mismatches: Vec<String> = plan.operations.iter().filter(|op| !matches!(op.op_type, OperationType::NoOp)).map(|op| format!("{}/{}={:?}", op.key.kind, op.key.name, op.op_type)).collect();
        if mismatches.is_empty() {
            return Ok(());
        }
        tracing::error!(target: LIVE_TARGET, test = self.test, phase = "idempotency", event = "fail", mismatch_count = mismatches.len(), first_mismatch = %mismatches[0], "expected all NoOp");
        anyhow::bail!("idempotency: {} mismatch(es); first: {}", mismatches.len(), mismatches[0]);
    }

    /// Assert all resources in the YAML no longer exist (plan shows Create for each).
    /// Retries up to 6 × 5s for async-deletion APIs (e.g. spaces).
    pub async fn assert_destroyed(&self, yaml: &str) -> anyhow::Result<()> {
        self.retry("verify_destroyed", 6, Duration::from_secs(5), |_attempt| {
            let yaml = yaml.to_string();
            async move {
                let mut config = Config::from_yaml(&yaml)?;
                let plan = self.client.plan(&mut config).await.map_err(into_anyhow)?;
                let all_gone = plan.operations.iter().all(|op| matches!(op.op_type, OperationType::Create));
                Ok(if all_gone { Some(()) } else { None })
            }
        })
        .await
    }
}

// ---------------------------------------------------------------------------
// Free-function plan assertions used by tests that need to inspect a CompiledPlan
// directly (e.g., to assert a specific resource's op_type or update fields).
// For full-plan idempotency checks and destroyed verification, prefer the
// `LiveCtx::assert_plan_all_noop` / `assert_destroyed` methods which emit
// structured fail events into `test-logs.jsonl`.
// ---------------------------------------------------------------------------

pub fn assert_plan_op_type(plan: &CompiledPlan, kind: &str, name_prefix: &str, expected: &str) {
    let op = plan.operations.iter().find(|op| &*op.key.kind == kind && op.key.name.starts_with(name_prefix)).unwrap_or_else(|| panic!("No operation for {kind}/{name_prefix} in plan"));
    let actual = format!("{}", op.op_type);
    assert_eq!(actual, expected, "Expected {expected} for {kind}/{name_prefix}, got {actual}");
}

pub fn assert_plan_update_fields(plan: &CompiledPlan, kind: &str, name_prefix: &str, expected_fields: &[&str]) {
    let op = plan.operations.iter().find(|op| &*op.key.kind == kind && op.key.name.starts_with(name_prefix)).unwrap_or_else(|| panic!("No operation for {kind}/{name_prefix} in plan"));
    match &op.op_type {
        OperationType::Update { fields } => {
            for expected in expected_fields {
                assert!(fields.iter().any(|f| f == expected), "Expected field '{expected}' in update fields {fields:?} for {kind}/{name_prefix}");
            }
        }
        other => panic!("Expected Update for {kind}/{name_prefix}, got {other}"),
    }
}

// ---------------------------------------------------------------------------
// Shared lifecycles for tests that share identical scaffolding except for the
// fixture name (`*_invocation.rs`) or the source content (`*_source_change.rs`).
// ---------------------------------------------------------------------------

pub async fn run_e2e_test(fixture_name: &'static str, expected_resources: usize, expected_tests: usize) -> anyhow::Result<()> {
    let yaml = load_fixture(fixture_name, &short_id());
    let real_yaml = strip_test_resources(&yaml);

    LiveTest::new(leak_str(format!("e2e_{fixture_name}")))
        .timeout(600)
        .guard_yaml(real_yaml.clone())
        .run(move |ctx| async move {
            ctx.phase("create", async {
                let result = ctx.apply("create", &real_yaml).await?;
                ctx.expect_eq_usize("create", "expected_resources", expected_resources, result.succeeded.len())?;
                Ok(())
            })
            .await?;

            ctx.phase("test_cases", async {
                let mut test_config = Config::from_yaml(&yaml)?;
                let observer: Arc<dyn TestObserver> = Arc::new(StderrTestObserver);
                let results = ctx.client.test_with_observer(&mut test_config, observer).await.map_err(into_anyhow)?;
                ctx.expect_eq_usize("test_cases", "total", expected_tests, results.total())?;
                ctx.expect_eq_usize("test_cases", "passed", expected_tests, results.passed)?;
                ctx.expect_eq_usize("test_cases", "failed", 0, results.failed)?;
                Ok(())
            })
            .await?;

            ctx.phase("destroy", async { ctx.destroy("destroy", &real_yaml).await.map(|_| ()) }).await?;

            ctx.phase("verify_destroyed", async { ctx.assert_destroyed(&real_yaml).await }).await?;

            Ok(())
        })
        .await
}

pub async fn run_source_change_test(kind: &'static str, kind_short: &'static str, source_file: &str, initial_source: &str, mutated_source: &str, yaml_template: &str) -> anyhow::Result<()> {
    let safe_id = short_id();

    let temp_dir = tempfile::tempdir()?;
    let source_path = temp_dir.path().join(source_file);
    std::fs::write(&source_path, initial_source)?;

    let source_path_str = source_path.to_string_lossy().to_string();
    let resource_yaml = yaml_template.replace("{safe_id}", &safe_id).replace("{source_path_str}", &source_path_str);

    let yaml = format!(
        r#"
kind: space
ref_name: wxctl_test_{kind_short}_{safe_id}
name: wxctl-test-{kind_short}-{safe_id}
type: wx
---
kind: software_specification
ref_name: wxctl_test_{kind_short}_swspec_{safe_id}
name: wxctl-test-{kind_short}-swspec-{safe_id}
base_software_specification: runtime-25.1-py3.12
space_id: ${{space.wxctl_test_{kind_short}_{safe_id}}}
---
{resource_yaml}
"#
    );

    let test_name = leak_str(format!("test_{kind}_source_change"));

    LiveTest::new(test_name)
        .timeout(600)
        .yaml(yaml.clone())
        .run(move |ctx| async move {
            let mutated_source = mutated_source.to_string();

            ctx.phase("create", async {
                let result = ctx.apply("create", &yaml).await?;
                ctx.expect_eq_usize("create", "expected_resources", 3, result.succeeded.len())?;
                Ok(())
            })
            .await?;

            ctx.phase("idempotency", async {
                let result = ctx.apply("idempotency", &yaml).await?;
                let op = result.succeeded.iter().find(|r| &*r.key.kind == kind);
                ctx.expect("idempotency", op.is_none() || matches!(op.unwrap().operation, OperationType::NoOp), "NoOp", format!("{:?}", op.map(|o| &o.operation)))?;
                Ok(())
            })
            .await?;

            ctx.phase("mutate_source", async {
                std::fs::write(&source_path, &mutated_source)?;
                Ok(())
            })
            .await?;

            ctx.phase("update", async {
                let result = ctx.apply("update", &yaml).await?;
                let op = result.succeeded.iter().find(|r| &*r.key.kind == kind);
                ctx.expect("update", op.map(|o| matches!(o.operation, OperationType::Update { .. })).unwrap_or(false), "Update", format!("{:?}", op.map(|o| &o.operation)))?;
                Ok(())
            })
            .await?;

            ctx.phase("destroy", async { ctx.destroy("destroy", &yaml).await.map(|_| ()) }).await?;

            Ok(())
        })
        .await
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Turn a runtime String into a &'static str. Acceptable here because each
/// LiveTest is a single test invocation; the small leak is bounded by the
/// number of tests in the binary (<100).
fn leak_str(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Convert `wxctl_sdk` / engine errors into anyhow, threading through the original message.
fn into_anyhow<E: std::fmt::Display>(e: E) -> anyhow::Error {
    anyhow::anyhow!("{e}")
}

#[derive(Default)]
struct FailFields {
    msg: String,
    trace_id: Option<String>,
    http_status: Option<u16>,
}

impl FailFields {
    fn msg(s: String) -> Self {
        Self { msg: s, ..Default::default() }
    }

    /// Walk the error chain (display strings) and try to lift trace_id / http_status
    /// out of stringified API error context emitted by the http client.
    fn from_error(e: &anyhow::Error) -> Self {
        let chain: Vec<String> = std::iter::once(format!("{e}")).chain(e.chain().skip(1).map(|c| format!("{c}"))).collect();
        let joined = chain.join(" | ");

        let trace_id = parse_trace_id(&joined);
        let http_status = parse_http_status(&joined);

        Self { msg: chain[0].clone(), trace_id, http_status }
    }
}

fn parse_trace_id(s: &str) -> Option<String> {
    // http client emits "[trace_id=<id>]" suffixes via wxctl_core::logging::extract_trace_id
    let marker = "trace_id=";
    let i = s.find(marker)?;
    let rest = &s[i + marker.len()..];
    let end = rest.find([']', ' ', ',', '|']).unwrap_or(rest.len());
    let id = rest[..end].trim();
    if id.is_empty() { None } else { Some(id.to_string()) }
}

fn parse_http_status(s: &str) -> Option<u16> {
    // matches "HTTP 503", "status: 500", "[status=429]" — try the most explicit first
    for prefix in ["HTTP ", "status=", "status: "] {
        if let Some(i) = s.find(prefix) {
            let rest = &s[i + prefix.len()..];
            let end = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
            if end >= 3
                && let Ok(n) = rest[..3].parse::<u16>()
                && (100..600).contains(&n)
            {
                return Some(n);
            }
        }
    }
    None
}

/// Cleanup state recorded on the `summary` event. `Disarmed` = test passed and the
/// guard was disarmed. `Cleaned`/`Leaked`/`Failed` = test failed and `run_cleanup`
/// ran inline; the value reflects the outcome. `NotApplicable` = no guard yaml.
#[derive(Copy, Clone)]
enum Cleanup {
    Disarmed,
    Cleaned,
    Leaked,
    Failed,
    NotApplicable,
}

impl Cleanup {
    fn as_str(self) -> &'static str {
        match self {
            Cleanup::Disarmed => "disarmed",
            Cleanup::Cleaned => "cleaned",
            Cleanup::Leaked => "leaked",
            Cleanup::Failed => "failed",
            Cleanup::NotApplicable => "n_a",
        }
    }
}

/// Run cleanup inline (not in `TestGuard::Drop`), so the resulting state is known
/// when the harness emits its `summary` event. Same retry policy as the guard:
/// up to 6 × 15s while the reconciler reports "nothing to delete" — some
/// watsonx.data list endpoints exclude resources still in PROVISIONING.
async fn run_cleanup(test: &'static str, profile: &'static str, yaml: &str) -> Cleanup {
    let client = match create_test_client_for(profile) {
        Ok(Some(c)) => c,
        _ => {
            emit_cleanup_event(test, "fail", "could not create cleanup client");
            return Cleanup::Failed;
        }
    };

    let max_attempts = 6;
    let retry_delay = Duration::from_secs(15);
    for attempt in 1..=max_attempts {
        let mut config = match Config::from_yaml(yaml) {
            Ok(c) => c,
            Err(e) => {
                emit_cleanup_event(test, "fail", &format!("re-parse failed: {e}"));
                return Cleanup::Failed;
            }
        };
        match client.destroy(&mut config).await {
            Ok(result) if result.has_failures() => {
                let first = &result.failed[0];
                let err = first.error.as_deref().unwrap_or("unknown");
                emit_cleanup_event(test, "leaked", &format!("{} resource(s) failed; first: {}/{}: {}", result.failed.len(), first.key.kind, first.key.name, err));
                return Cleanup::Leaked;
            }
            Ok(result) if result.succeeded.is_empty() => {
                if attempt == max_attempts {
                    emit_cleanup_event(test, "leaked", &format!("destroy produced no operations after {max_attempts} attempts"));
                    return Cleanup::Leaked;
                }
                tracing::info!(target: LIVE_TARGET, test, phase = "cleanup", event = "retry", attempt, max_attempts, delay_ms = retry_delay.as_millis() as u64, "reconciler found nothing to delete; resources may still be provisioning");
                tokio::time::sleep(retry_delay).await;
            }
            Ok(result) => {
                emit_cleanup_event(test, "cleaned", &format!("{} resource(s) cleaned", result.succeeded.len()));
                return Cleanup::Cleaned;
            }
            Err(e) => {
                emit_cleanup_event(test, "fail", &format!("destroy error: {e}"));
                return Cleanup::Failed;
            }
        }
    }
    Cleanup::Leaked
}

fn emit_phase_start(test: &'static str, phase: &'static str) {
    tracing::info!(target: LIVE_TARGET, test, phase, event = "start", "");
}

fn emit_phase_ok(test: &'static str, phase: &'static str, dur: Duration, msg: &str) {
    let dur_ms = dur.as_millis() as u64;
    if msg.is_empty() {
        tracing::info!(target: LIVE_TARGET, test, phase, event = "ok", dur_ms, "");
    } else {
        tracing::info!(target: LIVE_TARGET, test, phase, event = "ok", dur_ms, "{msg}");
    }
}

fn emit_phase_fail(test: &'static str, phase: &'static str, dur: Duration, fields: &FailFields) {
    // Empty string / 0 mean "not present"; downstream consumers ignore them.
    tracing::error!(
        target: LIVE_TARGET,
        test, phase,
        event = "fail",
        dur_ms = dur.as_millis() as u64,
        trace_id = fields.trace_id.as_deref().unwrap_or(""),
        http_status = fields.http_status.unwrap_or(0),
        "{}", fields.msg,
    );
}

fn emit_skip(test: &'static str, profile: &'static str, reason: &str) {
    tracing::warn!(target: LIVE_TARGET, test, profile, phase = "setup", event = "skip", "{reason}");
}

fn emit_summary(test: &'static str, dur: Duration, cleanup: Cleanup, passed: bool, msg: &str) {
    let dur_ms = dur.as_millis() as u64;
    let event = if passed { "ok" } else { "fail" };
    let cleanup_s = cleanup.as_str();
    if passed {
        tracing::info!(target: LIVE_TARGET, test, phase = "summary", event, dur_ms, cleanup = cleanup_s, "");
    } else if msg.is_empty() {
        tracing::error!(target: LIVE_TARGET, test, phase = "summary", event, dur_ms, cleanup = cleanup_s, "test failed");
    } else {
        tracing::error!(target: LIVE_TARGET, test, phase = "summary", event, dur_ms, cleanup = cleanup_s, "{msg}");
    }
}

fn emit_cleanup_event(test: &'static str, outcome: &'static str, msg: &str) {
    tracing::warn!(target: LIVE_TARGET, test, phase = "cleanup", event = outcome, "{msg}");
}

// ---------------------------------------------------------------------------
// TestGuard — panic safety net. Normal failure cleanup is run inline by
// `LiveTest::run` (see `run_cleanup`); the guard's Drop fires only when the
// body panics or the harness otherwise fails to disarm.
// ---------------------------------------------------------------------------

pub struct TestGuard {
    yaml: String,
    armed: bool,
    profile_name: String,
    test_name: &'static str,
}

impl TestGuard {
    fn with_profile(yaml: String, profile_name: &str) -> Self {
        Self { yaml, armed: true, profile_name: profile_name.to_string(), test_name: "unknown" }
    }

    fn for_test(mut self, test_name: &'static str) -> Self {
        self.test_name = test_name;
        self
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TestGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }

        emit_cleanup_event(self.test_name, "start", "guard not disarmed; attempting destroy");
        eprintln!("[{}][cleanup] guard triggered, attempting destroy", self.test_name);

        let yaml = self.yaml.clone();
        let profile_name = self.profile_name.clone();
        let test_name = self.test_name;

        let thread = std::thread::spawn(move || {
            let rt = match tokio::runtime::Runtime::new() {
                Ok(rt) => rt,
                Err(e) => {
                    emit_cleanup_event(test_name, "fail", &format!("could not create tokio runtime: {e}"));
                    eprintln!("[{test_name}][cleanup] runtime error: {e}");
                    return;
                }
            };

            rt.block_on(async {
                let cleanup_client = || -> anyhow::Result<WxctlClient> {
                    let path = test_profiles_path()?;
                    let path_str = path.to_str().ok_or_else(|| anyhow::anyhow!("Invalid path"))?;
                    Ok(WxctlClient::new(&profile_name, Some(path_str))?)
                };
                let client = match cleanup_client() {
                    Ok(c) => c,
                    Err(e) => {
                        emit_cleanup_event(test_name, "fail", &format!("could not create client: {e}"));
                        eprintln!("[{test_name}][cleanup] client error: {e}");
                        return;
                    }
                };

                // Retry while reconciler reports "nothing to delete" — some watsonx.data list
                // endpoints exclude resources still in PROVISIONING.
                let max_attempts = 6;
                let retry_delay = Duration::from_secs(15);
                let mut outcome: anyhow::Result<wxctl_engine::ExecutionResults> = Err(anyhow::anyhow!("cleanup not attempted"));
                for attempt in 1..=max_attempts {
                    let mut config = match Config::from_yaml(&yaml) {
                        Ok(c) => c,
                        Err(e) => {
                            outcome = Err(anyhow::anyhow!("re-parse failed: {e}"));
                            break;
                        }
                    };
                    match client.destroy(&mut config).await {
                        Ok(result) => {
                            let produced_ops = !result.succeeded.is_empty() || result.has_failures();
                            if produced_ops || attempt == max_attempts {
                                outcome = Ok(result);
                                break;
                            }
                            tracing::info!(target: LIVE_TARGET, test = test_name, phase = "cleanup", event = "retry", attempt, max_attempts, delay_ms = retry_delay.as_millis() as u64, "reconciler found nothing to delete; resources may still be provisioning");
                            tokio::time::sleep(retry_delay).await;
                        }
                        Err(e) => {
                            outcome = Err(e.into());
                            break;
                        }
                    }
                }

                match outcome {
                    Ok(result) if result.has_failures() => {
                        let first = &result.failed[0];
                        let err = first.error.as_deref().unwrap_or("unknown");
                        emit_cleanup_event(test_name, "leaked", &format!("{} resource(s) failed; first: {}/{}: {}", result.failed.len(), first.key.kind, first.key.name, err));
                        eprintln!("[{test_name}][cleanup] LEAKED — {} failed (first: {}/{}: {})", result.failed.len(), first.key.kind, first.key.name, err);
                    }
                    Ok(result) if result.succeeded.is_empty() => {
                        emit_cleanup_event(test_name, "leaked", &format!("destroy produced no operations after {max_attempts} attempts"));
                        eprintln!("[{test_name}][cleanup] LEAKED — destroy produced no operations");
                    }
                    Err(e) => {
                        emit_cleanup_event(test_name, "fail", &format!("destroy error: {e}"));
                        eprintln!("[{test_name}][cleanup] FAIL — {e}");
                    }
                    Ok(result) => {
                        emit_cleanup_event(test_name, "cleaned", &format!("{} resource(s) cleaned", result.succeeded.len()));
                        eprintln!("[{test_name}][cleanup] OK — {} cleaned", result.succeeded.len());
                    }
                }
            });
        });

        if thread.join().is_err() {
            emit_cleanup_event(self.test_name, "fail", "cleanup thread panicked");
            eprintln!("[{}][cleanup] PANIC — cleanup thread panicked", self.test_name);
        }
    }
}

#[path = "live/agent.rs"]
mod agent;
#[path = "live/collaborator_chain.rs"]
mod collaborator_chain;
#[path = "live/common_core_connection.rs"]
mod common_core_connection;
#[path = "live/connected_tool.rs"]
mod connected_tool;
#[path = "live/connection_basic_auth.rs"]
mod connection_basic_auth;
#[path = "live/connection_oauth2_client_creds.rs"]
mod connection_oauth2_client_creds;
#[path = "live/convergence.rs"]
mod convergence;
#[path = "live/cpd_common_core.rs"]
mod cpd_common_core;
#[path = "live/cpd_watsonx_data_core.rs"]
mod cpd_watsonx_data_core;
#[path = "live/cpd_watsonx_data_v3.rs"]
mod cpd_watsonx_data_v3;
#[path = "live/database_registration.rs"]
mod database_registration;
#[path = "live/dependency_chain.rs"]
mod dependency_chain;
#[path = "live/destroy_order.rs"]
mod destroy_order;
#[path = "live/empty_config.rs"]
mod empty_config;
#[path = "live/from_id_ref.rs"]
mod from_id_ref;
#[path = "live/ingestion_chain.rs"]
mod ingestion_chain;
#[path = "live/ingestion_job.rs"]
mod ingestion_job;
#[path = "live/kb_and_tool.rs"]
mod kb_and_tool;
#[path = "live/mcp_toolkit.rs"]
mod mcp_toolkit;
#[path = "live/mcp_toolkit_agent.rs"]
mod mcp_toolkit_agent;
#[path = "live/mcp_toolkit_node_invocation.rs"]
mod mcp_toolkit_node_invocation;
#[path = "live/mcp_toolkit_python_invocation.rs"]
mod mcp_toolkit_python_invocation;
#[path = "live/mcp_toolkit_remote_sse.rs"]
mod mcp_toolkit_remote_sse;
#[path = "live/mcp_toolkit_streamable_http.rs"]
mod mcp_toolkit_streamable_http;
#[path = "live/cloud_object_storage/on_destroy_retain_roundtrip.rs"]
mod on_destroy_retain_roundtrip;
#[path = "live/openapi_tool.rs"]
mod openapi_tool;
#[path = "live/openapi_tool_invocation.rs"]
mod openapi_tool_invocation;
#[path = "live/parallel_test.rs"]
mod parallel_test;
#[path = "live/partial_failure.rs"]
mod partial_failure;
#[path = "live/plan_ops.rs"]
mod plan_ops;
#[path = "live/presto_engine.rs"]
mod presto_engine;
#[path = "live/registration_env_and_warn.rs"]
mod registration_env_and_warn;
#[path = "live/cloud_object_storage/s3_bucket_lifecycle_hmac.rs"]
mod s3_bucket_lifecycle_hmac;
#[path = "live/cloud_object_storage/s3_bucket_lifecycle_iam.rs"]
mod s3_bucket_lifecycle_iam;
#[path = "live/cloud_object_storage/s3_object_lifecycle_inline.rs"]
mod s3_object_lifecycle_inline;
#[path = "live/schema.rs"]
mod schema;
#[path = "live/simple_chain.rs"]
mod simple_chain;
#[path = "live/spark_engine.rs"]
mod spark_engine;
#[path = "live/storage_registration.rs"]
mod storage_registration;
#[path = "live/storage_registration_engine_ref.rs"]
mod storage_registration_engine_ref;
#[path = "live/tool.rs"]
mod tool;
#[path = "live/validation.rs"]
mod validation;
#[path = "live/wml_ai_service_invocation.rs"]
mod wml_ai_service_invocation;
#[path = "live/wml_ai_service_source_change.rs"]
mod wml_ai_service_source_change;
#[path = "live/wml_chain.rs"]
mod wml_chain;
#[path = "live/wml_function_chain.rs"]
mod wml_function_chain;
#[path = "live/wml_function_invocation.rs"]
mod wml_function_invocation;
#[path = "live/wml_function_openapi_invocation.rs"]
mod wml_function_openapi_invocation;
#[path = "live/wml_function_source_change.rs"]
mod wml_function_source_change;
#[path = "live/wml_package_extension_e2e.rs"]
mod wml_package_extension_e2e;
#[path = "live/wml_script_chain.rs"]
mod wml_script_chain;
#[path = "live/wml_script_invocation.rs"]
mod wml_script_invocation;
#[path = "live/wml_script_source_change.rs"]
mod wml_script_source_change;
#[path = "live/wxd_cleanup_orphans.rs"]
mod wxd_cleanup_orphans;
#[path = "live/wxd_list_engines.rs"]
mod wxd_list_engines;
