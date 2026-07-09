// Re-export ParsedReference from core as the primary type for template parsing.
// This eliminates the unnecessary ResourceRef wrapper.
pub use wxctl_core::{ParsedReference, extract_references as core_extract_refs, parse_reference_with_path};

/// Check if a string contains a template reference.
#[must_use]
#[inline]
pub fn is_template(s: &str) -> bool {
    s.starts_with("${") && s.ends_with("}")
}

/// Extract all template references from a JSON value.
///
/// Uses the unified reference extractor from `wxctl_core` and parses
/// each reference into a `ParsedReference` (which includes field path support).
#[must_use]
pub fn extract_references(value: &serde_json::Value) -> Vec<ParsedReference> {
    let mut refs = Vec::new();
    core_extract_refs(value, &mut |ref_str| {
        if let Some(r) = parse_reference_with_path(ref_str) {
            refs.push(r);
        }
    });
    refs
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wxctl_core::ResourceKey;

    #[test]
    fn extract_references_multiple() {
        let value = json!({
            "a": "${catalog.x}",
            "b": "${connection.y}",
        });
        let refs = extract_references(&value);
        assert_eq!(refs.len(), 2);

        let keys: Vec<_> = refs.iter().map(|r| &r.key).collect();
        assert!(keys.contains(&&ResourceKey::new("catalog", "x")));
        assert!(keys.contains(&&ResourceKey::new("connection", "y")));
    }

    #[test]
    fn extract_references_skips_malformed() {
        let value = json!({
            "good": "${catalog.x}",
            "bad": "${invalid}",
        });
        let refs = extract_references(&value);
        // "${invalid}" has only one segment after split('.'), so parse fails
        assert_eq!(refs.len(), 1);
        assert_eq!(refs[0].key, ResourceKey::new("catalog", "x"));
    }
}
