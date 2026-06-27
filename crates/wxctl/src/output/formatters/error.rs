use crate::output::color::{Color, Theme};
use wxctl_core::logging::ErrorEvent;

/// One-line compact stream marker shown the instant an error event arrives:
/// `✗ kind/name · CODE · <first message line>`. Full detail renders once more in
/// the final `▌ Errors` section — together they satisfy the single-render
/// contract (the error is never rendered as a multi-line block twice, and never
/// falsely under a `✓`).
pub fn format_stream_line(theme: &Theme, event: &ErrorEvent) -> String {
    let head = match (&event.resource_type, &event.resource_name) {
        (Some(k), Some(n)) => format!("{}/{}", k, n),
        _ => format!("{} stage", event.stage),
    };
    let first = event.message.lines().next().unwrap_or("").trim();
    let sep = theme.paint(Color::Dim, " \u{00b7} ");
    format!("{} {}{}{}{}{}", theme.paint(Color::Red, "\u{2717}"), head, sep, theme.paint(Color::Dim, &event.error_code), sep, first)
}
