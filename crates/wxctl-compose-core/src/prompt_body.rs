//! `extract_prompt_body` — pull the fenced prompt body out of a compose
//! markdown template (` ```\n …body… \n``` ` pattern). Moved verbatim from the bin
//! crate's `commands/common.rs` so the compose core and `validate` share one copy.

/// Extract the prompt body from a markdown template file.
///
/// Templates use the pattern: docs header → ``` fence → prompt body → ``` fence.
/// This returns only the content between the first ``` and the last ```.
pub fn extract_prompt_body(template: &str) -> &str {
    if let Some(start) = template.find("```\n") {
        let body_start = start + 4;
        if let Some(end) = template.rfind("\n```")
            && end > body_start
        {
            return &template[body_start..end];
        }
    }
    template
}
