//! Final-phase E2E (compose-scaffold-incwd spec):
//! AC5 — the CLI `wxctl compose scaffold --output-dir <dir>` is unchanged: it flattens
//!        by filename into <dir> (legacy rebase behavior), independent of the MCP in-cwd
//!        default. Drives the freshly-built binary.

use std::process::Command;

const WXCTL_BIN: &str = env!("CARGO_BIN_EXE_wxctl");

#[test]
fn ac5_cli_scaffold_output_dir_flattens_by_filename() {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.yaml");
    // source_path with a leading dir; --output-dir must flatten to <out>/score.py.
    std::fs::write(&config, "kind: wml_function\nref_name: f\nsource_path: nested/deep/score.py\n").unwrap();
    let out_dir = tmp.path().join("out");

    // CLI clap: config via `-f`/`--filename`, output dir via `-o`/`--output-dir`.
    let out = Command::new(WXCTL_BIN).args(["compose", "scaffold", "-f", config.to_str().unwrap(), "-o", out_dir.to_str().unwrap()]).output().expect("spawn wxctl compose scaffold");
    assert!(out.status.success(), "stdout: {}\nstderr: {}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    // Flattened: <out>/score.py exists; the nested/ prefix is dropped.
    assert!(out_dir.join("score.py").exists(), "AC5: flattened file in output dir");
    assert!(!out_dir.join("nested").exists(), "AC5: source_path dir prefix is dropped");
}
