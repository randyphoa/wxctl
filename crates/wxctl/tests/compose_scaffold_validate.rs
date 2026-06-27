//! Phase 2 deliverable: `compose scaffold` produces every file a config
//! references so `wxctl validate` (incl. post_validate existence checks) passes
//! with zero manual file creation; `--dry-run` writes nothing.

use std::path::Path;
use std::process::Command;

/// Path to the freshly-built `wxctl` binary (Cargo injects this for integration tests).
const WXCTL_BIN: &str = env!("CARGO_BIN_EXE_wxctl");

fn run(args: &[&str]) -> std::process::Output {
    Command::new(WXCTL_BIN).args(args).output().expect("failed to spawn wxctl")
}

/// Scaffold then validate `config_yaml` written into `dir`; assert validate succeeds.
fn scaffold_then_validate(dir: &Path, config_yaml: &str) {
    let config_path = dir.join("config.yaml");
    std::fs::write(&config_path, config_yaml).unwrap();
    let cfg = config_path.to_str().unwrap();

    let scaffold = run(&["compose", "scaffold", "-f", cfg]);
    assert!(scaffold.status.success(), "scaffold failed:\nstdout: {}\nstderr: {}", String::from_utf8_lossy(&scaffold.stdout), String::from_utf8_lossy(&scaffold.stderr));

    let validate = run(&["validate", "-f", cfg]);
    assert!(validate.status.success(), "validate failed:\nstdout: {}\nstderr: {}", String::from_utf8_lossy(&validate.stdout), String::from_utf8_lossy(&validate.stderr));
}

#[test]
fn wml_example_scaffolds_and_validates() {
    let tmp = tempfile::tempdir().unwrap();
    // The config-template WML example (space → software_specification → wml_function → wml_deployment).
    let yaml = r#"kind: space
ref_name: scoring_space
name: scoring-space
type: wx
---
kind: software_specification
ref_name: scoring_swspec
name: scoring-swspec
base_software_specification: runtime-25.1-py3.12
space_id: ${space.scoring_space}
---
kind: wml_function
ref_name: scoring_function
name: scoring-function
description: Statistical scoring function
software_spec: ${software_specification.scoring_swspec}
space_id: ${space.scoring_space}
source_path: score.py
---
kind: wml_deployment
ref_name: scoring_deployment
name: scoring-deployment
description: Online endpoint
asset: ${wml_function.scoring_function}
space_id: ${space.scoring_space}
online: {}
"#;
    scaffold_then_validate(tmp.path(), yaml);
    assert!(tmp.path().join("score.py").exists());
}

#[test]
fn hr_chatbot_scaffolds_and_validates() {
    let tmp = tempfile::tempdir().unwrap();
    // Agent + knowledge_base + python tool.
    let yaml = r#"kind: tool
ref_name: lookup_employee
name: lookup_employee
description: Look up an employee record by name.
permission: read_only
input_schema:
  type: object
  properties:
    name:
      type: string
      description: Employee full name
  required:
    - name
source_path: ./resources/tool/lookup_employee
binding:
  python:
    function: lookup_employee:main
---
kind: knowledge_base
ref_name: hr_policies
name: hr_policies
description: HR policy documents.
documents:
  - path: ./resources/kb/leave_policy.md
  - path: ./resources/kb/benefits.txt
---
kind: agent
ref_name: hr_assistant
name: hr_assistant
display_name: HR Assistant
description: Answers HR questions.
instructions: Be concise and cite policy.
llm: groq/openai/gpt-oss-120b
style: default
tools:
  - ${tool.lookup_employee}
knowledge_base:
  - ${knowledge_base.hr_policies}
"#;
    scaffold_then_validate(tmp.path(), yaml);
    assert!(tmp.path().join("resources/tool/lookup_employee/lookup_employee.py").exists());
    assert!(tmp.path().join("resources/tool/lookup_employee/schema.yaml").exists());
    assert!(tmp.path().join("resources/kb/leave_policy.md").exists());
    assert!(tmp.path().join("resources/kb/benefits.txt").exists());
}

#[test]
fn dry_run_writes_nothing() {
    let tmp = tempfile::tempdir().unwrap();
    let yaml = "kind: wml_function\nref_name: f\nname: f\nsource_path: score.py\n";
    let config_path = tmp.path().join("config.yaml");
    std::fs::write(&config_path, yaml).unwrap();
    let out = run(&["compose", "scaffold", "-f", config_path.to_str().unwrap(), "--dry-run"]);
    assert!(out.status.success(), "dry-run scaffold failed: {}", String::from_utf8_lossy(&out.stderr));
    assert!(!tmp.path().join("score.py").exists(), "dry-run must not write any file");
    // Manifest is printed to stderr.
    assert!(String::from_utf8_lossy(&out.stderr).contains("would create"), "manifest should report would-create entries");
}
