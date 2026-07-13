//! Phase-1 deliverable: prove the CLI (`compose prompt --resources-dir`) and the local
//! stdio MCP (`compose_prompt` with inline `existing_resources`) emit byte-identical
//! config prompts for the same inputs. The CLI leg runs the built binary; the MCP leg
//! drives `wxctl mcp serve --read-only` as a child process over stdio (rmcp client).
//! Fails loudly with the first differing byte offset if the two surfaces diverge.

use rmcp::model::{CallToolRequestParams, ClientInfo};
use rmcp::serve_client;
use rmcp::transport::{ConfigureCommandExt, TokioChildProcess};
use serde_json::json;
use std::path::Path;
use std::process::Command;
use wxctl_compose_core::assemble_recipe;

/// The freshly-built `wxctl` binary (Cargo injects this for integration tests).
const WXCTL_BIN: &str = env!("CARGO_BIN_EXE_wxctl");

const INPUT: &str = "HR chatbot that answers leave-policy questions";
const PATHS_YAML: &str = "format: compose/v1\ndeployment: saas\npaths:\n  - name: minimal\n    recommended: true\n    resources:\n      - kind: agent\n      - kind: knowledge_base\n    edges: []\n";

/// CLI leg: write input/paths into `dir`, run `wxctl compose prompt`, return stdout.
/// `write_output` uses `print!` (not `println!`) so no trailing newline is appended
/// by the CLI — no stripping is needed here.
fn cli_prompt(dir: &Path, resources_dir: Option<&Path>) -> String {
    let input_path = dir.join("input.txt");
    let paths_path = dir.join("paths.yaml");
    std::fs::write(&input_path, INPUT).unwrap();
    std::fs::write(&paths_path, PATHS_YAML).unwrap();

    let mut args = vec!["compose".to_string(), "prompt".to_string(), "--input".to_string(), input_path.to_string_lossy().into_owned(), "--paths".to_string(), paths_path.to_string_lossy().into_owned()];
    if let Some(res) = resources_dir {
        args.push("--resources-dir".to_string());
        args.push(res.to_string_lossy().into_owned());
    }
    let out = Command::new(WXCTL_BIN).args(&args).output().expect("failed to spawn wxctl compose prompt");
    assert!(out.status.success(), "CLI compose prompt failed:\nstdout: {}\nstderr: {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).expect("CLI prompt is valid UTF-8")
}

/// MCP leg: spawn `wxctl mcp serve --read-only`, call `compose_prompt` with the inline
/// `existing_resources` block, return the `prompt` field.
async fn mcp_prompt(existing_resources: &str) -> String {
    let transport = TokioChildProcess::new(tokio::process::Command::new(WXCTL_BIN).configure(|c| {
        c.arg("mcp").arg("serve").arg("--read-only").arg("-p").arg("nonexistent-profile-for-parity");
    }))
    .expect("spawn wxctl mcp serve");
    let client = serve_client(ClientInfo::default(), transport).await.expect("connect mcp client");

    let args = json!({ "input": INPUT, "paths": PATHS_YAML, "existing_resources": existing_resources });
    let r = client.call_tool(CallToolRequestParams::new("compose_prompt").with_arguments(args.as_object().cloned().unwrap())).await.expect("call compose_prompt");
    assert_ne!(r.is_error, Some(true), "compose_prompt errored: {r:?}");
    let prompt = r.structured_content.as_ref().and_then(|v| v.get("prompt")).and_then(|v| v.as_str()).expect("prompt field").to_string();
    client.cancel().await.ok();
    prompt
}

/// Report the first differing byte offset + surrounding context (the spec's loud-diff requirement).
fn assert_byte_identical(cli: &str, mcp: &str, label: &str) {
    if cli == mcp {
        return;
    }
    let cb = cli.as_bytes();
    let mb = mcp.as_bytes();
    let at = cb.iter().zip(mb.iter()).position(|(a, b)| a != b).unwrap_or(cb.len().min(mb.len()));
    let lo = at.saturating_sub(40);
    let cli_ctx = String::from_utf8_lossy(&cb[lo..(at + 40).min(cb.len())]);
    let mcp_ctx = String::from_utf8_lossy(&mb[lo..(at + 40).min(mb.len())]);
    panic!("{label}: CLI and MCP prompts differ at byte {at} (cli len {}, mcp len {})\n  cli: …{cli_ctx}…\n  mcp: …{mcp_ctx}…", cli.len(), mcp.len());
}

#[tokio::test(flavor = "multi_thread")]
async fn config_prompt_parity_without_existing_resources() {
    let tmp = tempfile::tempdir().unwrap();
    let cli = cli_prompt(tmp.path(), None);
    let mcp = mcp_prompt("").await;
    assert_byte_identical(&cli, &mcp, "no existing resources");
}

#[tokio::test(flavor = "multi_thread")]
async fn config_prompt_parity_with_existing_resources() {
    let tmp = tempfile::tempdir().unwrap();
    // Seed a knowledge_base directory so --resources-dir discovers files.
    let kb = tmp.path().join("knowledge_base");
    std::fs::create_dir_all(&kb).unwrap();
    std::fs::write(kb.join("leave_policy.md"), "policy").unwrap();
    std::fs::write(kb.join("benefits.txt"), "benefits").unwrap();

    // CLI renders the block internally from --resources-dir.
    let cli = cli_prompt(tmp.path(), Some(tmp.path()));
    // MCP leg receives the SAME block the CLI rendered, via the shared library functions.
    let files = wxctl_compose::discover_existing_resources(Some(tmp.path().to_str().unwrap())).unwrap();
    let block = wxctl_compose::render_existing_resources(&files);
    assert!(!block.is_empty(), "fixture should produce a non-empty existing-resources block");
    let mcp = mcp_prompt(&block).await;
    assert_byte_identical(&cli, &mcp, "with existing resources");
}

/// `compose_start` has no CLI surface; its parity is local-MCP vs the deterministic core.
/// Asserts the stdio MCP output equals `assemble_recipe` (same single source), guarding
/// against the DTO drifting away from the recipe template.
#[tokio::test(flavor = "multi_thread")]
async fn compose_start_matches_core_recipe() {
    let use_case = "HR chatbot with employee handbook and database access";
    let transport = TokioChildProcess::new(tokio::process::Command::new(WXCTL_BIN).configure(|c| {
        c.arg("mcp").arg("serve").arg("--read-only").arg("-p").arg("nonexistent-profile-for-parity");
    }))
    .expect("spawn wxctl mcp serve");
    let client = serve_client(ClientInfo::default(), transport).await.expect("connect mcp client");
    let args = json!({ "use_case": use_case });
    let r = client.call_tool(CallToolRequestParams::new("compose_start").with_arguments(args.as_object().cloned().unwrap())).await.expect("call compose_start");
    assert_ne!(r.is_error, Some(true), "compose_start errored: {r:?}");
    let sc = r.structured_content.as_ref().expect("structured content");
    let recipe = sc.get("recipe").and_then(|v| v.as_array()).expect("recipe");
    let mcp_names: Vec<String> = recipe.iter().map(|s| s.get("name").and_then(|v| v.as_str()).unwrap_or_default().to_string()).collect();
    let mcp_tiers: Vec<String> = recipe.iter().map(|s| s.get("tier").and_then(|v| v.as_str()).unwrap_or_default().to_string()).collect();
    let mcp_identify = sc.get("identify_prompt").and_then(|v| v.as_str()).expect("identify_prompt").to_string();
    let mcp_max = sc.get("fix_loop").and_then(|v| v.get("max_iterations")).and_then(|v| v.as_u64()).expect("max_iterations");
    let mcp_clar = sc.get("clarification").and_then(|v| v.get("policy")).and_then(|v| v.as_str()).expect("clarification.policy").to_string();

    let core = assemble_recipe(use_case).expect("core recipe");
    let core_names: Vec<String> = core.steps.iter().map(|s| s.name.clone()).collect();
    let core_tiers: Vec<String> = core.steps.iter().map(|s| s.tier.clone()).collect();
    assert_eq!(mcp_names, core_names, "recipe step names diverged from the core");
    assert_eq!(mcp_tiers, core_tiers, "recipe step tiers diverged from the core");
    assert_eq!(mcp_identify, core.identify_prompt, "identify prompt diverged from the core");
    assert_eq!(mcp_max, core.fix_loop.max_iterations as u64);
    assert_eq!(mcp_clar, core.clarification.policy, "clarification policy diverged from the core");
    assert!(mcp_identify.contains(use_case), "identify prompt carries the use case");
    client.cancel().await.ok();
}
