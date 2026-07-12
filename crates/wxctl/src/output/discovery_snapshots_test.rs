//! Offline snapshot + byte-assertion suite for the discovery renderers. `explain`
//! uses the real (schema-derived, offline) `presto_engine` view — AC5's named
//! surface; verbs and `required` must carry no semantic red/green. `resources`
//! uses synthetic rows to render the full-description catalog deterministically.
//! Binds AC5, AC6, AC7.

use crate::commands::explain::render_table;
use crate::commands::resources::{ResourceRow, print_grouped};
use crate::output::panel::glyphs::GlyphSet;
use crate::output::panel::layout::Panel;
use crate::output::panel::theme::{ColorMode, Theme};

fn panel(width: usize, mode: ColorMode, glyphs: GlyphSet) -> Panel {
    Panel::new(Theme::new(mode), width, glyphs)
}

// Truecolor sequences that must NOT appear on the explain verb column / required cells.
const GREEN: &str = "38;2;63;185;80";
const RED: &str = "38;2;248;81;73";

fn explain_presto(p: &Panel) -> String {
    let view = wxctl_schema::explain_kind("presto_engine").expect("presto_engine schema present");
    render_table(p, &view).join("\n")
}

/// Two products, one kind carrying a long description — long enough that the
/// hanging-indent wrap produces multiple lines, exercising the multi-line body.
fn sample_rows() -> Vec<ResourceRow> {
    vec![
        ResourceRow {
            kind: "presto_engine".into(),
            product: "watsonx.data".into(),
            service: "watsonx_data".into(),
            deployment: "saas, software".into(),
            endpoint: "/v3/engines".into(),
            description: "Interactive federated SQL query engine for the lakehouse: queries across catalogs with autoscaling worker pods and per-tenant t-shirt sizing.".into(),
        },
        ResourceRow { kind: "s3_bucket".into(), product: "Cloud Object Storage".into(), service: "cloud_object_storage".into(), deployment: "saas".into(), endpoint: "/buckets".into(), description: "An object-storage bucket.".into() },
    ]
}

// ── snapshots ──

#[test]
fn explain_presto_dark_100() {
    insta::assert_snapshot!("explain_presto_dark_100", explain_presto(&panel(100, ColorMode::Dark, GlyphSet::Unicode)));
}

#[test]
fn explain_presto_plain_100() {
    insta::assert_snapshot!("explain_presto_plain_100", explain_presto(&panel(100, ColorMode::Plain, GlyphSet::Unicode)));
}

#[test]
fn explain_presto_ascii_100() {
    insta::assert_snapshot!("explain_presto_ascii_100", explain_presto(&panel(100, ColorMode::Plain, GlyphSet::Ascii)));
}

#[test]
fn resources_dark_80() {
    insta::assert_snapshot!("resources_dark_80", print_grouped(&sample_rows(), &panel(80, ColorMode::Dark, GlyphSet::Unicode)).join("\n"));
}

#[test]
fn resources_ascii_80() {
    insta::assert_snapshot!("resources_ascii_80", print_grouped(&sample_rows(), &panel(80, ColorMode::Plain, GlyphSet::Ascii)).join("\n"));
}

// ── byte assertions ──

/// AC5 — no semantic red/green in the `Endpoints`+`Fields` region of `explain presto_engine`
/// (verbs neutral; `required` bold-white, not green). Header deployment tag (above Endpoints)
/// may carry semantic color, so scope the check to the endpoints..dependencies region.
#[test]
fn ac5_explain_endpoints_and_fields_have_no_semantic_color() {
    let view = wxctl_schema::explain_kind("presto_engine").expect("presto_engine schema present");
    let lines = render_table(&panel(100, ColorMode::Dark, GlyphSet::Unicode), &view);
    let idx = |needle: &str| lines.iter().position(|l| l.contains(needle)).unwrap_or_else(|| panic!("section {needle} present"));
    let start = idx("Endpoints");
    let end = idx("Dependencies");
    let region = lines[start..end].join("\n");
    assert!(!region.contains(GREEN), "no green in endpoints/fields tables: {region}");
    assert!(!region.contains(RED), "no red in endpoints/fields tables: {region}");
}

/// I4/AC8 — the ascii explain screen is pure ASCII.
#[test]
fn i4_explain_ascii_is_pure_ascii() {
    let out = explain_presto(&panel(100, ColorMode::Plain, GlyphSet::Ascii));
    assert!(out.bytes().all(|b| b < 0x80), "ascii explain screen is pure ASCII: {out:?}");
}

/// AC7 — the catalog header reads `resource catalog`, not `watsonx resource catalog`.
#[test]
fn ac7_header_is_resource_catalog() {
    let out = print_grouped(&sample_rows(), &panel(80, ColorMode::Plain, GlyphSet::Unicode)).join("\n");
    assert!(out.contains("resource catalog"), "header present: {out}");
    assert!(!out.contains("watsonx resource catalog"), "old product-scoped header gone: {out}");
}

/// AC6 — the catalog renders full descriptions: a long description wraps onto
/// more than one body line, so there are more lines than kinds.
#[test]
fn ac6_full_descriptions_wrap_to_multiple_lines() {
    let rows = sample_rows();
    let out = print_grouped(&rows, &panel(80, ColorMode::Plain, GlyphSet::Unicode));
    let kind_lines = out.iter().filter(|l| l.contains("presto_engine") || l.contains("s3_bucket")).count();
    assert_eq!(kind_lines, rows.len(), "one lead line per kind: {out:?}");
    // The presto_engine description is long enough to wrap, so body lines > lead lines.
    let body_lines = out.iter().filter(|l| l.contains("query engine") || l.contains("worker pods") || l.contains("t-shirt")).count();
    assert!(body_lines > 1, "long description wraps onto continuation lines: {out:?}");
}

/// I4/AC8 — the ascii catalog is pure ASCII.
#[test]
fn i4_resources_ascii_is_pure_ascii() {
    let out = print_grouped(&sample_rows(), &panel(80, ColorMode::Plain, GlyphSet::Ascii)).join("\n");
    assert!(out.bytes().all(|b| b < 0x80), "ascii catalog is pure ASCII: {out:?}");
}
