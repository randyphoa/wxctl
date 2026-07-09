//! Pure presentation for the update/news notice. Themed via the panel role
//! palette, ordered security-first, with distinct `info`/`security` labels.
//! Returns lines (no trailing newline) for `main.rs` to print to **stderr**.
//! Snapshot-tested in `notice_snapshots_test.rs`.

use crate::output::color::Theme;
use crate::output::panel::theme::{Role, paint_role};
use crate::update::{Severity, UpdateNotice};

/// Render the notice into themed lines. Empty when there is nothing to show.
pub fn render_notice(theme: &Theme, notice: &UpdateNotice) -> Vec<String> {
    let has_update = notice.new_version.is_some();
    let has_news = !notice.news.is_empty();
    let has_changelog = !notice.changelog.is_empty();
    let mut lines = Vec::new();
    if !has_update && !has_news && !has_changelog {
        return lines;
    }
    // One blank line separates the notice from the command's own output.
    lines.push(String::new());

    if let Some(latest) = &notice.new_version {
        lines.push(format!(
            "{} A new release of wxctl is available: {} {} {}",
            paint_role(theme, Role::Caution, "update"),
            notice.current_version,
            "\u{2192}", // →
            paint_role(theme, Role::Heading, latest),
        ));
        lines.push(paint_role(theme, Role::Meta, "  Run `wxctl update` to upgrade"));
    }

    // "What's new": the newest gainable release's notes (changelog is newest-first),
    // capped at 3 bullets; point to `wxctl update --notes` when there is more.
    if let Some(entry) = notice.changelog.first() {
        const MAX_NOTES: usize = 3;
        lines.push(paint_role(theme, Role::Heading, &format!("What's new in v{}", entry.version)));
        for note in entry.notes.iter().take(MAX_NOTES) {
            lines.push(paint_role(theme, Role::Meta, &format!("  \u{2022} {note}")));
        }
        if entry.notes.len() > MAX_NOTES || notice.changelog.len() > 1 {
            lines.push(paint_role(theme, Role::Meta, "  run `wxctl update --notes` for all"));
        }
    }

    // News, security-first (stable within each severity group).
    let ordered = notice.news.iter().filter(|n| n.severity == Severity::Security).chain(notice.news.iter().filter(|n| n.severity == Severity::Info));
    for item in ordered {
        let (label, role) = match item.severity {
            Severity::Security => ("[security]", Role::Danger),
            Severity::Info => ("[news]", Role::Active),
        };
        lines.push(format!("{} {}", paint_role(theme, role, label), item.title));
        if let Some(body) = &item.body {
            lines.push(paint_role(theme, Role::Meta, &format!("  {body}")));
        }
        if let Some(url) = &item.url {
            lines.push(paint_role(theme, Role::Meta, &format!("  {url}")));
        }
    }
    lines
}
