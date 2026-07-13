//! Offline snapshot + byte-assertion suite for the error surfaces: the top-level
//! early-error renderer (config-load / missing-profile) and the panel `▌ Errors`
//! block built from an `ErrorEvent` through the `fix_for` remediation choke point
//! (`error_event_to_block`), plus the compact `✗ kind/name · CODE` stream line.
//! Deterministic (fixed `Theme` + explicit `GlyphSet`, synthetic events). Binds AC1-AC4.
//! Run `cargo insta review` to accept.

use crate::output::collector::error_event_to_block;
use crate::output::formatters::error::format_stream_line;
use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{ColorMode, Theme};
use crate::output::panel_render::{render_errors, render_top_level_error};
use crate::output::sections::ErrorsSection;
use wxctl_core::logging::{ErrorEvent, ErrorEventBuilder, error_codes};

fn panel(width: usize, mode: ColorMode, glyphs: GlyphSet) -> Panel {
    Panel::new(Theme::new(mode), width, glyphs)
}

/// A missing-required-field validation event carrying the *engine-supplied*
/// generic fix — `error_event_to_block` must override it via `fix_for`.
fn v003_event() -> ErrorEvent {
    ErrorEventBuilder::new(error_codes::V003, "validation", "Missing required field: name").resource("agent", "churn_agent").field("name").fix("Fix the resource schema to match the expected format").build()
}

/// The validate schema-error screen: the compact stream line (Phase 1 trimmed to
/// `✗ kind/name · CODE`, message removed) followed by the single full `▌ Errors`
/// block built through the remediation choke point.
fn schema_error_screen(p: &Panel) -> String {
    let ev = v003_event();
    let mut lines = vec![format_stream_line(&p.theme, p.glyphs, &ev)];
    lines.extend(render_errors(p, &ErrorsSection { blocks: vec![error_event_to_block(&ev)] }));
    lines.join("\n")
}

const V301_MSG: &str = "WXCTL-V301: environment variable WATSONX_APIKEY referenced by ${env:WATSONX_APIKEY} is not set";
const PROFILE_MISSING_MSG: &str = "failed to load profile file /nonexistent/profiles.yaml: No such file or directory (os error 2)";

// ── snapshots ──

#[test]
fn top_level_v301_dark_80() {
    insta::assert_snapshot!("top_level_v301_dark_80", render_top_level_error(&panel(80, ColorMode::Dark, GlyphSet::Unicode), V301_MSG).join("\n"));
}

#[test]
fn top_level_v301_plain_80() {
    insta::assert_snapshot!("top_level_v301_plain_80", render_top_level_error(&panel(80, ColorMode::Plain, GlyphSet::Ascii), V301_MSG).join("\n"));
}

#[test]
fn top_level_profile_missing_plain_80() {
    insta::assert_snapshot!("top_level_profile_missing_plain_80", render_top_level_error(&panel(80, ColorMode::Plain, GlyphSet::Ascii), PROFILE_MISSING_MSG).join("\n"));
}

#[test]
fn schema_error_v003_dark_80() {
    insta::assert_snapshot!("schema_error_v003_dark_80", schema_error_screen(&panel(80, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn schema_error_v003_plain_80() {
    insta::assert_snapshot!("schema_error_v003_plain_80", schema_error_screen(&panel(80, ColorMode::Plain, GlyphSet::Unicode)));
}

#[test]
fn schema_error_v003_ascii_80() {
    insta::assert_snapshot!("schema_error_v003_ascii_80", schema_error_screen(&panel(80, ColorMode::Plain, GlyphSet::Ascii)));
}

// ── byte assertions ──

/// AC1 — the top-level V301 error renders the panel idiom (▌ Errors + code + fix), never a bare `Error:` line.
#[test]
fn ac1_top_level_v301_is_panel_not_bare() {
    let out = render_top_level_error(&panel(80, ColorMode::Dark, GlyphSet::Unicode), V301_MSG).join("\n");
    assert!(out.contains('\u{258c}'), "panel bar ▌ present: {out}");
    assert!(out.contains("WXCTL-V301"), "code present: {out}");
    assert!(out.contains("fix"), "fix line present: {out}");
    assert!(!out.lines().any(|l| l.starts_with("Error:")), "no bare `Error:` line: {out}");
}

/// AC4 — the V301 fix references setting the env var (not the generic fallback).
#[test]
fn ac4_v301_fix_references_env_var() {
    let out = render_top_level_error(&panel(80, ColorMode::Plain, GlyphSet::Ascii), V301_MSG).join("\n");
    assert!(out.contains("set the ${env:VAR} reference, or export it before running"), "actionable env fix: {out}");
}

/// AC4 — the V003 fix names the field, overriding the engine's generic message.
#[test]
fn ac4_v003_fix_names_the_field() {
    let block = error_event_to_block(&v003_event());
    assert_eq!(block.fix, "add `name:` to the resource (see `wxctl explain <kind>`)", "field-named fix");
    assert!(!block.fix.contains("Fix the resource schema to match the expected format"), "generic fix overridden");
}

/// AC3 — the failing message appears exactly once (in the `▌ Errors` block); the stream line carries no message.
#[test]
fn ac3_message_rendered_exactly_once() {
    let ev = v003_event();
    let stream = format_stream_line(&Theme::new(ColorMode::Plain), GlyphSet::Ascii, &ev);
    assert!(!stream.contains("Missing required field"), "stream line carries no message: {stream}");
    assert!(stream.contains("agent/churn_agent"), "stream line names the resource: {stream}");
    assert!(stream.contains("WXCTL-V003"), "stream line carries the code: {stream}");
    let screen = schema_error_screen(&panel(80, ColorMode::Plain, GlyphSet::Ascii));
    assert_eq!(screen.matches("Missing required field: name").count(), 1, "message rendered exactly once: {screen}");
}

/// I4/AC8 — the ascii schema-error screen is pure ASCII (validate plain-mode surface).
#[test]
fn i4_schema_error_ascii_is_pure_ascii() {
    let screen = schema_error_screen(&panel(80, ColorMode::Plain, GlyphSet::Ascii));
    assert!(screen.bytes().all(|b| b < 0x80), "ascii validate screen is pure ASCII: {screen:?}");
}
