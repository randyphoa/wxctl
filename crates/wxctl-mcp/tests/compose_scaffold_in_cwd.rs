//! Final-phase E2E (compose-scaffold-incwd spec):
//! AC1 — compose_scaffold with a python-tool config and no output_dir returns a
//!        ScaffoldOutputDto whose `config` is non-empty, the tool's source_path is a
//!        cwd-relative path under .wxctl-scaffold/, and schema.yaml + <module>.py +
//!        requirements.txt exist at that path.
//! AC3 — compose_scaffold with an output_dir resolving outside cwd returns an error
//!        naming the working-directory constraint and writes nothing there.
//! compose_scaffold reads std::env::current_dir(), which is process-global, so both
//! tests change cwd under a shared lock to serialize.

use std::sync::Mutex;
use wxctl_mcp::compose_tools::{ComposeScaffoldInput, compose_scaffold};

static CWD_LOCK: Mutex<()> = Mutex::new(());

const PY_TOOL: &str = "kind: tool\nref_name: weather\nsource_path: ignored\ninput_schema:\n  type: object\n  properties:\n    city:\n      type: string\nbinding:\n  python:\n    function: weather:main\n";

#[test]
fn ac1_in_cwd_scaffold_returns_canonical_config_and_writes_files() {
    let _g = CWD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let prev = std::env::current_dir().unwrap();
    std::env::set_current_dir(tmp.path()).unwrap();
    let result = compose_scaffold(&ComposeScaffoldInput { config: PY_TOOL.to_string(), output_dir: None, dry_run: false });
    std::env::set_current_dir(&prev).unwrap();

    let out = result.expect("scaffold ok");
    assert!(!out.failed, "manifest: {}", out.manifest);
    assert!(!out.config.is_empty(), "AC1: returned config is non-empty");
    // The rewritten source_path is cwd-relative under .wxctl-scaffold/.
    assert!(out.config.contains("source_path: .wxctl-scaffold/weather"), "AC1: source_path under .wxctl-scaffold/: {}", out.config);
    let dir = tmp.path().join(".wxctl-scaffold/weather");
    assert!(dir.join("schema.yaml").exists(), "AC1: schema.yaml");
    assert!(dir.join("weather.py").exists(), "AC1: <module>.py");
    assert!(dir.join("requirements.txt").exists(), "AC1: requirements.txt");
}

#[test]
fn ac3_outside_cwd_output_dir_errors_and_writes_nothing() {
    let _g = CWD_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let cwd_tmp = tempfile::tempdir().unwrap();
    let prev = std::env::current_dir().unwrap();
    std::env::set_current_dir(cwd_tmp.path()).unwrap();
    let result = compose_scaffold(&ComposeScaffoldInput { config: PY_TOOL.to_string(), output_dir: Some("/tmp/outside".to_string()), dry_run: false });
    std::env::set_current_dir(&prev).unwrap();

    let err = result.expect_err("AC3: outside-cwd output_dir must error");
    assert!(err.contains("working directory"), "AC3: error names the cwd constraint: {err}");
    assert!(!std::path::Path::new("/tmp/outside/weather").exists(), "AC3: nothing written outside cwd");
}
