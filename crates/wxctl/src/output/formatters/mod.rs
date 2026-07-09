pub mod error;
pub mod stage;
pub mod summary;

use crate::output::color::Theme;
use wxctl_core::logging::*;

/// Format a compact one-line error stream marker
pub fn format_error_stream_line(theme: &Theme, event: &ErrorEvent) -> String {
    error::format_stream_line(theme, event)
}
