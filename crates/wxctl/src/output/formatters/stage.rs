use crate::output::color::{Color, Theme, format_duration};

/// Total line width for right-aligning duration
const LINE_WIDTH: usize = 70;

/// Format stage name for display
fn format_stage_name(stage: &str) -> String {
    match stage {
        "validation" => "Validation".to_string(),
        "reconciliation" => "Reconciliation".to_string(),
        "planning" => "Planning".to_string(),
        "execution" => "Execution".to_string(),
        _ => stage.to_string(),
    }
}

/// Format the spinner message text for an active stage (stage name only, dot handled by {spinner})
pub fn format_stage_spinner_msg(theme: &Theme, stage: &str) -> String {
    let name = format_stage_name(stage);
    theme.paint(Color::Blue, &format!("{}...", name))
}

/// Format sub-stage with indentation and timing
pub fn format_substage(theme: &Theme, name: &str, duration_ms: Option<u64>) -> String {
    let duration_str = duration_ms.map(format_duration).unwrap_or_default();

    if duration_str.is_empty() {
        format!("    {}", theme.paint(Color::Dim, name))
    } else {
        let content_len = 4 + name.len(); // 4 spaces indent + name
        let duration_len = duration_str.len();
        let padding = if content_len + duration_len + 1 < LINE_WIDTH { LINE_WIDTH - content_len - duration_len } else { 2 };
        format!("    {}{}{}", name, " ".repeat(padding), theme.paint(Color::Dim, &duration_str))
    }
}
