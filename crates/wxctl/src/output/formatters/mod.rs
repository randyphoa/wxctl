pub mod dependency;
pub mod error;
pub mod stage;
pub mod summary;

use crate::output::color::Theme;
use wxctl_core::logging::*;

/// Format a dependency event (suppressed in new format)
pub fn format_dependency_event(theme: &Theme, event: &DependencyEvent) -> String {
    dependency::format(theme, event)
}

/// Format a compact one-line error stream marker
pub fn format_error_stream_line(theme: &Theme, event: &ErrorEvent) -> String {
    error::format_stream_line(theme, event)
}
