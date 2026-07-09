//! Environment-variable interpolation for parsed YAML values.
//!
//! Substitutes `${env:VAR_NAME}` literals in string leaves of a
//! `serde_norway::Value` tree. Called from resource YAML loading (before
//! schema validation) and from profile JSON loading (before client
//! construction) so every downstream stage sees resolved values.
//!
//! Syntax: `${env:NAME}` where `NAME` matches `[A-Z_][A-Z0-9_]*`.
//! Missing or empty env var → `WXCTL-V301`; malformed expression → `WXCTL-V302`.
//! No defaults, no escape syntax, single-pass (resolved values are treated as
//! opaque — any `${env:...}` pattern inside a resolved value is left literal).
use anyhow::{Result, anyhow};
use serde_norway::Value;

use crate::logging::error_codes;

pub trait EnvReader {
    fn get(&self, var: &str) -> Option<String>;
}

pub struct ProcessEnv;
impl EnvReader for ProcessEnv {
    fn get(&self, var: &str) -> Option<String> {
        std::env::var(var).ok().filter(|v| !v.is_empty())
    }
}

/// Walk the YAML tree and substitute every `${env:VAR}` in string leaves.
pub fn interpolate(value: &mut Value, env: &dyn EnvReader) -> Result<()> {
    interpolate_at(value, env, "")
}

fn interpolate_at(value: &mut Value, env: &dyn EnvReader, path: &str) -> Result<()> {
    match value {
        Value::String(s) => {
            if let Some(replaced) = substitute(s, env, path)? {
                *s = replaced;
            }
            Ok(())
        }
        Value::Sequence(seq) => {
            for (i, item) in seq.iter_mut().enumerate() {
                let child_path = if path.is_empty() { format!("[{i}]") } else { format!("{path}[{i}]") };
                interpolate_at(item, env, &child_path)?;
            }
            Ok(())
        }
        Value::Mapping(map) => {
            for (k, v) in map.iter_mut() {
                let key_repr = k.as_str().unwrap_or("?");
                let child_path = if path.is_empty() { key_repr.to_string() } else { format!("{path}.{key_repr}") };
                interpolate_at(v, env, &child_path)?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Scan `s` for `${env:NAME}` occurrences; return `Some(replaced)` if any
/// substitution happened, `None` if `s` had no `${env:` prefix at all.
/// Single-pass: resolved values are NOT re-scanned.
fn substitute(s: &str, env: &dyn EnvReader, path: &str) -> Result<Option<String>> {
    if !s.contains("${env:") {
        return Ok(None);
    }

    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(idx) = rest.find("${env:") {
        out.push_str(&rest[..idx]);
        let after_prefix = &rest[idx + "${env:".len()..];
        let Some(end) = after_prefix.find('}') else {
            return Err(anyhow!("{}: malformed ${{env:...}} expression at field '{}': missing closing '}}'", error_codes::V302, display_path(path)));
        };
        let name = &after_prefix[..end];
        if !is_valid_env_name(name) {
            return Err(anyhow!("{}: malformed ${{env:...}} expression at field '{}': env var name '{}' must match [A-Z_][A-Z0-9_]*", error_codes::V302, display_path(path), name));
        }
        let Some(val) = env.get(name) else {
            return Err(anyhow!("{}: env var '{}' is unset or empty, required by field '{}'", error_codes::V301, name, display_path(path)));
        };
        out.push_str(&val);
        rest = &after_prefix[end + 1..];
    }
    out.push_str(rest);

    Ok(Some(out))
}

fn is_valid_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else { return false };
    if !(first.is_ascii_uppercase() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

fn display_path(path: &str) -> &str {
    if path.is_empty() { "<root>" } else { path }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    struct MapEnv(HashMap<String, String>);
    impl EnvReader for MapEnv {
        fn get(&self, var: &str) -> Option<String> {
            self.0.get(var).cloned().filter(|v| !v.is_empty())
        }
    }

    fn env(pairs: &[(&str, &str)]) -> MapEnv {
        MapEnv(pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
    }

    fn yaml(s: &str) -> Value {
        serde_norway::from_str(s).unwrap()
    }

    #[test]
    fn substitutes_string_leaves_across_shapes() {
        // (yaml, env, key-path-to-leaf, expected) covering: single hit, multiple
        // hits in one string, plain string untouched, literal `$` without the
        // `${env:` prefix passes through, non-ascii surrounding text preserved,
        // and single-pass (a resolved value containing `${env:BAR}` is NOT
        // re-scanned — callers treat resolved values as opaque data).
        let mut single = yaml("key: ${env:FOO}");
        interpolate(&mut single, &env(&[("FOO", "bar")])).unwrap();
        assert_eq!(single["key"].as_str().unwrap(), "bar", "single hit");

        let mut multi = yaml("url: https://${env:HOST}:${env:PORT}/api");
        interpolate(&mut multi, &env(&[("HOST", "example.com"), ("PORT", "443")])).unwrap();
        assert_eq!(multi["url"].as_str().unwrap(), "https://example.com:443/api", "multiple in one string");

        let mut plain = yaml("key: plain-value");
        interpolate(&mut plain, &env(&[])).unwrap();
        assert_eq!(plain["key"].as_str().unwrap(), "plain-value", "plain string untouched");

        let mut dollar = yaml(r#"key: "cost is $5""#);
        interpolate(&mut dollar, &env(&[])).unwrap();
        assert_eq!(dollar["key"].as_str().unwrap(), "cost is $5", "literal $ without env prefix");

        let mut non_ascii = yaml("note: café ${env:NAME} résumé");
        interpolate(&mut non_ascii, &env(&[("NAME", "Jörg")])).unwrap();
        assert_eq!(non_ascii["note"].as_str().unwrap(), "café Jörg résumé", "non-ascii surrounding text");

        // Single-pass: resolved value is opaque, inner ${env:BAR} left literal.
        let mut single_pass = yaml("key: ${env:FOO}");
        interpolate(&mut single_pass, &env(&[("FOO", "${env:BAR}"), ("BAR", "should-not-resolve")])).unwrap();
        assert_eq!(single_pass["key"].as_str().unwrap(), "${env:BAR}", "single-pass opaque");
    }

    #[test]
    fn substitutes_inside_nested_objects_and_arrays() {
        let mut v = yaml(
            r#"
outer:
  list:
    - ${env:A}
    - plain
  inner:
    key: ${env:B}
"#,
        );
        interpolate(&mut v, &env(&[("A", "a-val"), ("B", "b-val")])).unwrap();
        assert_eq!(v["outer"]["list"][0].as_str().unwrap(), "a-val");
        assert_eq!(v["outer"]["list"][1].as_str().unwrap(), "plain");
        assert_eq!(v["outer"]["inner"]["key"].as_str().unwrap(), "b-val");
    }

    #[test]
    fn malformed_or_unset_expressions_error_with_codes() {
        // V301 = env var unset/empty (both treated identically); V302 = malformed
        // expression (lowercase name fails the [A-Z_][A-Z0-9_]* rule; missing
        // closing brace). The first case also asserts the field path is reported.
        let mut missing = yaml("outer:\n  nested: ${env:MISSING}");
        let err = interpolate(&mut missing, &env(&[])).unwrap_err().to_string();
        assert!(err.contains("WXCTL-V301"), "missing var: {err}");
        assert!(err.contains("MISSING"), "missing var name: {err}");
        assert!(err.contains("outer.nested"), "missing field path: {err}");

        let mut empty = yaml("key: ${env:EMPTY}");
        let err = interpolate(&mut empty, &env(&[("EMPTY", "")])).unwrap_err().to_string();
        assert!(err.contains("WXCTL-V301"), "empty var treated as missing: {err}");

        let mut lower = yaml("key: ${env:lowercase}");
        let err = interpolate(&mut lower, &env(&[("lowercase", "x")])).unwrap_err().to_string();
        assert!(err.contains("WXCTL-V302"), "lowercase name: {err}");

        let mut no_brace = yaml(r#"key: "${env:FOO""#);
        let err = interpolate(&mut no_brace, &env(&[("FOO", "x")])).unwrap_err().to_string();
        assert!(err.contains("WXCTL-V302"), "missing closing brace: {err}");
    }

    #[test]
    fn non_string_leaves_untouched() {
        let mut v = yaml("num: 42\nflag: true\nnil: null");
        interpolate(&mut v, &env(&[])).unwrap();
        assert_eq!(v["num"].as_i64().unwrap(), 42);
        assert!(v["flag"].as_bool().unwrap());
        assert!(v["nil"].is_null());
    }
}
