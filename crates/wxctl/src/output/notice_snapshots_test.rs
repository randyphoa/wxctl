//! Offline snapshot suite for the update/news notice. Builds fixed
//! `UpdateNotice` fixtures and snapshots `render_notice` across dark/light/plain
//! via `insta` — deterministic (fixed `Theme::new(mode)`, no OSC probe, no live
//! version: `current_version` is pinned). Run `cargo insta review` to accept.

use crate::output::color::{ColorMode, Theme};
use crate::output::notice::render_notice;
use crate::update::{ChangelogEntry, NewsItem, Severity, UpdateNotice};

fn news(id: &str, sev: Severity, title: &str, body: Option<&str>, url: Option<&str>) -> NewsItem {
    NewsItem { id: id.into(), severity: sev, title: title.into(), body: body.map(Into::into), url: url.map(Into::into), min_version: None, max_version: None, fixed_in: None, expires: None }
}

/// Update available + one security + one info item (security renders first).
fn full_notice() -> UpdateNotice {
    UpdateNotice {
        current_version: "0.1.0".into(),
        new_version: Some("0.2.0".into()),
        news: vec![news("welcome-2026-06", Severity::Info, "Welcome to wxctl", Some("Thanks for installing."), None), news("cve-2026-1", Severity::Security, "Security fix in 0.2.0", Some("Upgrade recommended."), Some("https://example.invalid/advisory"))],
        changelog: vec![],
    }
}

fn render(mode: ColorMode) -> String {
    render_notice(&Theme::new(mode), &full_notice()).join("\n")
}

#[test]
fn notice_full_dark_80() {
    insta::assert_snapshot!("notice_full_dark_80", render(ColorMode::Dark));
}

#[test]
fn notice_full_light_80() {
    insta::assert_snapshot!("notice_full_light_80", render(ColorMode::Light));
}

#[test]
fn notice_full_plain_80() {
    insta::assert_snapshot!("notice_full_plain_80", render(ColorMode::Plain));
}

// ── AC byte assertions over the rendered notice ──

/// AC 1: the update line names `current → latest`.
#[test]
fn update_line_names_current_and_latest() {
    let out = render(ColorMode::Plain);
    assert!(out.contains("0.1.0"), "current version present: {out}");
    assert!(out.contains("0.2.0"), "latest version present: {out}");
    assert!(out.contains('\u{2192}'), "the → arrow is present: {out}");
}

/// AC 3 (automatable label-distinctness): security and info carry distinct
/// labels; security is ordered first; in a colored theme the security label
/// carries the red accent and the info label does not.
#[test]
fn security_label_is_distinct_and_first() {
    // Plain: distinct text labels, security before info.
    let plain = render(ColorMode::Plain);
    let sec = plain.find("[security]").expect("security label present");
    let news = plain.find("[news]").expect("news label present");
    assert_ne!("[security]", "[news]");
    assert!(sec < news, "security item is ordered before info: {plain}");
    // Dark: the security line carries the red accent; the info line does not.
    let dark = render(ColorMode::Dark);
    let sec_line = dark.lines().find(|l| l.contains("Security fix")).expect("security line");
    let info_line = dark.lines().find(|l| l.contains("Welcome")).expect("info line");
    assert!(sec_line.contains("\u{1b}[38;2;248;81;73m"), "security label painted red (dark): {sec_line:?}");
    assert!(!info_line.contains("\u{1b}[38;2;248;81;73m"), "info label not red: {info_line:?}");
}

/// Plain mode emits zero ANSI (mirrors the plan/exec snapshot invariants).
#[test]
fn plain_notice_has_no_ansi() {
    assert!(!render(ColorMode::Plain).contains('\u{1b}'), "plain notice has no ANSI escape");
}

// ── changelog / "What's new" snapshot tests ──

/// Update available + a changelog block with >3 notes (exercises the
/// "run `wxctl update --notes` for all" truncation line). No news, so the
/// snapshot isolates the "What's new" rendering.
fn full_notice_with_changelog() -> UpdateNotice {
    UpdateNotice {
        current_version: "0.1.0".into(),
        new_version: Some("0.2.0".into()),
        news: vec![],
        changelog: vec![ChangelogEntry {
            version: "0.2.0".into(),
            notes: vec!["wxctl update: upgrade in place from the CLI".into(), "What's new block in the post-command notice".into(), "Disable update checks with WXCTL_NO_UPDATE_CHECK".into(), "Fourth note to trigger the truncation line".into()],
            date: Some("2026-06-28".into()),
        }],
    }
}

fn render_changelog(mode: ColorMode) -> String {
    render_notice(&Theme::new(mode), &full_notice_with_changelog()).join("\n")
}

#[test]
fn notice_full_changelog_dark_80() {
    insta::assert_snapshot!("notice_full_changelog_dark_80", render_changelog(ColorMode::Dark));
}

#[test]
fn notice_full_changelog_light_80() {
    insta::assert_snapshot!("notice_full_changelog_light_80", render_changelog(ColorMode::Light));
}

#[test]
fn notice_full_changelog_plain_80() {
    insta::assert_snapshot!("notice_full_changelog_plain_80", render_changelog(ColorMode::Plain));
}
