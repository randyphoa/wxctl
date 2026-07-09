//! Snapshot suite for the panel renderer. Each test runs the built `wxctl`
//! binary with a fixed terminal width / color / glyph env and snapshots stdout
//! via `insta`. Redactions strip non-deterministic substrings (durations,
//! run ids) so snapshots stay stable — inactive for these static screens, they
//! activate when the pipeline screens land (Phases 3–4). Run `cargo insta
//! review` to accept changes.
//!
//! `WXCTL_COLOR` accepted values: `never` (plain/no color), `dark`, `light`,
//! `always` (alias for dark), `auto`. `never` is used here to keep snapshots
//! free of ANSI escape sequences.

use std::process::Command;

/// Run `wxctl <args>` with a fixed render env and return combined stdout.
/// `color` is `WXCTL_COLOR` (`dark`|`never`); `width` sets `WXCTL_WIDTH`;
/// `glyphs` sets `WXCTL_GLYPHS` (`ascii`|`unicode`, empty = default).
fn run(args: &[&str], width: &str, color: &str, glyphs: &str) -> String {
    let bin = env!("CARGO_BIN_EXE_wxctl");
    let mut cmd = Command::new(bin);
    cmd.args(args).env("WXCTL_WIDTH", width).env("WXCTL_COLOR", color).env_remove("NO_COLOR");
    if glyphs.is_empty() {
        cmd.env_remove("WXCTL_GLYPHS");
    } else {
        cmd.env("WXCTL_GLYPHS", glyphs);
    }
    let out = cmd.output().expect("wxctl binary runs");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn resources_plain_width_80() {
    let out = run(&["resources"], "80", "never", "");
    insta::assert_snapshot!("resources_plain_80", out);
}

// ── resources: width adaptation (dark) ──
// WXCTL_GLYPHS=unicode is forced here because stdout is piped in tests →
// vt_enabled()=false → GlyphSet::resolve() would fall back to ascii glyphs
// unless the env override is set explicitly.
#[test]
fn resources_dark_width_60() {
    insta::assert_snapshot!("resources_dark_60", run(&["resources"], "60", "dark", "unicode"));
}

#[test]
fn resources_dark_width_80() {
    insta::assert_snapshot!("resources_dark_80", run(&["resources"], "80", "dark", "unicode"));
}

#[test]
fn resources_dark_width_120() {
    insta::assert_snapshot!("resources_dark_120", run(&["resources"], "120", "dark", "unicode"));
}

// ── resources: ascii fallback (AC16) ──
#[test]
fn resources_ascii_width_80() {
    insta::assert_snapshot!("resources_ascii_80", run(&["resources"], "80", "dark", "ascii"));
}

// ── explain agent: dark render ──
// WXCTL_GLYPHS=unicode forced for the same reason as the resources dark matrix above.
// Only one width is snapshotted: `explain` output is width-independent (it never
// word-wraps), so 60/80/120 produced byte-identical bodies — the resources matrix
// above already covers the width-adaptation (wrapping) path.
#[test]
fn explain_agent_dark_width_80() {
    insta::assert_snapshot!("explain_agent_dark_80", run(&["explain", "agent"], "80", "dark", "unicode"));
}

// ── explain: plain (piped) parity baseline ──
#[test]
fn explain_agent_plain_width_80() {
    insta::assert_snapshot!("explain_agent_plain_80", run(&["explain", "agent"], "80", "never", ""));
}

// ── resources --service openscale: OpenScale Title-Case heading ──
// `--service openscale` filters to openscale kinds; the product heading must
// read "OpenScale" (display name added in Task 3.1).
// WXCTL_GLYPHS=unicode forced — piped stdout, same reason as dark matrix above.
#[test]
fn resources_openscale_filter_dark_80() {
    insta::assert_snapshot!("resources_openscale_dark_80", run(&["resources", "--service", "openscale"], "80", "dark", "unicode"));
}
