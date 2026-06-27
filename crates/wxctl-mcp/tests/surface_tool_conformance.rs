//! Conformance guard (AC3/AC4/AC7, the wxctl-mcp + CLI legs): assert the local stdio
//! MCP registers the full config + deploy capability set when mutating, and drops the
//! deploy-tier tools under `--read-only`; assert the CLI exposes the deploy-tier
//! subcommands. Mutating a wxctl-mcp tool router or a CLI subcommand out of this contract
//! turns this test red (AC7).

use rmcp::model::ClientInfo;
use rmcp::{ServiceExt, serve_client};
use std::path::PathBuf;
use std::process::Command;
use wxctl_mcp::WxctlMcpServer;

/// Locate the wxctl binary: use CARGO_MANIFEST_DIR (wxctl/crates/wxctl-mcp/) and
/// navigate up two levels to the workspace root, then into target/debug/wxctl.
/// This is the standard pattern for integration tests in a workspace crate that
/// does not own the binary (CARGO_BIN_EXE_wxctl is only available in the wxctl crate's
/// own integration tests and crates that have [[bin]] wxctl in scope).
fn wxctl_bin() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    // manifest_dir = .../wxctl/crates/wxctl-mcp
    // workspace root = .../wxctl
    let workspace_root = manifest_dir.parent().expect("crates/").parent().expect("wxctl root/");
    workspace_root.join("target").join("debug").join("wxctl")
}

// Config-tier tools every end-to-end surface must expose (always present, both modes).
const CONFIG_TOOLS: &[&str] = &["compose_start", "compose_paths", "compose_prompt", "wxctl_validate"];

// Deploy-tier tools present on wxctl-mcp (mutating) but NOT under --read-only.
const DEPLOY_TOOLS: &[&str] = &["compose_scaffold", "wxctl_apply", "wxctl_destroy", "wxctl_test"];

async fn tool_names(read_only: bool) -> Vec<String> {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = WxctlMcpServer::new("nonexistent-profile-for-conformance", None, read_only, false);
    tokio::spawn(async move {
        let running = server.serve(server_io).await.expect("server serve");
        let _ = running.waiting().await;
    });
    let client = serve_client(ClientInfo::default(), client_io).await.expect("client serve");
    let tools = client.list_all_tools().await.expect("list tools");
    let names = tools.iter().map(|t| t.name.to_string()).collect();
    client.cancel().await.ok();
    names
}

#[tokio::test]
async fn wxctl_mcp_mutating_exposes_config_and_deploy() {
    let names = tool_names(false).await;
    for t in CONFIG_TOOLS {
        assert!(names.contains(&t.to_string()), "mutating wxctl-mcp missing config-tier tool {t}; got {names:?}");
    }
    for t in DEPLOY_TOOLS {
        assert!(names.contains(&t.to_string()), "mutating wxctl-mcp missing deploy-tier tool {t}; got {names:?}");
    }
    // wxctl_plan is a read-only deploy-preview tool in the base router — present in both modes.
    assert!(names.contains(&"wxctl_plan".to_string()), "mutating wxctl-mcp missing wxctl_plan; got {names:?}");
}

#[tokio::test]
async fn wxctl_mcp_read_only_drops_deploy_tier() {
    let names = tool_names(true).await;
    for t in CONFIG_TOOLS {
        assert!(names.contains(&t.to_string()), "read-only wxctl-mcp missing config-tier tool {t}; got {names:?}");
    }
    for t in DEPLOY_TOOLS {
        assert!(!names.contains(&t.to_string()), "read-only wxctl-mcp must NOT register deploy-tier tool {t}; got {names:?}");
    }
    // wxctl_plan stays present in read-only mode (it is in the base router, not the mutating router).
    assert!(names.contains(&"wxctl_plan".to_string()), "read-only wxctl-mcp missing wxctl_plan; got {names:?}");
}

#[test]
fn cli_exposes_deploy_subcommands() {
    // Introspect the CLI without running a command: `wxctl <cmd> --help` exits 0 iff the
    // subcommand is registered. `compose` is hidden but still routable (its scaffold child too).
    let bin = wxctl_bin();
    assert!(bin.exists(), "wxctl binary not found at {bin:?}; run `cargo build -p wxctl` first");

    for cmd in [&["validate", "--help"][..], &["plan", "--help"][..], &["apply", "--help"][..], &["destroy", "--help"][..], &["test", "--help"][..], &["compose", "scaffold", "--help"][..]] {
        let out = Command::new(&bin).args(cmd).output().expect("spawn wxctl --help");
        assert!(out.status.success(), "CLI subcommand {cmd:?} not registered (exit {:?}):\n{}", out.status.code(), String::from_utf8_lossy(&out.stderr));
    }
}
