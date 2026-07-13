//! Reference parsing and extraction utilities.

use crate::types::{IStr, ResourceKey, istr};

/// Parsed reference with optional field path.
///
/// Format: `${kind.name[.field.subfield]}` with optional field path
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedReference {
    /// The resource key (kind + name).
    pub key: ResourceKey,
    /// Optional field path for nested access (e.g., ["metadata", "id"]).
    pub field_path: Vec<IStr>,
}

impl ParsedReference {
    /// Create a new ParsedReference with field path.
    #[inline]
    pub fn with_path(key: ResourceKey, field_path: Vec<IStr>) -> Self {
        Self { key, field_path }
    }
}

/// Parse `${kind.name[.field.path]}` into ParsedReference.
///
/// Returns the resource key and optional field path for nested access.
///
/// # Examples
/// - `${catalog.my-catalog}` → key=(catalog, my-catalog), field_path=[]
/// - `${connection.db.metadata.id}` → key=(connection, db), field_path=["metadata", "id"]
#[must_use]
#[inline]
pub fn parse_reference_with_path(s: &str) -> Option<ParsedReference> {
    let inner = s.strip_prefix("${")?.strip_suffix("}")?;

    // Parse format: kind.name[.field.subfield]
    let parts: Vec<&str> = inner.split('.').collect();
    if parts.len() < 2 || parts[0].is_empty() || parts[1].is_empty() {
        return None;
    }

    let key = ResourceKey::new(parts[0], parts[1]);
    let field_path: Vec<IStr> = parts[2..].iter().map(|s| istr(*s)).collect();

    Some(ParsedReference::with_path(key, field_path))
}

/// Parse `${kind.name}` reference string into ResourceKey.
///
/// This is the simple form that ignores field paths. Use `parse_reference_with_path`
/// when you need field path information.
#[must_use]
#[inline]
pub fn parse_reference(s: &str) -> Option<ResourceKey> {
    parse_reference_with_path(s).map(|r| r.key)
}

/// Extract all `${...}` reference strings from a JSON value using a callback.
///
/// Calls the collector function with (reference_string, field_path) for each found reference.
#[inline]
pub fn extract_references_with_path<F>(value: &serde_json::Value, field_path: &str, collector: &mut F)
where
    F: FnMut(&str, &str),
{
    let mut path_buf = String::from(field_path);
    extract_refs_recursive(value, &mut path_buf, collector);
}

/// Internal recursive extraction using a mutable path buffer.
fn extract_refs_recursive<F>(value: &serde_json::Value, path: &mut String, collector: &mut F)
where
    F: FnMut(&str, &str),
{
    match value {
        serde_json::Value::String(s) if s.starts_with("${") && s.ends_with("}") => {
            collector(s, path);
        }
        serde_json::Value::Object(map) => {
            for (key, val) in map {
                let start_len = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(key);
                extract_refs_recursive(val, path, collector);
                path.truncate(start_len);
            }
        }
        serde_json::Value::Array(arr) => {
            for (i, item) in arr.iter().enumerate() {
                let start_len = path.len();
                use std::fmt::Write;
                write!(path, "[{}]", i).unwrap();
                extract_refs_recursive(item, path, collector);
                path.truncate(start_len);
            }
        }
        _ => {}
    }
}

/// Extract all `${...}` reference strings from a JSON value (simple callback).
#[inline]
pub fn extract_references<F>(value: &serde_json::Value, collector: &mut F)
where
    F: FnMut(&str),
{
    extract_references_with_path(value, "", &mut |ref_str, _| collector(ref_str));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_reference_with_path() {
        // (input, Some((kind, name, field_path)) | None)
        type Parsed = Option<(&'static str, &'static str, Vec<&'static str>)>;
        let cases: &[(&str, Parsed)] = &[
            ("${catalog.my-cat}", Some(("catalog", "my-cat", vec![]))),                           // simple kind.name
            ("${connection.db.metadata.id}", Some(("connection", "db", vec!["metadata", "id"]))), // trailing field path
            ("${invalid}", None),                                                                 // single part → no kind.name
            ("${}", None),                                                                        // empty body
            ("plain string", None),                                                               // not a reference
            ("${incomplete", None),                                                               // unterminated
        ];
        for (input, expected) in cases {
            match (parse_reference_with_path(input), expected) {
                (Some(parsed), Some((kind, name, path))) => {
                    assert_eq!(&*parsed.key.kind, *kind, "kind for {input}");
                    assert_eq!(&*parsed.key.name, *name, "name for {input}");
                    assert_eq!(parsed.field_path.len(), path.len(), "field_path len for {input}");
                    for (got, want) in parsed.field_path.iter().zip(path) {
                        assert_eq!(&**got, *want, "field_path segment for {input}");
                    }
                }
                (None, None) => {}
                (got, _) => panic!("parse mismatch for {input}: got {:?}", got.is_some()),
            }
        }
    }

    #[test]
    fn test_extract_references_collects_nested_with_and_without_paths() {
        let json = serde_json::json!({
            "catalog_id": "${catalog.my-cat}",
            "name": "plain",
            "nested": {
                "connection_id": "${connection.db}"
            },
            "items": ["${asset.foo}", "not-a-ref"]
        });

        // Plain extract: refs only, skips non-ref strings.
        let mut refs = Vec::new();
        extract_references(&json, &mut |r| refs.push(r.to_string()));
        assert_eq!(refs.len(), 3);
        assert!(refs.contains(&"${catalog.my-cat}".to_string()));
        assert!(refs.contains(&"${connection.db}".to_string()));
        assert!(refs.contains(&"${asset.foo}".to_string()));

        // With-path variant: same refs, each carrying its dotted/indexed JSON path.
        let mut found: Vec<(String, String)> = Vec::new();
        extract_references_with_path(&json, "", &mut |ref_str, path| {
            found.push((ref_str.to_string(), path.to_string()));
        });
        assert_eq!(found.len(), 3);
        assert!(found.iter().any(|(r, p)| r == "${catalog.my-cat}" && p == "catalog_id"));
        assert!(found.iter().any(|(r, p)| r == "${connection.db}" && p == "nested.connection_id"));
        assert!(found.iter().any(|(r, p)| r == "${asset.foo}" && p == "items[0]"));
    }
}
