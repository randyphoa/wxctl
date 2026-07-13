//! Offline snapshot + byte-assertion suite for `wxctl runs list`. Synthetic
//! `RunSummary` fixtures render through `render_list` under a `▌ Runs` panel
//! section (Phase 3 chrome). Deterministic. Binds AC10.

use crate::commands::runs::render_list;
use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{ColorMode, Theme};
use wxctl_core::diagnose::RunSummary;

fn panel(width: usize, mode: ColorMode, glyphs: GlyphSet) -> Panel {
    Panel::new(Theme::new(mode), width, glyphs)
}

fn runs() -> Vec<RunSummary> {
    vec![
        RunSummary { run_id: "20260711-101500-apply-abc123".into(), command: "apply".into(), started: "2026-07-11 10:15:00".into(), outcome: "success".into(), error_count: 0 },
        RunSummary { run_id: "20260711-100000-plan-def456".into(), command: "plan".into(), started: "2026-07-11 10:00:00".into(), outcome: "failed".into(), error_count: 2 },
    ]
}

// ── snapshots ──

#[test]
fn runs_list_dark_80() {
    insta::assert_snapshot!("runs_list_dark_80", render_list(&panel(80, ColorMode::Dark, GlyphSet::Unicode), &runs()).join("\n"));
}

#[test]
fn runs_list_plain_80() {
    insta::assert_snapshot!("runs_list_plain_80", render_list(&panel(80, ColorMode::Plain, GlyphSet::Unicode), &runs()).join("\n"));
}

#[test]
fn runs_list_ascii_80() {
    insta::assert_snapshot!("runs_list_ascii_80", render_list(&panel(80, ColorMode::Plain, GlyphSet::Ascii), &runs()).join("\n"));
}

#[test]
fn runs_list_empty_plain_80() {
    insta::assert_snapshot!("runs_list_empty_plain_80", render_list(&panel(80, ColorMode::Plain, GlyphSet::Ascii), &[]).join("\n"));
}

// ── byte assertions ──

/// AC10 — the list renders under a `▌`-prefixed `Runs` section (unicode), which
/// degrades to `| Runs` in ascii; no ad-hoc `─` rule. Checked under
/// `ColorMode::Plain` (zero-ANSI) so the bar glyph and heading text form one
/// contiguous substring — under a live color mode, `Theme::paint` wraps the bar
/// and the heading in separate ANSI set/reset pairs (`\x1b[..m▌\x1b[0m \x1b[..mRuns\x1b[0m`),
/// so `"▌ Runs"` is not literally contiguous there (mirrors the zero-ANSI
/// byte-check pattern in `exec_snapshots_test.rs`).
#[test]
fn ac10_runs_under_panel_section() {
    let unicode = render_list(&panel(80, ColorMode::Plain, GlyphSet::Unicode), &runs()).join("\n");
    assert!(unicode.contains("\u{258c} Runs"), "▌ Runs section header (unicode): {unicode}");
    let ascii = render_list(&panel(80, ColorMode::Plain, GlyphSet::Ascii), &runs()).join("\n");
    assert!(ascii.contains("| Runs"), "| Runs section header (ascii): {ascii}");
    assert!(!ascii.contains("\u{2500}\u{2500}\u{2500}"), "no ad-hoc ─── rule: {ascii}");
}

/// I4/AC8 — the ascii runs list is pure ASCII.
#[test]
fn i4_runs_ascii_is_pure_ascii() {
    let out = render_list(&panel(80, ColorMode::Plain, GlyphSet::Ascii), &runs()).join("\n");
    assert!(out.bytes().all(|b| b < 0x80), "ascii runs list is pure ASCII: {out:?}");
}
