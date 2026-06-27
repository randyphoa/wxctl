use crate::output::color::Theme;
use wxctl_core::logging::DependencyEvent;

/// Format dependency event — suppressed in new hybrid output format.
/// Dependencies are tracked internally but not printed to avoid cluttering
/// the aligned column table. Can be shown via --verbose in future.
pub fn format(_theme: &Theme, _event: &DependencyEvent) -> String {
    String::new()
}
