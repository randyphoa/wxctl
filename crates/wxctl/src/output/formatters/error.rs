use crate::output::color::{Color, Theme};
use crate::output::panel::glyphs::{GlyphSet, glyph};
use wxctl_core::logging::ErrorEvent;

/// One-line compact stream marker shown the instant an error event arrives:
/// `✗ kind/name · CODE`. Full detail (message + fix) renders once more in the
/// final `▌ Errors` section — together they satisfy the single-render contract
/// (the message is never repeated on fast commands, and the error is never
/// falsely shown under a `✓`). The streaming line preserves progressive
/// feedback during long applies without duplicating the message. The `✗`/`·`
/// glyphs route through the glyph table so plain mode is pure ascii.
pub fn format_stream_line(theme: &Theme, glyphs: GlyphSet, event: &ErrorEvent) -> String {
    let head = match (&event.resource_type, &event.resource_name) {
        (Some(k), Some(n)) => format!("{}/{}", k, n),
        _ => format!("{} stage", event.stage),
    };
    let sep = theme.paint(Color::Dim, &format!(" {} ", glyph(glyphs, "dot")));
    format!("{} {}{}{}", theme.paint(Color::Red, glyph(glyphs, "cross")), head, sep, theme.paint(Color::Dim, &event.error_code))
}
