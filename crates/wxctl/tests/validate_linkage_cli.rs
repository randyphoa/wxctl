//! Phase 4 CLI surface (spec 2026-07-11-validate-linkage-diagnostics): dangling-ref V005,
//! literal/unknown pass, chain suggestion, orphan V505 advisory (JSON + table), --deployment
//! flag, --fix-prompt. Spawns the built binary; no profile, no network.

use std::path::Path;
use std::process::Command;

const WXCTL_BIN: &str = env!("CARGO_BIN_EXE_wxctl");

fn write(dir: &Path, yaml: &str) -> String {
    let p = dir.join("config.yaml");
    std::fs::write(&p, yaml).unwrap();
    p.to_str().unwrap().to_string()
}

fn run(args: &[&str]) -> std::process::Output {
    Command::new(WXCTL_BIN).args(args).output().expect("spawn wxctl")
}

const AGENT_DANGLING_TOOL: &str = "kind: agent\nref_name: a1\nname: a1\ndisplay_name: A1\ndescription: d\ninstructions: i\nllm: groq/openai/gpt-oss-120b\nstyle: default\ntools:\n  - ${tool.missing_tool}\n";
const AGENT_LITERAL: &str = "kind: agent\nref_name: a2\nname: a2\ndisplay_name: A2\ndescription: d\ninstructions: i\nllm: groq/openai/gpt-oss-120b\nstyle: default\n";
const AGENT_UNKNOWN_KIND: &str = "kind: agent\nref_name: a3\nname: a3\ndisplay_name: A3\ndescription: refers to ${foo.bar} unknown kind\ninstructions: i\nllm: groq/openai/gpt-oss-120b\nstyle: default\n";
const MODEL_TRACKING_CHAIN: &str = "kind: model_tracking\nref_name: mt\nmodel: ${wml_model.absent}\nmodel_entry: entry-literal\nmodel_entry_catalog_id: cat123\nspace_id: space-literal\n";
const AGENT_ORPHAN_CCC: &str = "kind: agent\nref_name: a6\nname: a6\ndisplay_name: A6\ndescription: d\ninstructions: i\nllm: groq/openai/gpt-oss-120b\nstyle: default\n---\nkind: common_core_connection\nref_name: db6\nname: db6\ndatasource_type: postgres\nproperties:\n  host: h\n";
const CCC_DEPENDED_UPON: &str =
    "kind: common_core_connection\nref_name: db7\nname: db7\ndatasource_type: postgres\nproperties:\n  host: h\n---\nkind: agent\nref_name: a7\nname: a7\ndisplay_name: A7\ndescription: d\ninstructions: i\nllm: groq/openai/gpt-oss-120b\nstyle: default\ndepends_on:\n  - common_core_connection.db7\n";

#[test]
fn ac1_dangling_tool_ref_fails_v005_json() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), AGENT_DANGLING_TOOL);
    let out = run(&["validate", "-f", &cfg, "--output", "json"]);
    assert!(!out.status.success(), "dangling ref must exit non-zero");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"valid\": false"), "valid:false expected: {stdout}");
    assert!(stdout.contains("WXCTL-V005"), "V005 in message: {stdout}");
    assert!(stdout.contains("`tool` resource with `ref_name: missing_tool`"), "add-resource suggestion: {stdout}");
    assert!(stdout.contains("replace the reference with a literal value if the resource is managed outside it"), "literal alternative: {stdout}");
}

#[test]
fn ac2_literal_passes_no_warning() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), AGENT_LITERAL);
    let out = run(&["validate", "-f", &cfg, "--output", "json"]);
    assert!(out.status.success(), "literal config must validate: {}", String::from_utf8_lossy(&out.stderr));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"valid\": true"));
    assert!(!stdout.contains("\"warnings\""), "no warnings key on a clean valid config: {stdout}");
}

#[test]
fn ac3_unknown_kind_template_silent() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), AGENT_UNKNOWN_KIND);
    let out = run(&["validate", "-f", &cfg, "--output", "json"]);
    assert!(out.status.success(), "unknown-kind template must keep passing: {}", String::from_utf8_lossy(&out.stdout));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"valid\": true"));
    assert!(!stdout.contains("WXCTL-V005"));
}

#[test]
fn ac5_chain_completion_in_suggestion() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), MODEL_TRACKING_CHAIN);
    let out = run(&["validate", "-f", &cfg, "--output", "json"]);
    assert!(!out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("`wml_model` resource with `ref_name: absent`"), "root suggestion: {stdout}");
    assert!(stdout.contains("autoai_experiment") && stdout.contains("wml_model.experiment"), "chain hop 1: {stdout}");
    assert!(stdout.contains("data_asset") && stdout.contains("autoai_experiment.training_data"), "chain hop 2: {stdout}");
}

#[test]
fn ac6_orphan_ccc_advisory_json_and_table() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), AGENT_ORPHAN_CCC);
    let j = run(&["validate", "-f", &cfg, "--output", "json"]);
    assert!(j.status.success(), "advisories never change exit code");
    let js = String::from_utf8_lossy(&j.stdout);
    assert!(js.contains("\"valid\": true"));
    assert!(js.contains("WXCTL-V505") && js.contains("orchestrate_connection"), "V505 advisory in warnings: {js}");
    let t = run(&["validate", "-f", &cfg]);
    assert!(t.status.success());
    // The human panel (Advisories section) is diagnostics → stderr; stdout is
    // reserved for `--output json`.
    let ts = String::from_utf8_lossy(&t.stderr);
    assert!(ts.contains("Advisories") && ts.contains("WXCTL-V505"), "table advisories section: {ts}");
}

#[test]
fn ac7_referenced_ccc_no_advisory() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), CCC_DEPENDED_UPON);
    let out = run(&["validate", "-f", &cfg, "--output", "json"]);
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("\"valid\": true"));
    assert!(!stdout.contains("\"warnings\""), "non-orphan ccc must yield no advisory: {stdout}");
}

#[test]
fn ac8_deployment_flag_accepted_and_validated() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), AGENT_LITERAL);
    assert!(run(&["validate", "-f", &cfg, "--deployment", "saas"]).status.success(), "--deployment saas accepted");
    assert!(run(&["validate", "-f", &cfg, "--deployment", "software"]).status.success(), "--deployment software accepted");
    let bogus = run(&["validate", "-f", &cfg, "--deployment", "bogus"]);
    assert_eq!(bogus.status.code(), Some(2), "bad --deployment value is a clap usage error (exit 2)");
}

#[test]
fn ac10_fix_prompt_contains_rule_and_schema_ref() {
    let tmp = tempfile::tempdir().unwrap();
    let cfg = write(tmp.path(), AGENT_DANGLING_TOOL);
    let out = run(&["validate", "-f", &cfg, "--fix-prompt"]);
    assert!(out.status.success(), "--fix-prompt path exits 0 (the prompt is the product)");
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(s.contains("You MAY add a new resource document ONLY when an error's suggestion explicitly names a"), "loosened rule: {s}");
    assert!(s.contains("Schema Reference"), "schema-reference section header");
    assert!(s.contains("# tool"), "schema docs for the suggested `tool` kind");
}
