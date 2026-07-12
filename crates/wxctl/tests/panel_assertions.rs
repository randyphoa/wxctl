//! Byte/ANSI assertions backing the panel ACs (palette, ascii-only, zero-ANSI
//! plain). Runs the built `wxctl` binary with a fixed render env and inspects
//! raw stdout bytes — distinct from the `insta` snapshot suite.

use std::process::Command;

/// Run `wxctl <args>` with a fixed render env and return stdout.
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

/// Byte/ANSI invariants over the *real binary's* stdout (the integration leg the
/// offline `panel_render` snapshots can't cover), folded into one render-and-check:
///
/// - AC4: the dark theme carries the corrected accent — blue-40 `#78A9FF` for
///   section bars/headings and green `#3FB950` for the stage/required check color.
/// - AC16: `WXCTL_GLYPHS=ascii` output is pure ASCII. Asserted on `explain <kind>`,
///   whose output is schema field names + layout glyphs (all ASCII-safe); `resources`
///   carries non-ASCII em-dashes in schema *descriptions* — content, not glyphs, so
///   outside the scope of WXCTL_GLYPHS=ascii.
/// - Plain/piped output is zero-ANSI: no ESC byte anywhere.
#[test]
fn panel_binary_byte_invariants() {
    // AC4 dark palette: blue from resources, green from explain.
    assert!(run(&["resources"], "80", "dark", "").contains("38;2;120;169;255"), "dark resources output contains #78A9FF blue");
    assert!(run(&["explain", "agent"], "80", "dark", "").contains("38;2;63;185;80"), "explain dark output contains #3FB950 green for required/check");

    // AC16 ascii-glyph (plain color so the only bytes are glyphs + text): pure ascii
    // across two kinds.
    for kind in ["agent", "space"] {
        let out = run(&["explain", kind], "80", "never", "ascii");
        assert!(out.is_ascii(), "ascii-glyph explain {kind} output must be pure ascii");
    }

    // Plain/piped output is zero-ANSI across both surfaces.
    assert!(!run(&["resources"], "80", "never", "").contains('\u{1b}'), "plain resources output has no ANSI escape");
    assert!(!run(&["explain", "agent"], "80", "never", "").contains('\u{1b}'), "plain explain output has no ANSI escape");
}
